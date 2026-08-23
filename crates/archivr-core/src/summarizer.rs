//! Per-entry LLM summaries.
//!
//! A summary is a *regenerable child record* of an entry (`entry_summaries`),
//! not a column on `archived_entries` and not an artifact on disk: an entry can
//! carry several summaries (one per provider / model / prompt version), any of
//! them can be discarded and recomputed, and none of them is part of the
//! preserved capture. Generation is manual-only — nothing in `capture.rs` calls
//! into this module.
//!
//! Four providers sit behind one [`SummaryProvider`] trait: two HTTP APIs and
//! two local CLIs. They are configured by environment variable, never by TOML,
//! matching how the rest of the tree resolves external tools (`ARCHIVR_YT_DLP`,
//! `ARCHIVR_SINGLE_FILE`, `ARCHIVR_TWEET_SCRAPER`, …) and keeping API keys out
//! of any file the archive would otherwise persist.

use anyhow::{Context, Result, anyhow, bail};
use std::{
    env,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

use crate::{archive::ArchivePaths, database, hash};

/// Bump whenever the prompt text below changes in a way that would produce a
/// materially different summary. It is part of the `entry_summaries` cache key,
/// so a bump makes every stored summary regenerate on next request instead of
/// silently mixing outputs from two different prompts.
pub const PROMPT_VERSION: &str = "v1-2026-08-22";

/// Upper bound on characters fed to a model. Archived pages run to hundreds of
/// kilobytes; past this point we are paying for tokens that do not change a
/// five-sentence summary. Truncation happens *before* hashing so the cache key
/// describes exactly what the model saw.
const MAX_INPUT_CHARS: usize = 48_000;

const DEFAULT_HTTP_TIMEOUT_SECS: u64 = 120;
const DEFAULT_CLI_TIMEOUT_SECS: u64 = 300;

/// The instruction half of the prompt. JSON output is requested because parsing
/// prose out of a free-form answer is the single most fragile part of an LLM
/// integration; a JSON object survives models that like to add pleasantries.
const SYSTEM_PROMPT: &str = "\
You summarize archived web content for a personal archive index.

Reply with a single JSON object and nothing else — no markdown fence, no prose
before or after. The object has exactly these keys:

  \"tldr\":    one sentence, at most 25 words.
  \"summary\": 4 to 6 sentences of plain English describing what the content
              says, its claims, and its conclusion. No preamble like
              \"This article discusses\".
  \"tags\":    an array of at most 5 short lowercase topic tags.

Write in English regardless of the source language. If the content is too
short or empty to summarize, still return the object and say so in \"summary\".";

// ── Request / output types ─────────────────────────────────────────────────

/// Everything the prompt builder needs about one entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryRequest {
    pub entry_uid: String,
    pub title: Option<String>,
    pub source_kind: String,
    pub entity_kind: String,
    pub content: String,
}

/// What a provider produced. `model` is echoed back because HTTP providers may
/// resolve an alias (`claude-3-5-sonnet-latest`) to a dated concrete model, and
/// the concrete one is what we want recorded against the summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryOutput {
    pub text: String,
    pub model: Option<String>,
}

/// One way of turning a [`SummaryRequest`] into text.
///
/// `Send + Sync` so a boxed provider can cross into the server's
/// `spawn_blocking` worker.
pub trait SummaryProvider: Send + Sync {
    /// Stable identifier persisted as `entry_summaries.provider_kind`.
    fn kind(&self) -> &'static str;
    fn model(&self) -> Option<&str>;
    fn summarize(&self, request: &SummaryRequest) -> Result<SummaryOutput>;
}

// ── Configuration ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpProviderConfig {
    /// Full URL, e.g. `https://api.anthropic.com/v1/messages`.
    pub endpoint: String,
    pub api_key: String,
    pub model: String,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliProviderConfig {
    /// Resolved via `ARCHIVR_CLAUDE_CLI` / `ARCHIVR_CODEX_CLI`; a bare name is
    /// left for the OS to resolve on `PATH`, as elsewhere in the tree.
    pub executable: PathBuf,
    pub model: Option<String>,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderConfig {
    AnthropicHttp(HttpProviderConfig),
    OpenAiCompatible(HttpProviderConfig),
    ClaudeCli(CliProviderConfig),
    CodexCli(CliProviderConfig),
}

pub const PROVIDER_KINDS: [&str; 4] = [
    "anthropic_http",
    "openai_compatible",
    "claude_cli",
    "codex_cli",
];

pub fn provider_from_config(cfg: ProviderConfig) -> Box<dyn SummaryProvider> {
    match cfg {
        ProviderConfig::AnthropicHttp(c) => Box::new(AnthropicHttpProvider(c)),
        ProviderConfig::OpenAiCompatible(c) => Box::new(OpenAiCompatibleProvider(c)),
        ProviderConfig::ClaudeCli(c) => Box::new(ClaudeCliProvider(c)),
        ProviderConfig::CodexCli(c) => Box::new(CodexCliProvider(c)),
    }
}

/// Reads a required env var, failing with the *exact variable name* so the
/// server can hand a caller an actionable 400 rather than "not configured".
fn required_env(name: &str) -> Result<String> {
    match env::var(name) {
        Ok(v) if !v.trim().is_empty() => Ok(v),
        _ => bail!("missing required environment variable: {name}"),
    }
}

fn env_or(name: &str, default: &str) -> String {
    env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn optional_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn env_timeout(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

/// Resolve a CLI executable path.
///
/// Priority: `env_name` override → first `well_known_absolute` path that
/// exists → `HOME/.local/bin/<bare>` if it exists → bare name (relies on the
/// server's PATH). The macOS defaults matter for `codex`, which the ChatGPT
/// desktop app installs at `/Applications/ChatGPT.app/Contents/Resources/codex`
/// and does not add to PATH.
fn resolve_cli(env_name: &str, well_known_absolute: &[&str], bare: &str) -> PathBuf {
    if let Some(explicit) = optional_env(env_name) {
        return PathBuf::from(explicit);
    }
    for candidate in well_known_absolute {
        let p = Path::new(candidate);
        if p.is_file() {
            return p.to_path_buf();
        }
    }
    if let Some(home) = env::var_os("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".local/bin");
        p.push(bare);
        if p.is_file() {
            return p;
        }
    }
    PathBuf::from(bare)
}

/// Builds a provider configuration for `kind` purely from the environment.
pub fn provider_from_env(kind: &str) -> Result<ProviderConfig> {
    match kind {
        "anthropic_http" => Ok(ProviderConfig::AnthropicHttp(HttpProviderConfig {
            endpoint: env_or("ARCHIVR_ANTHROPIC_URL", "https://api.anthropic.com/v1/messages"),
            api_key: required_env("ARCHIVR_ANTHROPIC_API_KEY")?,
            model: env_or("ARCHIVR_ANTHROPIC_MODEL", "claude-3-5-sonnet-latest"),
            timeout_secs: env_timeout("ARCHIVR_SUMMARY_HTTP_TIMEOUT", DEFAULT_HTTP_TIMEOUT_SECS),
        })),
        "openai_compatible" => Ok(ProviderConfig::OpenAiCompatible(HttpProviderConfig {
            endpoint: env_or("ARCHIVR_OPENAI_URL", "https://api.openai.com/v1/chat/completions"),
            api_key: required_env("ARCHIVR_OPENAI_API_KEY")?,
            model: env_or("ARCHIVR_OPENAI_MODEL", "gpt-4o-mini"),
            timeout_secs: env_timeout("ARCHIVR_SUMMARY_HTTP_TIMEOUT", DEFAULT_HTTP_TIMEOUT_SECS),
        })),
        "claude_cli" => Ok(ProviderConfig::ClaudeCli(CliProviderConfig {
            executable: resolve_cli(
                "ARCHIVR_CLAUDE_CLI",
                &["/opt/homebrew/bin/claude", "/usr/local/bin/claude"],
                "claude",
            ),
            model: optional_env("ARCHIVR_CLAUDE_MODEL"),
            timeout_secs: env_timeout("ARCHIVR_SUMMARY_CLI_TIMEOUT", DEFAULT_CLI_TIMEOUT_SECS),
        })),
        "codex_cli" => Ok(ProviderConfig::CodexCli(CliProviderConfig {
            executable: resolve_cli(
                "ARCHIVR_CODEX_CLI",
                &[
                    "/Applications/ChatGPT.app/Contents/Resources/codex",
                    "/opt/homebrew/bin/codex",
                    "/usr/local/bin/codex",
                ],
                "codex",
            ),
            model: optional_env("ARCHIVR_CODEX_MODEL"),
            timeout_secs: env_timeout("ARCHIVR_SUMMARY_CLI_TIMEOUT", DEFAULT_CLI_TIMEOUT_SECS),
        })),
        other => bail!(
            "unknown summary provider: {other} (expected one of {})",
            PROVIDER_KINDS.join(", ")
        ),
    }
}

// ── Prompt assembly ────────────────────────────────────────────────────────

/// The user half of the prompt: entry metadata as a small header, then content.
pub fn build_user_prompt(request: &SummaryRequest) -> String {
    let mut s = String::new();
    if let Some(title) = request.title.as_deref().filter(|t| !t.trim().is_empty()) {
        s.push_str(&format!("Title: {title}\n"));
    }
    s.push_str(&format!(
        "Source: {} / {}\n\nContent:\n{}\n",
        request.source_kind, request.entity_kind, request.content
    ));
    s
}

/// CLIs take a single prompt string on stdin, so the system half is prepended
/// rather than passed as a separate role.
fn build_combined_prompt(request: &SummaryRequest) -> String {
    format!("{SYSTEM_PROMPT}\n\n---\n\n{}", build_user_prompt(request))
}

// ── HTTP providers ─────────────────────────────────────────────────────────

fn http_client(timeout_secs: u64) -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        // reqwest's own timeout covers connect + read, which is all a
        // request/response provider needs — no watchdog thread required.
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .context("failed to build HTTP client for summary provider")
}

/// Body builder kept separate from the transport so it can be unit-tested
/// without a network round-trip.
pub fn anthropic_request_body(model: &str, request: &SummaryRequest) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "max_tokens": 1024,
        "messages": [{
            "role": "user",
            "content": build_combined_prompt(request),
        }],
    })
}

pub fn openai_request_body(model: &str, request: &SummaryRequest) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT },
            { "role": "user", "content": build_user_prompt(request) },
        ],
    })
}

struct AnthropicHttpProvider(HttpProviderConfig);

impl SummaryProvider for AnthropicHttpProvider {
    fn kind(&self) -> &'static str {
        "anthropic_http"
    }
    fn model(&self) -> Option<&str> {
        Some(&self.0.model)
    }
    fn summarize(&self, request: &SummaryRequest) -> Result<SummaryOutput> {
        let body = anthropic_request_body(&self.0.model, request);
        let resp = http_client(self.0.timeout_secs)?
            .post(&self.0.endpoint)
            .header("x-api-key", &self.0.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .with_context(|| format!("request to {} failed", self.0.endpoint))?;
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        if !status.is_success() {
            bail!("anthropic API returned {status}: {}", truncate_for_error(&text));
        }
        parse_anthropic_response(&text)
    }
}

pub fn parse_anthropic_response(body: &str) -> Result<SummaryOutput> {
    let json: serde_json::Value =
        serde_json::from_str(body).context("anthropic response was not JSON")?;
    let text = json["content"][0]["text"]
        .as_str()
        .ok_or_else(|| anyhow!("anthropic response had no content[0].text"))?;
    Ok(SummaryOutput {
        text: text.to_string(),
        model: json["model"].as_str().map(str::to_string),
    })
}

struct OpenAiCompatibleProvider(HttpProviderConfig);

impl SummaryProvider for OpenAiCompatibleProvider {
    fn kind(&self) -> &'static str {
        "openai_compatible"
    }
    fn model(&self) -> Option<&str> {
        Some(&self.0.model)
    }
    fn summarize(&self, request: &SummaryRequest) -> Result<SummaryOutput> {
        let body = openai_request_body(&self.0.model, request);
        let resp = http_client(self.0.timeout_secs)?
            .post(&self.0.endpoint)
            .header("authorization", format!("Bearer {}", self.0.api_key))
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .with_context(|| format!("request to {} failed", self.0.endpoint))?;
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        if !status.is_success() {
            bail!(
                "openai-compatible API returned {status}: {}",
                truncate_for_error(&text)
            );
        }
        parse_openai_response(&text)
    }
}

pub fn parse_openai_response(body: &str) -> Result<SummaryOutput> {
    let json: serde_json::Value =
        serde_json::from_str(body).context("openai-compatible response was not JSON")?;
    let text = json["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| anyhow!("response had no choices[0].message.content"))?;
    Ok(SummaryOutput {
        text: text.to_string(),
        model: json["model"].as_str().map(str::to_string),
    })
}

fn truncate_for_error(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= 400 {
        return trimmed.to_string();
    }
    trimmed.chars().take(400).collect::<String>() + "…"
}

// ── CLI providers ──────────────────────────────────────────────────────────

/// Runs `executable args…`, writes `prompt` to its stdin, and returns stdout.
///
/// `archivr-core` deliberately has no async runtime and the tree carries no
/// `wait_timeout` dependency, so the timeout is enforced by structure rather
/// than by a library: stdout is drained on its own thread and handed back over
/// a channel, which leaves the calling thread free to `recv_timeout` and kill
/// the child if it overruns. stdin is written on a third thread because a
/// 48 KB prompt can exceed the pipe buffer, and writing it inline would
/// deadlock against a child that is waiting for us to read its output.
fn run_cli(
    executable: &Path,
    args: &[&str],
    prompt: &str,
    timeout_secs: u64,
) -> Result<String> {
    let mut child = Command::new(executable)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn {}", executable.display()))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("failed to open stdin for {}", executable.display()))?;
    let prompt_owned = prompt.to_string();
    thread::spawn(move || {
        let _ = stdin.write_all(prompt_owned.as_bytes());
        // Dropping stdin closes the pipe, which is what tells the CLI the
        // prompt is complete.
    });

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("failed to open stdout for {}", executable.display()))?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = String::new();
        use std::io::Read;
        let mut stdout = stdout;
        let res = stdout.read_to_string(&mut buf).map(|_| buf);
        let _ = tx.send(res);
    });

    let collected = match rx.recv_timeout(Duration::from_secs(timeout_secs)) {
        Ok(res) => res.with_context(|| format!("failed to read stdout of {}", executable.display()))?,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "{} timed out after {timeout_secs}s",
                executable.display()
            );
        }
    };

    let status = child
        .wait()
        .with_context(|| format!("failed to wait for {}", executable.display()))?;
    if !status.success() {
        let mut stderr = String::new();
        if let Some(mut e) = child.stderr.take() {
            use std::io::Read;
            let _ = e.read_to_string(&mut stderr);
        }
        bail!(
            "{} exited with {status}: {}",
            executable.display(),
            truncate_for_error(&stderr)
        );
    }
    Ok(collected)
}

struct ClaudeCliProvider(CliProviderConfig);

impl SummaryProvider for ClaudeCliProvider {
    fn kind(&self) -> &'static str {
        "claude_cli"
    }
    fn model(&self) -> Option<&str> {
        self.0.model.as_deref()
    }
    fn summarize(&self, request: &SummaryRequest) -> Result<SummaryOutput> {
        // `claude -p --output-format text` is the documented one-shot
        // ("print") mode of the Claude Code CLI: it reads the prompt from
        // stdin, writes the answer to stdout, and exits.
        let mut args: Vec<&str> = vec!["-p", "--output-format", "text"];
        if let Some(model) = self.0.model.as_deref() {
            args.push("--model");
            args.push(model);
        }
        let out = run_cli(&self.0.executable, &args, &build_combined_prompt(request), self.0.timeout_secs)?;
        Ok(SummaryOutput {
            text: out,
            model: self.0.model.clone(),
        })
    }
}

/// Codex invocation lives in its own module because its one-shot interface is
/// the least stable of the four.
///
/// Primary form is `codex exec --output-last-message <file> -`, which reads the
/// prompt from stdin and writes ONLY the final assistant message to `<file>`.
/// Without `--output-last-message`, stdout is polluted with a header
/// (`OpenAI Codex vX`, session id, model, sandbox, …) and a footer
/// (`tokens used`, message replay), and the JSON extractor can pick up the
/// echoed user prompt instead of the real answer. Older builds that reject
/// `-` as stdin marker fall back to a positional prompt.
mod codex {
    use super::*;

    /// A short-lived path in the OS temp dir. Unique per (pid, wall time) so
    /// concurrent summarizations don't collide.
    fn last_message_temp_path() -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        env::temp_dir().join(format!(
            "archivr-codex-{}-{}.txt",
            std::process::id(),
            stamp
        ))
    }

    fn missing_binary_hint(cfg: &CliProviderConfig) -> &'static str {
        // Only nudge users about the env var when we're running the default
        // bare "codex" and it failed — an explicit ARCHIVR_CODEX_CLI path
        // failure is their configuration, not a discovery gap.
        if cfg.executable == Path::new("codex") {
            " (hint: set ARCHIVR_CODEX_CLI to your codex binary; on macOS the              ChatGPT desktop app installs it at              /Applications/ChatGPT.app/Contents/Resources/codex)"
        } else {
            ""
        }
    }

    fn read_and_cleanup(path: &Path) -> Option<String> {
        let out = std::fs::read_to_string(path).ok()?;
        let _ = std::fs::remove_file(path);
        let trimmed = out.trim();
        if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
    }

    pub fn run(cfg: &CliProviderConfig, prompt: &str) -> Result<String> {
        let out_path = last_message_temp_path();
        let out_str = out_path.to_string_lossy().into_owned();

        // Primary: stdin prompt + --output-last-message.
        let mut primary: Vec<String> = vec![
            "exec".into(),
            "--output-last-message".into(),
            out_str.clone(),
        ];
        if let Some(model) = cfg.model.as_deref() {
            primary.push("--model".into());
            primary.push(model.into());
        }
        primary.push("-".into());
        let primary_refs: Vec<&str> = primary.iter().map(String::as_str).collect();

        let primary_err = match run_cli(&cfg.executable, &primary_refs, prompt, cfg.timeout_secs) {
            Ok(_) => {
                if let Some(text) = read_and_cleanup(&out_path) {
                    return Ok(text);
                }
                // Codex succeeded but wrote nothing to the file — extremely
                // rare, but treat as a soft failure so we try the fallback.
                anyhow!("codex produced no last-message output")
            }
            Err(e) => {
                let _ = std::fs::remove_file(&out_path);
                e
            }
        };

        // Fallback: positional prompt, no stdin, same --output-last-message.
        let mut fb: Vec<String> = vec![
            "exec".into(),
            "--output-last-message".into(),
            out_str.clone(),
        ];
        if let Some(model) = cfg.model.as_deref() {
            fb.push("--model".into());
            fb.push(model.into());
        }
        fb.push(prompt.into());
        let out = Command::new(&cfg.executable)
            .args(&fb)
            .output()
            .with_context(|| {
                format!(
                    "codex `exec -` failed ({primary_err:#}); positional fallback also failed to spawn{}",
                    missing_binary_hint(cfg)
                )
            })?;
        if !out.status.success() {
            let _ = std::fs::remove_file(&out_path);
            bail!(
                "codex `exec -` failed ({primary_err:#}); positional fallback exited with {}: {}",
                out.status,
                truncate_for_error(&String::from_utf8_lossy(&out.stderr))
            );
        }
        if let Some(text) = read_and_cleanup(&out_path) {
            return Ok(text);
        }
        // Last resort — the child succeeded but wrote nothing to the file. Fall
        // back to raw stdout so the caller has *something* to normalize.
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }
}

struct CodexCliProvider(CliProviderConfig);

impl SummaryProvider for CodexCliProvider {
    fn kind(&self) -> &'static str {
        "codex_cli"
    }
    fn model(&self) -> Option<&str> {
        self.0.model.as_deref()
    }
    fn summarize(&self, request: &SummaryRequest) -> Result<SummaryOutput> {
        let out = codex::run(&self.0, &build_combined_prompt(request))?;
        Ok(SummaryOutput {
            text: out,
            model: self.0.model.clone(),
        })
    }
}

// ── Content extraction ─────────────────────────────────────────────────────

/// Strips markup from an archived HTML page.
///
/// Deliberately regex-based rather than a real parser: `html5ever` is not in
/// the dependency tree, and pulling a full HTML parser in to feed a language
/// model — which tolerates imperfect whitespace and stray angle brackets
/// fine — is not worth the build cost. `<script>`/`<style>`/`<noscript>` bodies
/// are removed first (they are the only tags whose *content* is not prose), then
/// remaining tags are dropped and whitespace collapsed.
pub fn strip_html(html: &str) -> String {
    // Building these per call keeps the function self-contained; extraction runs
    // at most once per summary request, so compilation cost is irrelevant here.
    // Spelled out per tag rather than with a `\1` backreference: Rust's regex
    // engine has none by design — it is a finite automaton, which is what buys
    // the linear-time guarantee we want when running over untrusted archived HTML.
    // Spelled out per tag rather than with a `\1` backreference: Rust's regex
    // engine has none by design — it is a finite automaton, which is what buys
    // the linear-time guarantee we want when running over untrusted archived HTML.
    let drop_blocks = regex::Regex::new(concat!(
        r"(?is)<script\b[^>]*>.*?</\s*script\s*>",
        r"|<style\b[^>]*>.*?</\s*style\s*>",
        r"|<noscript\b[^>]*>.*?</\s*noscript\s*>",
        r"|<template\b[^>]*>.*?</\s*template\s*>",
    ))
    .unwrap();
    let comments = regex::Regex::new(r"(?s)<!--.*?-->").unwrap();
    // Treat block-level closers as line breaks so paragraphs do not run together.
    let breaks = regex::Regex::new(r"(?i)</\s*(p|div|br|li|h[1-6]|tr|section|article)\s*>").unwrap();
    let tags = regex::Regex::new(r"(?s)<[^>]*>").unwrap();
    let spaces = regex::Regex::new(r"[ \t\r\f\v]+").unwrap();
    let blank_lines = regex::Regex::new(r"\n{3,}").unwrap();

    let s = drop_blocks.replace_all(html, " ");
    let s = comments.replace_all(&s, " ");
    let s = breaks.replace_all(&s, "\n");
    let s = tags.replace_all(&s, " ");
    let s = decode_entities(&s);
    let s = spaces.replace_all(&s, " ");
    let s = s
        .lines()
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n");
    blank_lines.replace_all(&s, "\n\n").trim().to_string()
}

/// Decodes only the handful of entities that actually change meaning in prose.
/// A model does not need a complete entity table.
fn decode_entities(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace("&hellip;", "…")
}

/// Pulls the human text out of a scraped tweet JSON payload, tolerating both
/// the flat shape and a `{ "tweet": { … } }` wrapper, and appending any thread
/// entries so a self-reply chain summarizes as one piece.
fn nonempty_string(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

fn flatten_article_blocks(v: &serde_json::Value, out: &mut Vec<String>) {
    const TEXT_KEYS: &[&str] = &["text", "plain_text", "content", "body", "title", "heading"];

    match v {
        serde_json::Value::Array(items) => {
            for item in items {
                flatten_article_blocks(item, out);
            }
        }
        serde_json::Value::Object(fields) => {
            for (key, value) in fields {
                if TEXT_KEYS.contains(&key.as_str()) {
                    if let Some(text) = value.as_str().filter(|text| !text.trim().is_empty()) {
                        out.push(text.to_string());
                    }
                }
                flatten_article_blocks(value, out);
            }
        }
        _ => {}
    }
}

fn article_text(status: &serde_json::Value) -> Option<String> {
    let article = status.get("article")?;
    article.as_object()?;
    let title = nonempty_string(article, "title");
    let body = nonempty_string(article, "plain_text")
        .or_else(|| {
            let mut blocks = Vec::new();
            if let Some(value) = article.get("blocks") {
                flatten_article_blocks(value, &mut blocks);
            }
            (!blocks.is_empty()).then(|| blocks.join("\n\n"))
        })
        .or_else(|| nonempty_string(article, "preview_text"))
        .or_else(|| nonempty_string(article, "summary_text"))?;

    Some(match title.as_deref() {
        Some(title) => format!("{title}\n\n{body}"),
        None => body,
    })
}

pub fn extract_tweet_text(json: &serde_json::Value) -> Option<String> {
    fn one(v: &serde_json::Value) -> Option<String> {
        article_text(v).or_else(|| {
            ["full_text", "text", "content", "body"]
                .into_iter()
                .find_map(|key| nonempty_string(v, key))
        })
    }
    let root = json.get("tweet").unwrap_or(json);
    let mut parts: Vec<String> = Vec::new();
    if let Some(t) = one(root) {
        parts.push(t);
    }
    for key in ["thread", "tweets", "replies"] {
        if let Some(arr) = root.get(key).and_then(|x| x.as_array()) {
            parts.extend(arr.iter().filter_map(one));
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

/// The text that will be sent to a model, plus the digest that identifies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryInput {
    pub request: SummaryRequest,
    /// Digest of `request.content` *after* truncation, so the cache key
    /// describes exactly the bytes the model saw.
    ///
    /// The column is named `input_sha256` for readability, but the digest is
    /// SHA3-256 via [`crate::hash::hash_bytes`] — the tree's single hashing
    /// primitive. Adding a second hash family for one column is not worth it.
    pub input_sha256: String,
}

fn extension_of(relpath: &str) -> String {
    Path::new(relpath)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Loads an entry's primary artifact and reduces it to plain text.
///
/// Returns a descriptive error rather than a summary for artifact kinds v1 does
/// not handle (video, audio, images): the caller surfaces it to the UI, and a
/// clear "unsupported" beats an empty or hallucinated summary.
fn load_summary_artifacts(
    conn: &rusqlite::Connection,
    entry_id: i64,
    artifact_role: &str,
) -> Result<Vec<(String, Option<String>)>> {
    let mut stmt = conn.prepare(
        "SELECT ea.relpath, b.mime_type
         FROM entry_artifacts ea
         LEFT JOIN blobs b ON b.id = ea.blob_id
         WHERE ea.entry_id = ?1 AND ea.artifact_role = ?2
         ORDER BY ea.id ASC",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![entry_id, artifact_role], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn build_summary_input(paths: &ArchivePaths, entry_uid: &str) -> Result<SummaryInput> {
    let conn = database::open_or_initialize(&paths.archive_path)?;
    let (entry_id, title, source_kind, entity_kind) = conn
        .query_row(
            "SELECT id, title, source_kind, entity_kind
             FROM archived_entries WHERE entry_uid = ?1",
            [entry_uid],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .map_err(|_| anyhow!("entry not found: {entry_uid}"))?;

    // Tweets and tweet threads use `raw_tweet_json` rather than `primary_media`,
    // and a THREAD is materialized as N separate JSON files (one per status).
    // Load every matching artifact in insertion order so a thread summarizes
    // as the whole conversation, not just its first status.
    let is_tweetish = matches!(entity_kind.as_str(), "tweet" | "tweet_thread");
    let primary_role = if is_tweetish { "raw_tweet_json" } else { "primary_media" };
    let mut artifacts = load_summary_artifacts(&conn, entry_id, primary_role)?;
    if artifacts.is_empty() && is_tweetish {
        // Older archives may have stored tweet payloads under `primary_media`.
        artifacts = load_summary_artifacts(&conn, entry_id, "primary_media")?;
    }
    if artifacts.is_empty() {
        bail!("entry {entry_uid} has no {primary_role} artifact to summarize");
    }

    let mut pieces: Vec<String> = Vec::with_capacity(artifacts.len());
    for (relpath, mime_opt) in &artifacts {
        let abs = paths.store_path.join(relpath);
        let ext = extension_of(relpath);
        let mime = mime_opt.clone().unwrap_or_default();

        let piece = if ext == "md" || ext == "markdown" || ext == "txt"
            || mime.starts_with("text/markdown") || mime == "text/plain"
        {
            std::fs::read_to_string(&abs)
                .with_context(|| format!("failed to read {}", abs.display()))?
        } else if ext == "html" || ext == "htm" || mime.starts_with("text/html") {
            let raw = std::fs::read_to_string(&abs)
                .with_context(|| format!("failed to read {}", abs.display()))?;
            strip_html(&raw)
        } else if ext == "json" || mime == "application/json" {
            let raw = std::fs::read_to_string(&abs)
                .with_context(|| format!("failed to read {}", abs.display()))?;
            let parsed: serde_json::Value = serde_json::from_str(&raw)
                .with_context(|| format!("{} is not valid JSON", abs.display()))?;
            extract_tweet_text(&parsed).unwrap_or_default()
        } else {
            bail!(
                "no text content available for this entry kind — v1 unsupported \
                 (artifact {relpath}, mime {})",
                if mime.is_empty() { "unknown" } else { &mime }
            );
        };
        if !piece.trim().is_empty() {
            pieces.push(piece);
        }
    }
    // Thread joiner: `---` on its own line reads as a paragraph break to both
    // humans and models. Single-piece entries never render the separator.
    let content = pieces.join("\n\n---\n\n").trim().to_string();
    if content.is_empty() {
        bail!("no text content available for this entry kind — v1 unsupported (extracted text was empty)");
    }
    // Truncate on a char boundary, then hash: the digest must describe the
    // bytes actually sent, or the cache would key on content the model never saw.
    let content: String = if content.chars().count() > MAX_INPUT_CHARS {
        content.chars().take(MAX_INPUT_CHARS).collect()
    } else {
        content
    };
    let input_sha256 = hash::hash_bytes(content.as_bytes());

    Ok(SummaryInput {
        request: SummaryRequest {
            entry_uid: entry_uid.to_string(),
            title,
            source_kind,
            entity_kind,
            content,
        },
        input_sha256,
    })
}

// ── Orchestration ──────────────────────────────────────────────────────────

/// Normalizes provider output into the `{tldr, summary, tags}` JSON we persist.
///
/// Models routinely wrap JSON in a ```json fence or add a sentence around it, so
/// we fence-strip and then take the outermost brace pair. If nothing parses, the
/// raw text is still preserved under `summary` — a slightly-off summary is far
/// more useful to a reader than a hard failure.
pub fn normalize_summary_json(raw: &str) -> String {
    let trimmed = raw.trim();
    let unfenced = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|s| s.trim_start())
        .and_then(|s| s.strip_suffix("```").or(Some(s)))
        .unwrap_or(trimmed)
        .trim();

    let candidate = match (unfenced.find('{'), unfenced.rfind('}')) {
        (Some(a), Some(b)) if b > a => &unfenced[a..=b],
        _ => unfenced,
    };

    if let Ok(v) = serde_json::from_str::<serde_json::Value>(candidate) {
        if v.get("summary").and_then(|s| s.as_str()).is_some() {
            return v.to_string();
        }
    }
    serde_json::json!({
        "tldr": "",
        "summary": trimmed,
        "tags": [],
    })
    .to_string()
}

/// Full manual summarization pass for one entry, persisting the result.
///
/// Owns the whole row lifecycle (`pending` → `running` → `completed`/`failed`)
/// so a caller running it on a background thread only has to handle the
/// `Err` case. The `(entry_id, provider_kind, provider_model, prompt_version,
/// input_sha256)` UNIQUE key means re-running against unchanged input reuses the
/// same row rather than accumulating duplicates.
pub fn summarize_entry(
    archive_paths: &ArchivePaths,
    entry_uid: &str,
    provider: &dyn SummaryProvider,
    prompt_version: &str,
) -> Result<database::EntrySummaryRecord> {
    let conn = database::open_or_initialize(&archive_paths.archive_path)?;
    let entry_id = database::entry_id_for_uid(&conn, entry_uid)?
        .ok_or_else(|| anyhow!("entry not found: {entry_uid}"))?;

    let input = build_summary_input(archive_paths, entry_uid)?;
    let summary_uid = database::upsert_pending_entry_summary(
        &conn,
        entry_id,
        provider.kind(),
        provider.model(),
        prompt_version,
        &input.input_sha256,
    )?;
    database::update_entry_summary_status(&conn, &summary_uid, "running", None, None)?;

    match provider.summarize(&input.request) {
        Ok(output) => {
            let text = normalize_summary_json(&output.text);
            database::update_entry_summary_status(
                &conn,
                &summary_uid,
                "completed",
                Some(&text),
                None,
            )?;
        }
        Err(e) => {
            let msg = format!("{e:#}");
            database::update_entry_summary_status(
                &conn,
                &summary_uid,
                "failed",
                None,
                Some(&msg),
            )?;
            return Err(e);
        }
    }

    database::get_entry_summary_by_uid(&conn, &summary_uid)?
        .ok_or_else(|| anyhow!("summary row disappeared after write"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Env vars are process-global, so provider_from_env tests must not run
    /// concurrently with one another. A single mutex around every such test
    /// serializes them without needing a test-harness flag.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn clear_provider_env() {
        for k in [
            "ARCHIVR_ANTHROPIC_API_KEY",
            "ARCHIVR_ANTHROPIC_URL",
            "ARCHIVR_ANTHROPIC_MODEL",
            "ARCHIVR_OPENAI_API_KEY",
            "ARCHIVR_OPENAI_URL",
            "ARCHIVR_OPENAI_MODEL",
            "ARCHIVR_CLAUDE_CLI",
            "ARCHIVR_CLAUDE_MODEL",
            "ARCHIVR_CODEX_CLI",
            "ARCHIVR_CODEX_MODEL",
            "ARCHIVR_SUMMARY_HTTP_TIMEOUT",
            "ARCHIVR_SUMMARY_CLI_TIMEOUT",
        ] {
            unsafe { env::remove_var(k) };
        }
    }

    fn sample_request() -> SummaryRequest {
        SummaryRequest {
            entry_uid: "ent_test".into(),
            title: Some("A Title".into()),
            source_kind: "web".into(),
            entity_kind: "page".into(),
            content: "Body text.".into(),
        }
    }

    #[test]
    fn provider_from_env_anthropic_uses_defaults_when_only_key_is_set() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_provider_env();
        unsafe { env::set_var("ARCHIVR_ANTHROPIC_API_KEY", "sk-test") };
        let cfg = provider_from_env("anthropic_http").unwrap();
        let ProviderConfig::AnthropicHttp(c) = cfg else {
            panic!("wrong variant")
        };
        assert_eq!(c.api_key, "sk-test");
        assert_eq!(c.endpoint, "https://api.anthropic.com/v1/messages");
        assert_eq!(c.model, "claude-3-5-sonnet-latest");
        assert_eq!(c.timeout_secs, DEFAULT_HTTP_TIMEOUT_SECS);
        clear_provider_env();
    }

    #[test]
    fn provider_from_env_anthropic_missing_key_names_the_variable() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_provider_env();
        let err = provider_from_env("anthropic_http").unwrap_err().to_string();
        // The exact var name is what the server surfaces in its 400, so it is
        // part of the contract, not just a nicety.
        assert!(err.contains("ARCHIVR_ANTHROPIC_API_KEY"), "got: {err}");
    }

    #[test]
    fn provider_from_env_openai_missing_key_names_the_variable() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_provider_env();
        let err = provider_from_env("openai_compatible").unwrap_err().to_string();
        assert!(err.contains("ARCHIVR_OPENAI_API_KEY"), "got: {err}");
    }

    #[test]
    fn provider_from_env_openai_honours_overrides() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_provider_env();
        unsafe {
            env::set_var("ARCHIVR_OPENAI_API_KEY", "k");
            env::set_var("ARCHIVR_OPENAI_URL", "http://localhost:1234/v1/chat/completions");
            env::set_var("ARCHIVR_OPENAI_MODEL", "local-model");
            env::set_var("ARCHIVR_SUMMARY_HTTP_TIMEOUT", "7");
        }
        let ProviderConfig::OpenAiCompatible(c) = provider_from_env("openai_compatible").unwrap()
        else {
            panic!("wrong variant")
        };
        assert_eq!(c.endpoint, "http://localhost:1234/v1/chat/completions");
        assert_eq!(c.model, "local-model");
        assert_eq!(c.timeout_secs, 7);
        clear_provider_env();
    }

    #[test]
    fn provider_from_env_clis_default_to_bare_binary_names() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_provider_env();
        let ProviderConfig::ClaudeCli(c) = provider_from_env("claude_cli").unwrap() else {
            panic!("wrong variant")
        };
        // Same rationale as the codex case below: resolve_cli may discover a
        // well-known install path, so accept either the bare name or any
        // file-name-`claude` path.
        assert!(
            c.executable == PathBuf::from("claude")
                || c.executable.file_name().map(|f| f == "claude").unwrap_or(false),
            "unexpected claude executable: {}",
            c.executable.display()
        );
        assert_eq!(c.model, None);
        assert_eq!(c.timeout_secs, DEFAULT_CLI_TIMEOUT_SECS);

        let ProviderConfig::CodexCli(c) = provider_from_env("codex_cli").unwrap() else {
            panic!("wrong variant")
        };
        // Either the well-known ChatGPT.app path (if present on this host) or
        // the bare `codex` fallback is acceptable — `resolve_cli` is
        // deliberately opportunistic.
        assert!(
            c.executable == PathBuf::from("codex")
                || c.executable.file_name().map(|f| f == "codex").unwrap_or(false),
            "unexpected codex executable: {}",
            c.executable.display()
        );
    }

    #[test]
    fn provider_from_env_rejects_unknown_kind() {
        let _g = ENV_LOCK.lock().unwrap();
        let err = provider_from_env("gemini").unwrap_err().to_string();
        assert!(err.contains("unknown summary provider"), "got: {err}");
    }

    #[test]
    fn provider_from_config_reports_matching_kind_and_model() {
        let p = provider_from_config(ProviderConfig::AnthropicHttp(HttpProviderConfig {
            endpoint: "http://x".into(),
            api_key: "k".into(),
            model: "m".into(),
            timeout_secs: 5,
        }));
        assert_eq!(p.kind(), "anthropic_http");
        assert_eq!(p.model(), Some("m"));

        let p = provider_from_config(ProviderConfig::ClaudeCli(CliProviderConfig {
            executable: PathBuf::from("claude"),
            model: None,
            timeout_secs: 5,
        }));
        assert_eq!(p.kind(), "claude_cli");
        assert_eq!(p.model(), None);
    }

    // ── Request-body builders ──────────────────────────────────────────────
    //
    // Neither `mockito` nor `wiremock` is in dev-dependencies, and introducing
    // a mock HTTP server (plus its transitive tree) to assert a JSON shape is a
    // poor trade. The two halves that can actually break — the request body we
    // send and the response shape we parse — are tested directly instead, which
    // covers everything except reqwest's own transport.

    #[test]
    fn anthropic_body_has_required_shape() {
        let body = anthropic_request_body("claude-3-5-sonnet-latest", &sample_request());
        assert_eq!(body["model"], "claude-3-5-sonnet-latest");
        assert_eq!(body["max_tokens"], 1024);
        assert_eq!(body["messages"][0]["role"], "user");
        let content = body["messages"][0]["content"].as_str().unwrap();
        // Anthropic's Messages API takes one user turn, so the system half must
        // be folded into it or the JSON-output instruction is simply lost.
        assert!(content.contains("\"tldr\""));
        assert!(content.contains("A Title"));
        assert!(content.contains("Body text."));
    }

    #[test]
    fn openai_body_splits_system_and_user_roles() {
        let body = openai_request_body("gpt-4o-mini", &sample_request());
        assert_eq!(body["model"], "gpt-4o-mini");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "user");
        assert!(
            body["messages"][1]["content"]
                .as_str()
                .unwrap()
                .contains("Body text.")
        );
        assert!(
            !body["messages"][1]["content"]
                .as_str()
                .unwrap()
                .contains("\"tldr\"")
        );
    }

    #[test]
    fn parse_anthropic_response_extracts_text_and_model() {
        let body = r#"{"model":"claude-3-5-sonnet-20241022","content":[{"type":"text","text":"hi"}]}"#;
        let out = parse_anthropic_response(body).unwrap();
        assert_eq!(out.text, "hi");
        assert_eq!(out.model.as_deref(), Some("claude-3-5-sonnet-20241022"));
    }

    #[test]
    fn parse_anthropic_response_errors_without_content() {
        assert!(parse_anthropic_response(r#"{"error":"nope"}"#).is_err());
    }

    #[test]
    fn parse_openai_response_extracts_message_content() {
        let body = r#"{"model":"gpt-4o-mini","choices":[{"message":{"content":"hello"}}]}"#;
        let out = parse_openai_response(body).unwrap();
        assert_eq!(out.text, "hello");
        assert_eq!(out.model.as_deref(), Some("gpt-4o-mini"));
    }

    // ── Content extraction ─────────────────────────────────────────────────

    #[test]
    fn strip_html_drops_script_and_style_bodies() {
        let html = "<html><head><style>p{color:red}</style><script>var x=1;</script></head>\
                    <body><p>Hello world</p></body></html>";
        let text = strip_html(html);
        assert!(text.contains("Hello world"));
        assert!(!text.contains("color:red"));
        assert!(!text.contains("var x"));
    }

    #[test]
    fn strip_html_keeps_paragraphs_apart() {
        let text = strip_html("<p>One</p><p>Two</p>");
        // Without block-level break handling these would run together as
        // "OneTwo", which reads as a single garbled sentence to the model.
        assert!(text.contains("One"));
        assert!(text.contains("Two"));
        assert!(!text.contains("OneTwo"));
    }

    #[test]
    fn strip_html_decodes_common_entities() {
        assert_eq!(strip_html("<p>a &amp; b &nbsp;c</p>").replace('\u{a0}', " "), "a & b c");
    }

    #[test]
    fn extract_tweet_text_handles_flat_and_wrapped_shapes() {
        let flat = serde_json::json!({ "full_text": "tweet body" });
        assert_eq!(extract_tweet_text(&flat).unwrap(), "tweet body");

        let wrapped = serde_json::json!({ "tweet": { "text": "wrapped body" } });
        assert_eq!(extract_tweet_text(&wrapped).unwrap(), "wrapped body");

        let threaded = serde_json::json!({
            "full_text": "first",
            "thread": [{ "full_text": "second" }],
        });
        assert_eq!(extract_tweet_text(&threaded).unwrap(), "first\n\nsecond");

        assert!(extract_tweet_text(&serde_json::json!({ "id": 1 })).is_none());
    }

    #[test]
    fn extract_tweet_text_prefers_x_article_plain_text_over_tco_body() {
        let tweet = serde_json::json!({
            "full_text": "https://t.co/article",
            "article": { "title": "Skin guide", "plain_text": "Use sunscreen daily." }
        });
        assert_eq!(
            extract_tweet_text(&tweet).as_deref(),
            Some("Skin guide\n\nUse sunscreen daily.")
        );
    }

    #[test]
    fn extract_tweet_text_uses_article_blocks_when_plain_text_is_empty() {
        let tweet = serde_json::json!({"article": {
            "title": "Blocks", "plain_text": " ",
            "blocks": [
                {"text": "First", "id": "ignored", "media_url": "https://example.test/image"},
                {"children": [{"text": "Second", "enabled": true}]}
            ]
        }});
        assert_eq!(
            extract_tweet_text(&tweet).as_deref(),
            Some("Blocks\n\nFirst\n\nSecond")
        );
    }

    #[test]
    fn extract_tweet_text_keeps_article_block_object_field_order() {
        let tweet: serde_json::Value = serde_json::from_str(
            r#"{
                "article": {
                    "title": "Ordered block",
                    "blocks": [{"heading": "Opening", "content": "Body copy"}]
                }
            }"#,
        )
        .unwrap();
        assert_eq!(
            extract_tweet_text(&tweet).as_deref(),
            Some("Ordered block\n\nOpening\n\nBody copy")
        );
    }

    #[test]
    fn extract_tweet_text_falls_back_from_article_preview_to_summary_then_tweet_body() {
        let preview = serde_json::json!({
            "full_text": "https://t.co/fallback",
            "article": { "title": "Preview", "preview_text": "Preview copy", "summary_text": "Later" }
        });
        assert_eq!(
            extract_tweet_text(&preview).as_deref(),
            Some("Preview\n\nPreview copy")
        );

        let summary = serde_json::json!({
            "full_text": "https://t.co/fallback",
            "article": { "title": "Summary", "summary_text": "Summary copy" }
        });
        assert_eq!(
            extract_tweet_text(&summary).as_deref(),
            Some("Summary\n\nSummary copy")
        );

        let empty_article = serde_json::json!({
            "full_text": "https://t.co/fallback",
            "article": { "title": "Only a title", "blocks": [{"id": "not text"}] }
        });
        assert_eq!(
            extract_tweet_text(&empty_article).as_deref(),
            Some("https://t.co/fallback")
        );
    }

    #[test]
    fn build_summary_input_joins_article_backed_tweet_artifacts() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::archive::initialize_archive(
            temp.path(),
            &temp.path().join("store"),
            "Test archive",
            false,
        )
        .unwrap();
        let conn = database::open_or_initialize(&paths.archive_path).unwrap();
        let user_id = database::ensure_default_user(&conn).unwrap();
        let run = database::create_archive_run(&conn, user_id, 1).unwrap();
        let source_id = database::upsert_source_identity(
            &conn,
            "twitter",
            "tweet_thread",
            Some("thread-1"),
            Some("https://x.com/example/status/1"),
            "x:thread-1",
        )
        .unwrap();
        let entry = database::create_archived_entry(
            &conn,
            &database::NewEntry {
                source_identity_id: source_id,
                archive_run_id: run.id,
                parent_entry_id: None,
                root_entry_id: None,
                created_by_user_id: user_id,
                owned_by_user_id: user_id,
                source_kind: "twitter".to_string(),
                entity_kind: "tweet_thread".to_string(),
                title: Some("Article thread".to_string()),
                visibility: "private".to_string(),
                representation_kind: "tweet_thread".to_string(),
                source_metadata_json: "{}".to_string(),
                display_metadata_json: None,
            },
        )
        .unwrap();

        for (ordinal, (relpath, body)) in [
            ("raw_tweets/article-one.json", "First article body."),
            ("raw_tweets/article-two.json", "Second article body."),
        ]
        .into_iter()
        .enumerate()
        {
            let path = paths.store_path.join(relpath);
            std::fs::write(
                &path,
                serde_json::json!({
                    "full_text": format!("https://t.co/{ordinal}"),
                    "article": { "title": format!("Article {}", ordinal + 1), "plain_text": body }
                })
                .to_string(),
            )
            .unwrap();
            database::add_entry_artifact(
                &conn,
                &database::NewArtifact {
                    entry_id: entry.id,
                    artifact_role: "raw_tweet_json".to_string(),
                    storage_area: "raw_tweets".to_string(),
                    relpath: relpath.to_string(),
                    blob_id: None,
                    logical_path: None,
                    metadata_json: None,
                },
            )
            .unwrap();
        }

        let input = build_summary_input(&paths, &entry.entry_uid).unwrap();
        assert!(input.request.content.contains("First article body.\n\n---\n\nArticle 2\n\nSecond article body."));
    }

    // ── Output normalization ───────────────────────────────────────────────

    #[test]
    fn normalize_summary_json_passes_through_clean_json() {
        let raw = r#"{"tldr":"t","summary":"s","tags":["a"]}"#;
        let v: serde_json::Value = serde_json::from_str(&normalize_summary_json(raw)).unwrap();
        assert_eq!(v["tldr"], "t");
        assert_eq!(v["tags"][0], "a");
    }

    #[test]
    fn normalize_summary_json_strips_markdown_fences() {
        let raw = "```json\n{\"tldr\":\"t\",\"summary\":\"s\",\"tags\":[]}\n```";
        let v: serde_json::Value = serde_json::from_str(&normalize_summary_json(raw)).unwrap();
        assert_eq!(v["summary"], "s");
    }

    #[test]
    fn normalize_summary_json_recovers_json_wrapped_in_prose() {
        let raw = "Sure! Here you go:\n{\"tldr\":\"t\",\"summary\":\"s\",\"tags\":[]}\nHope that helps.";
        let v: serde_json::Value = serde_json::from_str(&normalize_summary_json(raw)).unwrap();
        assert_eq!(v["tldr"], "t");
    }

    #[test]
    fn normalize_summary_json_wraps_unparseable_output_rather_than_losing_it() {
        // A model that ignored the format instruction still produced something
        // a human can read; discarding it would be worse than a missing tldr.
        let v: serde_json::Value =
            serde_json::from_str(&normalize_summary_json("just prose")).unwrap();
        assert_eq!(v["summary"], "just prose");
        assert_eq!(v["tldr"], "");
    }

    // ── CLI runner ─────────────────────────────────────────────────────────

    #[test]
    fn run_cli_round_trips_stdin_to_stdout() {
        // `cat` stands in for a provider CLI: it proves the prompt reaches the
        // child's stdin and the child's stdout comes back intact.
        let out = run_cli(Path::new("cat"), &[], "prompt text", 30).unwrap();
        assert_eq!(out, "prompt text");
    }

    #[test]
    fn run_cli_kills_a_child_that_overruns_its_timeout() {
        let err = run_cli(Path::new("sleep"), &["30"], "", 1).unwrap_err().to_string();
        assert!(err.contains("timed out"), "got: {err}");
    }

    #[test]
    fn run_cli_reports_a_nonzero_exit() {
        let err = run_cli(Path::new("false"), &[], "", 30).unwrap_err().to_string();
        assert!(err.contains("exited with"), "got: {err}");
    }
}
