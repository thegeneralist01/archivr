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
use base64::Engine;
use std::{
    env,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{
    archive::ArchivePaths,
    database, hash,
    subtitles::{self, SubtitleFormat},
    transcriber,
};
use crate::env_config::{env_or, env_timeout, optional_env, required_env, resolve_cli};

/// Bump whenever the prompt text below changes in a way that would produce a
/// materially different summary. It is part of the `entry_summaries` cache key,
/// so a bump makes every stored summary regenerate on next request instead of
/// silently mixing outputs from two different prompts.
pub const PROMPT_VERSION: &str = "v1-2026-08-22";

/// Maximum number of explicitly opted-in local images supplied to a summary.
pub const MAX_SUMMARY_IMAGES: usize = 4;
/// Maximum byte size for one explicitly opted-in image.
pub const MAX_SUMMARY_IMAGE_BYTES: u64 = 5 * 1024 * 1024;
/// Maximum combined byte size for explicitly opted-in images.
pub const MAX_SUMMARY_IMAGE_TOTAL_BYTES: u64 = 12 * 1024 * 1024;

/// User-safe copy for entries whose archived artifacts do not contain
/// summarizable text. Keep this separate from provider and archive failures.
pub const UNSUPPORTED_SUMMARY_CONTENT_HEADING: &str = "This entry can’t be summarized yet.";
pub const UNSUPPORTED_SUMMARY_CONTENT_DETAIL: &str = "It doesn’t contain archived text that a summary provider can read. Summaries currently support text notes, web pages, X posts and threads, X Articles, and YouTube videos with subtitles. Other video, audio, and image-only entries need a transcript or text source.";
pub const UNSUPPORTED_SUMMARY_CONTENT_MESSAGE: &str = concat!(
    "This entry can’t be summarized yet.\n\n",
    "It doesn’t contain archived text that a summary provider can read. Summaries currently support text notes, web pages, X posts and threads, X Articles, and YouTube videos with subtitles. Other video, audio, and image-only entries need a transcript or text source."
);

/// User-safe copy for YouTube videos with no usable subtitle track, neither
/// archived nor downloadable on demand.
pub const NO_SUBTITLES_SUMMARY_MESSAGE: &str = "This video can’t be summarized because no subtitles are available. Archivr found no archived subtitles and couldn’t download any from the original video — it may have no captions, or it may be private, deleted, or unreachable.";
/// Placeholder `input_sha256` for a pending summary row whose input cannot be
/// digested until subtitles have been fetched in the background.
pub const SUBTITLE_FETCH_PENDING_INPUT_SHA256: &str = "pending-subtitle-fetch";
/// User-safe copy when local transcription ran but its transcript reduced to
/// no text (silence or music).
pub const NO_SUBTITLES_AFTER_TRANSCRIPTION_MESSAGE: &str = "This video can’t be summarized because no subtitles are available and local transcription found no speech in its audio.";

/// Upper bound on characters fed to a model. Archived pages run to hundreds of
/// kilobytes; past this point we are paying for tokens that do not change a
/// five-sentence summary. Truncation happens *before* hashing so the cache key
/// describes exactly what the model saw.
const MAX_INPUT_CHARS: usize = 48_000;

const DEFAULT_HTTP_TIMEOUT_SECS: u64 = 120;
const DEFAULT_CLI_TIMEOUT_SECS: u64 = 300;

#[derive(Debug)]
struct UnsupportedSummaryContent;

impl std::fmt::Display for UnsupportedSummaryContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("unsupported summary content")
    }
}

impl std::error::Error for UnsupportedSummaryContent {}

fn unsupported_summary_content_error() -> anyhow::Error {
    anyhow::Error::new(UnsupportedSummaryContent)
}

/// True only for expected, pre-provider summary-input limitations.
pub fn is_unsupported_summary_content_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<UnsupportedSummaryContent>().is_some())
}

#[derive(Debug)]
struct NoSubtitlesAvailable;

impl std::fmt::Display for NoSubtitlesAvailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("no subtitles available")
    }
}

impl std::error::Error for NoSubtitlesAvailable {}

fn no_subtitles_error() -> anyhow::Error {
    anyhow::Error::new(NoSubtitlesAvailable)
}

/// True when a YouTube video has no usable subtitle track to summarize.
pub fn is_no_subtitles_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<NoSubtitlesAvailable>().is_some())
}

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

/// Controls optional material included while building a summary request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SummaryBuildOptions {
    pub include_images: bool,
}

/// An archived image that was explicitly selected for a summary request.
///
/// `archive_file` is an absolute local path, kept inside the archive store and
/// never exposed through an API response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryImage {
    pub sha256: String,
    pub mime_type: String,
    pub byte_size: u64,
    pub archive_file: PathBuf,
}

/// Everything the prompt builder needs about one entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryRequest {
    pub entry_uid: String,
    pub title: Option<String>,
    pub source_kind: String,
    pub entity_kind: String,
    pub content: String,
    pub images: Vec<SummaryImage>,
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

/// Bound for summary-path subprocesses (`ARCHIVR_SUMMARY_CLI_TIMEOUT`).
pub(crate) fn summary_cli_timeout() -> std::time::Duration {
    std::time::Duration::from_secs(env_timeout(
        "ARCHIVR_SUMMARY_CLI_TIMEOUT",
        DEFAULT_CLI_TIMEOUT_SECS,
    ))
}

/// Builds a provider configuration for `kind` purely from the environment.
pub fn provider_from_env(kind: &str) -> Result<ProviderConfig> {
    match kind {
        "anthropic_http" => Ok(ProviderConfig::AnthropicHttp(HttpProviderConfig {
            endpoint: env_or(
                "ARCHIVR_ANTHROPIC_URL",
                "https://api.anthropic.com/v1/messages",
            ),
            api_key: required_env("ARCHIVR_ANTHROPIC_API_KEY")?,
            model: env_or("ARCHIVR_ANTHROPIC_MODEL", "claude-3-5-sonnet-latest"),
            timeout_secs: env_timeout("ARCHIVR_SUMMARY_HTTP_TIMEOUT", DEFAULT_HTTP_TIMEOUT_SECS),
        })),
        "openai_compatible" => Ok(ProviderConfig::OpenAiCompatible(HttpProviderConfig {
            endpoint: env_or(
                "ARCHIVR_OPENAI_URL",
                "https://api.openai.com/v1/chat/completions",
            ),
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
fn read_image_base64(image: &SummaryImage) -> Result<String> {
    let bytes = std::fs::read(&image.archive_file).with_context(|| {
        format!(
            "failed to read summary image {}",
            image.archive_file.display()
        )
    })?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

pub fn anthropic_request_body(model: &str, request: &SummaryRequest) -> Result<serde_json::Value> {
    if request.images.is_empty() {
        return Ok(serde_json::json!({
        "model": model,
        "max_tokens": 1024,
        "messages": [{
            "role": "user",
            "content": build_combined_prompt(request),
        }],
        }));
    }

    let mut content = vec![serde_json::json!({
        "type": "text",
        "text": build_combined_prompt(request),
    })];
    for image in &request.images {
        content.push(serde_json::json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": image.mime_type,
                "data": read_image_base64(image)?,
            },
        }));
    }
    Ok(serde_json::json!({
        "model": model,
        "max_tokens": 1024,
        "messages": [{ "role": "user", "content": content }],
    }))
}

pub fn openai_request_body(model: &str, request: &SummaryRequest) -> Result<serde_json::Value> {
    if request.images.is_empty() {
        return Ok(serde_json::json!({
        "model": model,
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT },
            { "role": "user", "content": build_user_prompt(request) },
        ],
        }));
    }

    let mut content = vec![serde_json::json!({
        "type": "text",
        "text": build_user_prompt(request),
    })];
    for image in &request.images {
        content.push(serde_json::json!({
            "type": "image_url",
            "image_url": {
                "url": format!("data:{};base64,{}", image.mime_type, read_image_base64(image)?),
            },
        }));
    }
    Ok(serde_json::json!({
        "model": model,
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT },
            { "role": "user", "content": content },
        ],
    }))
}

pub fn anthropic_plain_body(
    model: &str,
    system: &str,
    user: &str,
    max_tokens: u32,
) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "max_tokens": max_tokens,
        "system": system,
        "messages": [{ "role": "user", "content": user }],
    })
}

/// No `max_tokens`: newer OpenAI models reject it (mirrors `openai_request_body`).
pub fn openai_plain_body(model: &str, system: &str, user: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
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
        send_anthropic(&self.0, &anthropic_request_body(&self.0.model, request)?)
    }
}

fn send_anthropic(cfg: &HttpProviderConfig, body: &serde_json::Value) -> Result<SummaryOutput> {
    let resp = http_client(cfg.timeout_secs)?
        .post(&cfg.endpoint)
        .header("x-api-key", &cfg.api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .with_context(|| format!("request to {} failed", cfg.endpoint))?;
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    if !status.is_success() {
        bail!(
            "anthropic API returned {status}: {}",
            truncate_for_error(&text)
        );
    }
    parse_anthropic_response(&text)
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
        send_openai(&self.0, &openai_request_body(&self.0.model, request)?)
    }
}

fn send_openai(cfg: &HttpProviderConfig, body: &serde_json::Value) -> Result<SummaryOutput> {
    let resp = http_client(cfg.timeout_secs)?
        .post(&cfg.endpoint)
        .header("authorization", format!("Bearer {}", cfg.api_key))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .with_context(|| format!("request to {} failed", cfg.endpoint))?;
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
/// A thin adapter over [`crate::process::run_with_timeout`], which drains
/// stdout and stderr on their own threads, writes stdin on a third, and kills
/// the child once `timeout_secs` pass.
fn run_cli(executable: &Path, args: &[&str], prompt: &str, timeout_secs: u64) -> Result<String> {
    let args: Vec<std::ffi::OsString> = args.iter().map(std::ffi::OsString::from).collect();
    crate::process::run_with_timeout(
        executable,
        &args,
        Some(prompt),
        Duration::from_secs(timeout_secs),
    )
    .map(|output| output.stdout)
}

/// `claude -p --output-format text` is the documented one-shot ("print") mode
/// of the Claude Code CLI: it reads the prompt from stdin, writes the answer
/// to stdout, and exits.
fn claude_cli_args(model: Option<&str>) -> Vec<&str> {
    let mut args: Vec<&str> = vec!["-p", "--output-format", "text"];
    if let Some(model) = model {
        args.push("--model");
        args.push(model);
    }
    args
}

fn run_claude_cli(cfg: &CliProviderConfig, prompt: &str) -> Result<String> {
    run_cli(
        &cfg.executable,
        &claude_cli_args(cfg.model.as_deref()),
        prompt,
        cfg.timeout_secs,
    )
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
        if !request.images.is_empty() {
            bail!("Claude CLI cannot attach local images; choose an HTTP provider or Codex CLI");
        }
        Ok(SummaryOutput {
            text: run_claude_cli(&self.0, &build_combined_prompt(request))?,
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
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    pub fn primary_args(
        images: &[SummaryImage],
        out_path: &Path,
        model: Option<&str>,
    ) -> Vec<String> {
        let mut args = vec!["exec".into()];
        for image in images {
            args.push("--image".into());
            args.push(image.archive_file.to_string_lossy().into_owned());
        }
        args.push("--output-last-message".into());
        args.push(out_path.to_string_lossy().into_owned());
        if let Some(model) = model {
            args.push("--model".into());
            args.push(model.into());
        }
        args.push("-".into());
        args
    }

    pub fn positional_args(
        images: &[SummaryImage],
        out_path: &Path,
        model: Option<&str>,
        prompt: &str,
    ) -> Vec<String> {
        let mut args = primary_args(images, out_path, model);
        args.pop();
        args.push(prompt.into());
        args
    }

    pub fn run(cfg: &CliProviderConfig, prompt: &str, images: &[SummaryImage]) -> Result<String> {
        let out_path = last_message_temp_path();

        // Primary: stdin prompt + --output-last-message.
        let primary = primary_args(images, &out_path, cfg.model.as_deref());
        let primary_refs: Vec<&str> = primary.iter().map(String::as_str).collect();

        let primary_err = match run_cli(&cfg.executable, &primary_refs, prompt, cfg.timeout_secs) {
            Ok(_) => {
                if let Some(text) = read_and_cleanup(&out_path) {
                    return Ok(text);
                }
                // Codex succeeded but wrote nothing to the file — extremely
                // rare, but treat as a soft failure so we try the fallback.
                let _ = std::fs::remove_file(&out_path);
                anyhow!("codex produced no last-message output")
            }
            Err(e) => {
                let _ = std::fs::remove_file(&out_path);
                e
            }
        };

        // Fallback: positional prompt, no stdin, same --output-last-message.
        let fb = positional_args(images, &out_path, cfg.model.as_deref(), prompt);
        let fb_refs: Vec<&str> = fb.iter().map(String::as_str).collect();
        let out = run_cli(&cfg.executable, &fb_refs, "", cfg.timeout_secs).with_context(|| {
            let _ = std::fs::remove_file(&out_path);
            format!(
                "codex `exec -` failed ({primary_err:#}); positional fallback failed{}",
                missing_binary_hint(cfg)
            )
        })?;
        if let Some(text) = read_and_cleanup(&out_path) {
            return Ok(text);
        }
        // Last resort — the child succeeded but wrote nothing to the file. Fall
        // back to raw stdout so the caller has *something* to normalize.
        let _ = std::fs::remove_file(&out_path);
        Ok(out)
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
        let out = codex::run(&self.0, &build_combined_prompt(request), &request.images)?;
        Ok(SummaryOutput {
            text: out,
            model: self.0.model.clone(),
        })
    }
}

/// One-shot plain-text completion over the same transports as `summarize`,
/// for short non-summary prompts (thread titles). Never attaches images.
pub fn complete_plain(
    cfg: &ProviderConfig,
    system: &str,
    user: &str,
    max_tokens: u32,
) -> Result<SummaryOutput> {
    let combined = || format!("{system}\n\n---\n\n{user}");
    match cfg {
        ProviderConfig::AnthropicHttp(c) => {
            send_anthropic(c, &anthropic_plain_body(&c.model, system, user, max_tokens))
        }
        ProviderConfig::OpenAiCompatible(c) => {
            send_openai(c, &openai_plain_body(&c.model, system, user))
        }
        ProviderConfig::ClaudeCli(c) => Ok(SummaryOutput {
            text: run_claude_cli(c, &combined())?,
            model: c.model.clone(),
        }),
        ProviderConfig::CodexCli(c) => Ok(SummaryOutput {
            text: codex::run(c, &combined(), &[])?,
            model: c.model.clone(),
        }),
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
    let breaks =
        regex::Regex::new(r"(?i)</\s*(p|div|br|li|h[1-6]|tr|section|article)\s*>").unwrap();
    let tags = regex::Regex::new(r"(?s)<[^>]*>").unwrap();
    let spaces = regex::Regex::new(r"[ \t\r\f\v]+").unwrap();
    let blank_lines = regex::Regex::new(r"\n{3,}").unwrap();

    let s = drop_blocks.replace_all(html, " ");
    let s = comments.replace_all(&s, " ");
    let s = breaks.replace_all(&s, "\n");
    let s = tags.replace_all(&s, " ");
    let s = decode_entities(&s);
    let s = spaces.replace_all(&s, " ");
    let s = s.lines().map(str::trim).collect::<Vec<_>>().join("\n");
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

/// Loads an entry's primary text artifacts and reduces them to plain text.
///
/// Returns the unsupported-content error rather than a summary for artifact
/// kinds without text (video, audio, images): a clear "unsupported" beats an
/// empty or hallucinated summary.
pub(crate) fn artifact_text_content(
    conn: &rusqlite::Connection,
    store_path: &Path,
    entry_id: i64,
    entity_kind: &str,
) -> Result<String> {
    // Tweets and tweet threads use `raw_tweet_json` rather than `primary_media`,
    // and a THREAD is materialized as N separate JSON files (one per status).
    // Load every matching artifact in insertion order so a thread summarizes
    // as the whole conversation, not just its first status.
    let is_tweetish = matches!(entity_kind, "tweet" | "tweet_thread");
    let primary_role = if is_tweetish {
        "raw_tweet_json"
    } else {
        "primary_media"
    };
    let mut artifacts = database::list_entry_artifacts_by_role(conn, entry_id, primary_role)?;
    if artifacts.is_empty() && is_tweetish {
        // Older archives may have stored tweet payloads under `primary_media`.
        artifacts = database::list_entry_artifacts_by_role(conn, entry_id, "primary_media")?;
    }
    if artifacts.is_empty() {
        return Err(unsupported_summary_content_error());
    }

    let mut pieces: Vec<String> = Vec::with_capacity(artifacts.len());
    for artifact in &artifacts {
        let abs = store_path.join(&artifact.relpath);
        let ext = extension_of(&artifact.relpath);
        let mime = artifact.mime_type.as_deref().unwrap_or_default();

        let piece = if ext == "md"
            || ext == "markdown"
            || ext == "txt"
            || mime.starts_with("text/markdown")
            || mime == "text/plain"
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
            return Err(unsupported_summary_content_error());
        };
        if !piece.trim().is_empty() {
            pieces.push(piece);
        }
    }
    // Thread joiner: `---` on its own line reads as a paragraph break to both
    // humans and models. Single-piece entries never render the separator.
    let content = pieces.join("\n\n---\n\n").trim().to_string();
    if content.is_empty() {
        return Err(unsupported_summary_content_error());
    }
    Ok(content)
}

/// Picks the single best usable `subtitle` track of a YouTube video and
/// reduces it to a labelled transcript. Tracks are ordered by
/// [`subtitles::subtitle_track_rank`], ties broken by artifact id, so the
/// choice (and therefore the digest) is deterministic.
fn youtube_transcript_content(
    conn: &rusqlite::Connection,
    store_path: &Path,
    entry_id: i64,
) -> Result<String> {
    let mut tracks: Vec<(u8, i64, subtitles::SubtitleTrackMeta, String)> =
        database::list_entry_artifacts_by_role(conn, entry_id, subtitles::SUBTITLE_ARTIFACT_ROLE)?
            .into_iter()
            .filter(|artifact| {
                SubtitleFormat::detect(
                    &extension_of(&artifact.relpath),
                    artifact.mime_type.as_deref().unwrap_or_default(),
                )
                .is_some()
            })
            .map(|artifact| {
                let meta = subtitles::parse_subtitle_metadata(artifact.metadata_json.as_deref());
                (
                    subtitles::subtitle_track_rank(&meta),
                    artifact.id,
                    meta,
                    artifact.relpath,
                )
            })
            .collect();
    tracks.sort_by_key(|(rank, id, _, _)| (*rank, *id));

    for (_, _, meta, relpath) in tracks {
        let raw = match std::fs::read_to_string(store_path.join(&relpath)) {
            Ok(raw) => raw,
            Err(e) => {
                eprintln!("warn: summary subtitle {relpath}: {e:#}");
                continue;
            }
        };
        let transcript = subtitles::subtitle_to_transcript(&raw);
        if transcript.trim().is_empty() {
            continue;
        }
        let language = if meta.language.is_empty() {
            "unknown language"
        } else {
            meta.language.as_str()
        };
        return Ok(format!(
            "Transcript ({language}, {} subtitles):\n{}",
            meta.kind.as_str(),
            transcript.trim()
        ));
    }
    Err(no_subtitles_error())
}

fn matching_image_mime(extension: &str, mime_type: &str) -> bool {
    matches!(
        (extension, mime_type),
        ("jpg" | "jpeg", "image/jpeg")
            | ("png", "image/png")
            | ("webp", "image/webp")
            | ("gif", "image/gif")
            | ("avif", "image/avif")
    )
}

/// Loads media artifacts in their insertion order and keeps only bounded,
/// supported image files that resolve inside the archive store.
fn load_summary_image_candidates(
    conn: &rusqlite::Connection,
    store_path: &Path,
    entry_id: i64,
) -> Result<Vec<SummaryImage>> {
    let canonical_store = store_path.canonicalize().with_context(|| {
        format!(
            "failed to canonicalize store path: {}",
            store_path.display()
        )
    })?;
    let mut stmt = conn.prepare(
        "SELECT b.sha256, b.mime_type, b.extension, b.byte_size, ea.relpath
         FROM entry_artifacts ea
         JOIN blobs b ON b.id = ea.blob_id
         WHERE ea.entry_id = ?1 AND ea.artifact_role = 'media'
         ORDER BY ea.id ASC",
    )?;
    let rows = stmt
        .query_map([entry_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut images = Vec::new();
    let mut total_bytes = 0_u64;
    for (sha256, mime_type, extension, byte_size, relpath) in rows {
        let Some(mime_type) = mime_type else {
            continue;
        };
        let Some(extension) = extension else {
            continue;
        };
        let extension = extension.to_ascii_lowercase();
        let mime_type = mime_type.to_ascii_lowercase();
        let Ok(byte_size) = u64::try_from(byte_size) else {
            continue;
        };
        if !matching_image_mime(&extension, &mime_type)
            || byte_size > MAX_SUMMARY_IMAGE_BYTES
            || images.len() >= MAX_SUMMARY_IMAGES
            || total_bytes.saturating_add(byte_size) > MAX_SUMMARY_IMAGE_TOTAL_BYTES
        {
            continue;
        }

        let archive_file = match store_path.join(relpath).canonicalize() {
            Ok(path) if path.starts_with(&canonical_store) => path,
            _ => continue,
        };
        total_bytes += byte_size;
        images.push(SummaryImage {
            sha256,
            mime_type,
            byte_size,
            archive_file,
        });
    }
    Ok(images)
}

fn summary_input_digest(content: &str, include_images: bool, images: &[SummaryImage]) -> String {
    let mut preimage = Vec::with_capacity(content.len() + 32 + images.len() * 128);
    preimage.extend_from_slice(content.as_bytes());
    preimage.extend_from_slice(b"\0images=");
    preimage.extend_from_slice(if include_images { b"1" } else { b"0" });
    for image in images {
        preimage.push(0);
        preimage.extend_from_slice(image.sha256.as_bytes());
        preimage.push(0);
        preimage.extend_from_slice(image.mime_type.as_bytes());
        preimage.push(0);
        preimage.extend_from_slice(image.byte_size.to_string().as_bytes());
    }
    hash::hash_bytes(&preimage)
}

pub fn build_summary_input(
    paths: &ArchivePaths,
    entry_uid: &str,
    options: SummaryBuildOptions,
) -> Result<SummaryInput> {
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

    let content = if source_kind == "youtube" && entity_kind == "video" {
        youtube_transcript_content(&conn, &paths.store_path, entry_id)?
    } else {
        artifact_text_content(&conn, &paths.store_path, entry_id, &entity_kind)?
    };
    // Truncate on a char boundary, then hash: the digest must describe the
    // bytes actually sent, or the cache would key on content the model never saw.
    let content: String = if content.chars().count() > MAX_INPUT_CHARS {
        content.chars().take(MAX_INPUT_CHARS).collect()
    } else {
        content
    };
    let images = if options.include_images {
        load_summary_image_candidates(&conn, &paths.store_path, entry_id)?
    } else {
        Vec::new()
    };
    let input_sha256 = summary_input_digest(&content, options.include_images, &images);

    Ok(SummaryInput {
        request: SummaryRequest {
            entry_uid: entry_uid.to_string(),
            title,
            source_kind,
            entity_kind,
            content,
            images,
        },
        input_sha256,
    })
}

/// Builds the summary input for a YouTube video that had no usable subtitles
/// at preflight, in this order: subtitles fetched from the original video
/// (a no-op for other entries or when a usable track is already archived),
/// then — only if that still leaves none and `transcription` is given — a
/// local transcription of the audio.
///
/// Without `transcription` this still returns the no-subtitles error when
/// nothing usable could be fetched. With it, failures carry a
/// [`transcriber::TranscriptionUserMessage`].
pub fn build_summary_input_with_subtitle_fetch(
    paths: &ArchivePaths,
    entry_uid: &str,
    options: SummaryBuildOptions,
    cookie_rules: &[database::CookieRule],
    transcription: Option<&transcriber::TranscriptionRequest>,
) -> Result<SummaryInput> {
    let outcome = subtitles::fetch_subtitles_for_entry(paths, entry_uid, cookie_rules)?;
    eprintln!(
        "info: summary {entry_uid}: subtitle fetch added {} artifact(s)",
        outcome.added
    );
    let request = match build_summary_input(paths, entry_uid, options) {
        Ok(input) => return Ok(input),
        Err(e) if is_no_subtitles_error(&e) => match transcription {
            Some(request) => request,
            None => return Err(e),
        },
        Err(e) => return Err(e),
    };

    transcriber::transcribe_entry(
        paths,
        entry_uid,
        request,
        outcome.original_language.as_deref(),
        cookie_rules,
    )?;
    match build_summary_input(paths, entry_uid, options) {
        Err(e) if is_no_subtitles_error(&e) => Err(e.context(transcriber::TranscriptionUserMessage(
            NO_SUBTITLES_AFTER_TRANSCRIPTION_MESSAGE.to_string(),
        ))),
        other => other,
    }
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
/// input_sha256)` cache key identifies equivalent requests. Each generation is
/// nevertheless recorded as a distinct attempt, so a forced regeneration cannot
/// hide a prior completed result while the new attempt is pending or running.
pub fn summarize_entry(
    archive_paths: &ArchivePaths,
    entry_uid: &str,
    options: SummaryBuildOptions,
    provider: &dyn SummaryProvider,
    prompt_version: &str,
) -> Result<database::EntrySummaryRecord> {
    let conn = database::open_or_initialize(&archive_paths.archive_path)?;
    let entry_id = database::entry_id_for_uid(&conn, entry_uid)?
        .ok_or_else(|| anyhow!("entry not found: {entry_uid}"))?;

    let input = build_summary_input(archive_paths, entry_uid, options)?;
    let summary_uid = database::upsert_pending_entry_summary(
        &conn,
        entry_id,
        provider.kind(),
        provider.model(),
        prompt_version,
        &input.input_sha256,
    )?;
    summarize_prebuilt_entry(archive_paths, input, &summary_uid, provider)
}

/// Runs a previously validated and claimed summary attempt.
///
/// The server uses this after doing its preflight in a blocking task, avoiding
/// a second filesystem/SQLite extraction and ensuring provider output updates
/// the exact pending row returned to the caller.
pub fn summarize_prebuilt_entry(
    archive_paths: &ArchivePaths,
    input: SummaryInput,
    summary_uid: &str,
    provider: &dyn SummaryProvider,
) -> Result<database::EntrySummaryRecord> {
    let conn = database::open_or_initialize(&archive_paths.archive_path)?;
    database::update_entry_summary_status(&conn, summary_uid, "running", None, None)?;

    match provider.summarize(&input.request) {
        Ok(output) => {
            let text = normalize_summary_json(&output.text);
            database::update_entry_summary_completed(
                &conn,
                summary_uid,
                &text,
                output.model.as_deref(),
            )?;
        }
        Err(e) => {
            let msg = format!("{e:#}");
            database::update_entry_summary_status(&conn, summary_uid, "failed", None, Some(&msg))?;
            return Err(e);
        }
    }

    database::get_entry_summary_by_uid(&conn, summary_uid)?
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
            images: Vec::new(),
        }
    }

    fn image_request() -> (tempfile::TempDir, SummaryRequest) {
        let temp = tempfile::tempdir().unwrap();
        let image_file = temp.path().join("fixture.png");
        std::fs::write(&image_file, [0_u8, 1, 2, 3]).unwrap();
        let mut request = sample_request();
        request.images.push(SummaryImage {
            sha256: "fixture-sha".into(),
            mime_type: "image/png".into(),
            byte_size: 4,
            archive_file: image_file,
        });
        (temp, request)
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
        let err = provider_from_env("openai_compatible")
            .unwrap_err()
            .to_string();
        assert!(err.contains("ARCHIVR_OPENAI_API_KEY"), "got: {err}");
    }

    #[test]
    fn provider_from_env_openai_honours_overrides() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_provider_env();
        unsafe {
            env::set_var("ARCHIVR_OPENAI_API_KEY", "k");
            env::set_var(
                "ARCHIVR_OPENAI_URL",
                "http://localhost:1234/v1/chat/completions",
            );
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
                || c.executable
                    .file_name()
                    .map(|f| f == "claude")
                    .unwrap_or(false),
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
                || c.executable
                    .file_name()
                    .map(|f| f == "codex")
                    .unwrap_or(false),
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
        let body = anthropic_request_body("claude-3-5-sonnet-latest", &sample_request()).unwrap();
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
        let body = openai_request_body("gpt-4o-mini", &sample_request()).unwrap();
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
    fn anthropic_request_body_attaches_base64_images() {
        let (_temp, request) = image_request();
        let body = anthropic_request_body("claude", &request).unwrap();
        let content = body["messages"][0]["content"].as_array().unwrap();
        assert!(content[0]["text"].as_str().unwrap().contains("Body text."));
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["type"], "base64");
        assert_eq!(content[1]["source"]["media_type"], "image/png");
        assert_eq!(content[1]["source"]["data"], "AAECAw==");
    }

    #[test]
    fn openai_request_body_attaches_data_url_images() {
        let (_temp, request) = image_request();
        let body = openai_request_body("gpt", &request).unwrap();
        let content = body["messages"][1]["content"].as_array().unwrap();
        assert!(content[0]["text"].as_str().unwrap().contains("Body text."));
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(
            content[1]["image_url"]["url"],
            "data:image/png;base64,AAECAw=="
        );
    }

    #[test]
    fn codex_primary_arguments_put_images_before_output_path() {
        let (_temp, request) = image_request();
        let args = codex::primary_args(&request.images, Path::new("/tmp/output"), None);
        assert_eq!(args[0], "exec");
        let image_at = args.iter().position(|arg| arg == "--image").unwrap();
        let output_at = args
            .iter()
            .position(|arg| arg == "--output-last-message")
            .unwrap();
        assert!(image_at < output_at);
        assert_eq!(
            args[image_at + 1],
            request.images[0].archive_file.to_string_lossy()
        );
        assert_eq!(args.last().unwrap(), "-");
    }

    #[test]
    fn codex_positional_arguments_put_images_before_output_path() {
        let (_temp, request) = image_request();
        let args =
            codex::positional_args(&request.images, Path::new("/tmp/output"), None, "prompt");
        let image_at = args.iter().position(|arg| arg == "--image").unwrap();
        let output_at = args
            .iter()
            .position(|arg| arg == "--output-last-message")
            .unwrap();
        assert!(image_at < output_at);
        assert_eq!(
            args[image_at + 1],
            request.images[0].archive_file.to_string_lossy()
        );
        assert_eq!(args.last().unwrap(), "prompt");
    }

    #[cfg(unix)]
    #[test]
    fn codex_positional_fallback_honors_cli_timeout() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("codex-fixture.sh");
        std::fs::write(
            &executable,
            "#!/bin/sh\nfor arg in \"$@\"; do [ \"$arg\" = \"-\" ] && exit 1; done\nsleep 30\n",
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cfg = CliProviderConfig {
            executable,
            model: None,
            timeout_secs: 1,
        };

        let started = std::time::Instant::now();
        let err = format!("{:#}", codex::run(&cfg, "prompt", &[]).unwrap_err());

        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(err.contains("timed out after 1s"), "got: {err}");
    }

    #[test]
    fn claude_rejects_images_before_spawning() {
        let (_temp, request) = image_request();
        let provider = ClaudeCliProvider(CliProviderConfig {
            executable: PathBuf::from("definitely-not-a-claude-binary"),
            model: None,
            timeout_secs: 1,
        });
        let err = provider.summarize(&request).unwrap_err().to_string();
        assert!(
            err.contains("Claude CLI cannot attach local images"),
            "got: {err}"
        );
    }

    #[test]
    fn parse_anthropic_response_extracts_text_and_model() {
        let body =
            r#"{"model":"claude-3-5-sonnet-20241022","content":[{"type":"text","text":"hi"}]}"#;
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
        assert_eq!(
            strip_html("<p>a &amp; b &nbsp;c</p>").replace('\u{a0}', " "),
            "a & b c"
        );
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

        let input =
            build_summary_input(&paths, &entry.entry_uid, SummaryBuildOptions::default()).unwrap();
        assert!(
            input
                .request
                .content
                .contains("First article body.\n\n---\n\nArticle 2\n\nSecond article body.")
        );
    }

    fn summary_image_fixture() -> (tempfile::TempDir, ArchivePaths, database::ArchivedEntry) {
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
            "web",
            "page",
            Some("summary-image-test"),
            Some("https://example.test/summary-image-test"),
            "https://example.test/summary-image-test",
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
                source_kind: "web".to_string(),
                entity_kind: "page".to_string(),
                title: Some("Image test".to_string()),
                visibility: "private".to_string(),
                representation_kind: "webpage".to_string(),
                source_metadata_json: "{}".to_string(),
                display_metadata_json: None,
            },
        )
        .unwrap();

        let text_relpath = "raw/summary-image-test.txt";
        std::fs::write(paths.store_path.join(text_relpath), "Summary source text.").unwrap();
        database::add_entry_artifact(
            &conn,
            &database::NewArtifact {
                entry_id: entry.id,
                artifact_role: "primary_media".to_string(),
                storage_area: "raw".to_string(),
                relpath: text_relpath.to_string(),
                blob_id: None,
                logical_path: None,
                metadata_json: None,
            },
        )
        .unwrap();
        (temp, paths, entry)
    }

    #[test]
    fn unsupported_summary_content_errors_are_classified_without_relabeling_other_errors() {
        let (_temp, paths, entry) = summary_image_fixture();
        let conn = database::open_or_initialize(&paths.archive_path).unwrap();
        conn.execute(
            "DELETE FROM entry_artifacts WHERE entry_id = ?1",
            [entry.id],
        )
        .unwrap();

        let no_artifact =
            build_summary_input(&paths, &entry.entry_uid, SummaryBuildOptions::default())
                .unwrap_err();
        assert!(is_unsupported_summary_content_error(&no_artifact));

        add_summary_image_artifact(&paths, entry.id, 99, "primary_media", "mp4", "video/mp4", 1);
        let video = build_summary_input(&paths, &entry.entry_uid, SummaryBuildOptions::default())
            .unwrap_err();
        assert!(is_unsupported_summary_content_error(&video));

        let conn = database::open_or_initialize(&paths.archive_path).unwrap();
        conn.execute(
            "DELETE FROM entry_artifacts WHERE entry_id = ?1",
            [entry.id],
        )
        .unwrap();
        let empty_relpath = "raw/empty-summary.txt";
        std::fs::write(paths.store_path.join(empty_relpath), "").unwrap();
        database::add_entry_artifact(
            &conn,
            &database::NewArtifact {
                entry_id: entry.id,
                artifact_role: "primary_media".to_string(),
                storage_area: "raw".to_string(),
                relpath: empty_relpath.to_string(),
                blob_id: None,
                logical_path: None,
                metadata_json: None,
            },
        )
        .unwrap();
        drop(conn);
        let empty_text =
            build_summary_input(&paths, &entry.entry_uid, SummaryBuildOptions::default())
                .unwrap_err();
        assert!(is_unsupported_summary_content_error(&empty_text));

        std::fs::remove_file(paths.store_path.join(empty_relpath)).unwrap();
        let read_error =
            build_summary_input(&paths, &entry.entry_uid, SummaryBuildOptions::default())
                .unwrap_err();
        assert!(!is_unsupported_summary_content_error(&read_error));
        assert_eq!(
            UNSUPPORTED_SUMMARY_CONTENT_MESSAGE,
            "This entry can’t be summarized yet.\n\nIt doesn’t contain archived text that a summary provider can read. Summaries currently support text notes, web pages, X posts and threads, X Articles, and YouTube videos with subtitles. Other video, audio, and image-only entries need a transcript or text source."
        );

        assert!(!is_unsupported_summary_content_error(&anyhow!(
            "provider timeout"
        )));
        assert!(!is_unsupported_summary_content_error(&anyhow!(
            "entry not found: {}",
            entry.entry_uid
        )));
    }

    fn video_summary_fixture(
        source_kind: &str,
    ) -> (tempfile::TempDir, ArchivePaths, String, i64) {
        video_summary_fixture_with_url(
            source_kind,
            &format!("https://{source_kind}.example/watch?v=abc123"),
        )
    }

    fn video_summary_fixture_with_url(
        source_kind: &str,
        url: &str,
    ) -> (tempfile::TempDir, ArchivePaths, String, i64) {
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
            source_kind,
            "video",
            Some("abc123"),
            Some(url),
            &format!("{source_kind}:abc123"),
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
                source_kind: source_kind.to_string(),
                entity_kind: "video".to_string(),
                title: Some("A video".to_string()),
                visibility: "private".to_string(),
                representation_kind: "video".to_string(),
                source_metadata_json: "{}".to_string(),
                display_metadata_json: None,
            },
        )
        .unwrap();
        drop(conn);
        add_summary_image_artifact(&paths, entry.id, 0, "primary_media", "mp4", "video/mp4", 1);
        (temp, paths, entry.entry_uid, entry.id)
    }

    fn youtube_summary_fixture() -> (tempfile::TempDir, ArchivePaths, String, i64) {
        video_summary_fixture("youtube")
    }

    /// YouTube video whose canonical URL is not http(s), so the on-demand
    /// subtitle fetch returns without spawning yt-dlp.
    fn youtube_offline_summary_fixture() -> (tempfile::TempDir, ArchivePaths, String, i64) {
        video_summary_fixture_with_url("youtube", "youtube-test:offline")
    }

    fn add_subtitle_artifact(
        paths: &ArchivePaths,
        entry_id: i64,
        name: &str,
        body: &str,
        language: &str,
        kind: &str,
    ) {
        let relpath = format!("raw/{name}");
        std::fs::write(paths.store_path.join(&relpath), body).unwrap();
        let format = if name.ends_with(".srt") { "srt" } else { "vtt" };
        let conn = database::open_or_initialize(&paths.archive_path).unwrap();
        database::add_entry_artifact(
            &conn,
            &database::NewArtifact {
                entry_id,
                artifact_role: subtitles::SUBTITLE_ARTIFACT_ROLE.to_string(),
                storage_area: "raw".to_string(),
                relpath,
                blob_id: None,
                logical_path: None,
                metadata_json: Some(
                    serde_json::json!({
                        "language": language,
                        "kind": kind,
                        "format": format,
                        "original_language": "en",
                        "origin": "capture",
                    })
                    .to_string(),
                ),
            },
        )
        .unwrap();
    }

    const AUTO_EN_VTT: &str = "WEBVTT\nKind: captions\nLanguage: en\n\n00:00:00.000 --> 00:00:02.000 align:start position:0%\nauto caption words\n \n\n00:00:02.000 --> 00:00:04.000\nmore auto words\n";
    const MANUAL_EN_SRT: &str =
        "1\n00:00:00,000 --> 00:00:02,000\nManual line one.\n\n2\n00:00:02,000 --> 00:00:04,000\nManual line two.\n";

    #[test]
    fn youtube_summary_uses_best_subtitle_track_transcript() {
        let (_temp, paths, entry_uid, entry_id) = youtube_summary_fixture();
        add_subtitle_artifact(&paths, entry_id, "auto.en.vtt", AUTO_EN_VTT, "en", "auto");
        add_subtitle_artifact(&paths, entry_id, "manual.en.srt", MANUAL_EN_SRT, "en", "manual");

        let input =
            build_summary_input(&paths, &entry_uid, SummaryBuildOptions::default()).unwrap();
        let content = &input.request.content;
        assert!(
            content.starts_with("Transcript (en, manual subtitles):"),
            "got: {content}"
        );
        assert!(content.contains("Manual line one."));
        assert!(content.contains("Manual line two."));
        assert!(!content.contains("auto caption"));
        assert!(!content.contains("-->"));
    }

    #[test]
    fn youtube_summary_skips_unusable_tracks_and_labels_unknown_language() {
        let (_temp, paths, entry_uid, entry_id) = youtube_summary_fixture();
        // Better-ranked but empty manual track must be skipped.
        add_subtitle_artifact(&paths, entry_id, "empty.en.srt", "", "en", "manual");
        add_subtitle_artifact(&paths, entry_id, "auto.vtt", AUTO_EN_VTT, "", "auto");

        let input =
            build_summary_input(&paths, &entry_uid, SummaryBuildOptions::default()).unwrap();
        assert!(
            input
                .request
                .content
                .starts_with("Transcript (unknown language, auto subtitles):\nauto caption words")
        );
    }

    #[test]
    fn youtube_summary_digest_changes_when_subtitles_added() {
        let (_temp, paths, entry_uid, entry_id) = youtube_summary_fixture();
        add_subtitle_artifact(&paths, entry_id, "auto.en.vtt", AUTO_EN_VTT, "en", "auto");
        let sha1 = build_summary_input(&paths, &entry_uid, SummaryBuildOptions::default())
            .unwrap()
            .input_sha256;
        add_subtitle_artifact(&paths, entry_id, "manual.en.srt", MANUAL_EN_SRT, "en", "manual");
        let sha2 = build_summary_input(&paths, &entry_uid, SummaryBuildOptions::default())
            .unwrap()
            .input_sha256;
        assert_ne!(sha1, sha2);
    }

    #[test]
    fn youtube_summary_without_subtitles_is_no_subtitles_error() {
        let (_temp, paths, entry_uid, _entry_id) = youtube_summary_fixture();
        let err = build_summary_input(&paths, &entry_uid, SummaryBuildOptions::default())
            .unwrap_err();
        assert!(is_no_subtitles_error(&err));
        assert!(!is_unsupported_summary_content_error(&err));
    }

    #[test]
    fn non_youtube_video_still_unsupported() {
        let (_temp, paths, entry_uid, entry_id) = video_summary_fixture("tiktok");
        // Subtitle artifacts are ignored outside YouTube videos.
        add_subtitle_artifact(&paths, entry_id, "manual.en.srt", MANUAL_EN_SRT, "en", "manual");
        let err = build_summary_input(&paths, &entry_uid, SummaryBuildOptions::default())
            .unwrap_err();
        assert!(is_unsupported_summary_content_error(&err));
        assert!(!is_no_subtitles_error(&err));
    }

    #[test]
    fn no_subtitles_error_classification() {
        assert!(is_no_subtitles_error(&no_subtitles_error()));
        assert!(is_no_subtitles_error(
            &no_subtitles_error().context("while building summary input")
        ));
        assert_eq!(no_subtitles_error().to_string(), "no subtitles available");
        assert!(!is_no_subtitles_error(&unsupported_summary_content_error()));
        assert!(!is_no_subtitles_error(&anyhow!("provider timeout")));
        assert!(!is_unsupported_summary_content_error(&no_subtitles_error()));
    }

    fn add_summary_image_artifact(
        paths: &ArchivePaths,
        entry_id: i64,
        ordinal: usize,
        role: &str,
        extension: &str,
        mime_type: &str,
        byte_size: u64,
    ) {
        let conn = database::open_or_initialize(&paths.archive_path).unwrap();
        let relpath = format!("raw/summary-image-{ordinal}.{extension}");
        std::fs::write(paths.store_path.join(&relpath), "image fixture").unwrap();
        let blob_id = database::upsert_blob(
            &conn,
            &database::BlobRecord {
                sha256: format!("summary-image-{ordinal:02}"),
                byte_size: byte_size.try_into().unwrap(),
                mime_type: Some(mime_type.to_string()),
                extension: Some(extension.to_string()),
                raw_relpath: relpath.clone(),
            },
        )
        .unwrap();
        database::add_entry_artifact(
            &conn,
            &database::NewArtifact {
                entry_id,
                artifact_role: role.to_string(),
                storage_area: "raw".to_string(),
                relpath,
                blob_id: Some(blob_id),
                logical_path: None,
                metadata_json: None,
            },
        )
        .unwrap();
    }

    #[test]
    fn summary_image_selection_filters_candidates_and_stops_at_image_count() {
        let (_temp, paths, entry) = summary_image_fixture();
        add_summary_image_artifact(&paths, entry.id, 0, "avatar", "jpg", "image/jpeg", 1);
        add_summary_image_artifact(&paths, entry.id, 1, "video", "mp4", "video/mp4", 1);
        add_summary_image_artifact(&paths, entry.id, 2, "audio", "mp3", "audio/mpeg", 1);
        add_summary_image_artifact(&paths, entry.id, 3, "media", "svg", "image/svg+xml", 1);
        add_summary_image_artifact(&paths, entry.id, 4, "media", "png", "image/jpeg", 1);
        add_summary_image_artifact(
            &paths,
            entry.id,
            5,
            "media",
            "jpg",
            "image/jpeg",
            MAX_SUMMARY_IMAGE_BYTES + 1,
        );
        add_summary_image_artifact(&paths, entry.id, 6, "media", "jpg", "image/jpeg", 1);
        add_summary_image_artifact(&paths, entry.id, 7, "media", "png", "image/png", 2);
        add_summary_image_artifact(&paths, entry.id, 8, "media", "webp", "image/webp", 3);
        add_summary_image_artifact(&paths, entry.id, 9, "media", "gif", "image/gif", 4);
        add_summary_image_artifact(&paths, entry.id, 10, "media", "avif", "image/avif", 5);

        let text_only = build_summary_input(
            &paths,
            &entry.entry_uid,
            SummaryBuildOptions {
                include_images: false,
            },
        )
        .unwrap();
        let visual = build_summary_input(
            &paths,
            &entry.entry_uid,
            SummaryBuildOptions {
                include_images: true,
            },
        )
        .unwrap();

        assert!(text_only.request.images.is_empty());
        assert_ne!(text_only.input_sha256, visual.input_sha256);
        assert_eq!(visual.request.images.len(), MAX_SUMMARY_IMAGES);
        assert_eq!(
            visual
                .request
                .images
                .iter()
                .map(|image| image.sha256.as_str())
                .collect::<Vec<_>>(),
            vec![
                "summary-image-06",
                "summary-image-07",
                "summary-image-08",
                "summary-image-09",
            ]
        );
        assert!(
            visual
                .request
                .images
                .iter()
                .all(|image| image.byte_size <= MAX_SUMMARY_IMAGE_BYTES)
        );
    }

    #[test]
    fn summary_image_selection_enforces_aggregate_limit_and_keeps_scanning() {
        let (_temp, paths, entry) = summary_image_fixture();
        add_summary_image_artifact(
            &paths,
            entry.id,
            0,
            "media",
            "jpg",
            "image/jpeg",
            4 * 1024 * 1024,
        );
        add_summary_image_artifact(
            &paths,
            entry.id,
            1,
            "media",
            "png",
            "image/png",
            4 * 1024 * 1024,
        );
        add_summary_image_artifact(
            &paths,
            entry.id,
            2,
            "media",
            "webp",
            "image/webp",
            4 * 1024 * 1024,
        );
        add_summary_image_artifact(&paths, entry.id, 3, "media", "gif", "image/gif", 1);

        let visual = build_summary_input(
            &paths,
            &entry.entry_uid,
            SummaryBuildOptions {
                include_images: true,
            },
        )
        .unwrap();

        assert_eq!(visual.request.images.len(), 3);
        assert_eq!(
            visual
                .request
                .images
                .iter()
                .map(|image| image.byte_size)
                .sum::<u64>(),
            MAX_SUMMARY_IMAGE_TOTAL_BYTES
        );
        assert!(
            visual
                .request
                .images
                .iter()
                .all(|image| image.sha256 != "summary-image-03")
        );
    }

    #[test]
    fn summarize_entry_uses_requested_alias_for_cache_and_response_model_for_display() {
        struct ResolvedModelProvider;

        impl SummaryProvider for ResolvedModelProvider {
            fn kind(&self) -> &'static str {
                "anthropic_http"
            }
            fn model(&self) -> Option<&str> {
                Some("claude-3-5-sonnet-latest")
            }
            fn summarize(&self, _: &SummaryRequest) -> Result<SummaryOutput> {
                Ok(SummaryOutput {
                    text: r#"{"tldr":"t","summary":"s","tags":[]}"#.to_string(),
                    model: Some("claude-3-5-sonnet-20241022".to_string()),
                })
            }
        }

        let (_temp, paths, entry) = summary_image_fixture();
        let record = summarize_entry(
            &paths,
            &entry.entry_uid,
            SummaryBuildOptions::default(),
            &ResolvedModelProvider,
            "v1",
        )
        .unwrap();

        assert_eq!(
            record.provider_model.as_deref(),
            Some("claude-3-5-sonnet-latest")
        );
        assert_eq!(
            record.resolved_model.as_deref(),
            Some("claude-3-5-sonnet-20241022")
        );
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
        let raw =
            "Sure! Here you go:\n{\"tldr\":\"t\",\"summary\":\"s\",\"tags\":[]}\nHope that helps.";
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
        let err = run_cli(Path::new("sleep"), &["30"], "", 1)
            .unwrap_err()
            .to_string();
        assert!(err.contains("timed out"), "got: {err}");
    }

    #[test]
    fn run_cli_reports_a_nonzero_exit() {
        let err = run_cli(Path::new("false"), &[], "", 30)
            .unwrap_err()
            .to_string();
        assert!(err.contains("exited with"), "got: {err}");
    }

    #[test]
    fn anthropic_plain_body_uses_system_field_and_max_tokens() {
        let body = anthropic_plain_body("claude-haiku-4-5", "sys prompt", "user text", 64);
        assert_eq!(body["model"], "claude-haiku-4-5");
        assert_eq!(body["system"], "sys prompt");
        assert_eq!(body["max_tokens"], 64);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "user text");
        assert!(!body.to_string().contains(SYSTEM_PROMPT.trim()));
    }

    #[test]
    fn openai_plain_body_has_system_then_user() {
        let body = openai_plain_body("gpt-4o-mini", "sys prompt", "user text");
        assert_eq!(body["model"], "gpt-4o-mini");
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "sys prompt");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "user text");
        assert!(body.get("max_tokens").is_none());
    }

    #[test]
    fn claude_cli_args_append_model_only_when_set() {
        assert_eq!(claude_cli_args(None), vec!["-p", "--output-format", "text"]);
        assert_eq!(
            claude_cli_args(Some("haiku")),
            vec!["-p", "--output-format", "text", "--model", "haiku"]
        );
    }

    // ── Local transcription fallback ───────────────────────────────────────

    /// In-process engine: writes `vtt` as the transcript and counts calls.
    struct FakeTranscriber {
        kind: &'static str,
        label: &'static str,
        vtt: &'static str,
        languages: Option<Vec<String>>,
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl transcriber::Transcriber for FakeTranscriber {
        fn kind(&self) -> &'static str {
            self.kind
        }
        fn label(&self) -> &'static str {
            self.label
        }
        fn model(&self) -> &str {
            "fake-model"
        }
        fn timeout_secs(&self) -> u64 {
            30
        }
        fn supports_language(&self, original_language: Option<&str>) -> bool {
            match (&self.languages, original_language) {
                (Some(allowed), Some(lang)) => {
                    allowed.contains(&crate::downloader::ytdlp::language_base(lang))
                }
                _ => true,
            }
        }
        fn supported_languages(&self) -> Option<&[String]> {
            self.languages.as_deref()
        }
        fn transcribe(
            &self,
            _audio_wav: &Path,
            _lang_hint: Option<&str>,
            out_dir: &Path,
            _deadline: std::time::Instant,
        ) -> Result<transcriber::TranscriptOutput> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let vtt_path = out_dir.join("transcript.vtt");
            std::fs::write(&vtt_path, self.vtt)?;
            Ok(transcriber::TranscriptOutput {
                vtt_path,
                language: Some("en".to_string()),
            })
        }
    }

    const FAKE_TRANSCRIPT_VTT: &str =
        "WEBVTT\n\n00:00:00.000 --> 00:00:02.000\nlocally transcribed words\n";

    fn fake_transcription(
        bin_dir: &Path,
        kind: &'static str,
        label: &'static str,
        languages: Option<Vec<String>>,
    ) -> (
        transcriber::TranscriptionRequest,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ffmpeg = transcriber::test_support::write_stub_script(
            bin_dir,
            "ffmpeg",
            transcriber::test_support::STUB_FFMPEG,
        );
        let request = transcriber::TranscriptionRequest {
            transcriber: Box::new(FakeTranscriber {
                kind,
                label,
                vtt: FAKE_TRANSCRIPT_VTT,
                languages,
                calls: calls.clone(),
            }),
            settings: transcriber::TranscriptionSettings { ffmpeg },
        };
        (request, calls)
    }

    fn transcription_lock() -> std::sync::MutexGuard<'static, ()> {
        transcriber::TRANSCRIBE_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    #[cfg(unix)]
    #[test]
    fn subtitle_fetch_with_transcriber_uses_transcribed_track() {
        let _lock = transcription_lock();
        let (temp, paths, entry_uid, _entry_id) = youtube_offline_summary_fixture();
        let (request, calls) = fake_transcription(temp.path(), "whisper", "Whisper", None);
        let input = build_summary_input_with_subtitle_fetch(
            &paths,
            &entry_uid,
            SummaryBuildOptions::default(),
            &[],
            Some(&request),
        )
        .unwrap();
        let content = &input.request.content;
        assert!(
            content.starts_with("Transcript (en, transcribed subtitles):"),
            "got: {content}"
        );
        assert!(content.contains("locally transcribed words"));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_ne!(input.input_sha256, SUBTITLE_FETCH_PENDING_INPUT_SHA256);
    }

    #[test]
    fn subtitle_fetch_without_transcriber_keeps_no_subtitles_error() {
        let (_temp, paths, entry_uid, _entry_id) = youtube_offline_summary_fixture();
        let err = build_summary_input_with_subtitle_fetch(
            &paths,
            &entry_uid,
            SummaryBuildOptions::default(),
            &[],
            None,
        )
        .unwrap_err();
        assert!(is_no_subtitles_error(&err));
        assert_eq!(transcriber::transcription_user_message(&err), None);
    }

    #[cfg(unix)]
    #[test]
    fn transcriber_not_called_when_usable_subtitles_exist() {
        let _lock = transcription_lock();
        let (temp, paths, entry_uid, entry_id) = youtube_offline_summary_fixture();
        add_subtitle_artifact(&paths, entry_id, "auto.en.vtt", AUTO_EN_VTT, "en", "auto");
        let (request, calls) = fake_transcription(temp.path(), "whisper", "Whisper", None);
        let input = build_summary_input_with_subtitle_fetch(
            &paths,
            &entry_uid,
            SummaryBuildOptions::default(),
            &[],
            Some(&request),
        )
        .unwrap();
        assert!(input.request.content.starts_with("Transcript (en, auto subtitles):"));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[cfg(unix)]
    #[test]
    fn youtube_summary_digest_changes_when_transcript_added() {
        let _lock = transcription_lock();
        let (temp, paths, entry_uid, entry_id) = youtube_offline_summary_fixture();
        let (request, _calls) = fake_transcription(temp.path(), "whisper", "Whisper", None);
        let transcribed = build_summary_input_with_subtitle_fetch(
            &paths,
            &entry_uid,
            SummaryBuildOptions::default(),
            &[],
            Some(&request),
        )
        .unwrap();
        // A rebuild reuses the stored transcript: same digest, no second run.
        let rebuilt =
            build_summary_input(&paths, &entry_uid, SummaryBuildOptions::default()).unwrap();
        assert_eq!(transcribed.input_sha256, rebuilt.input_sha256);
        // A better-ranked manual track replaces it and changes the digest.
        add_subtitle_artifact(&paths, entry_id, "manual.en.srt", MANUAL_EN_SRT, "en", "manual");
        let manual =
            build_summary_input(&paths, &entry_uid, SummaryBuildOptions::default()).unwrap();
        assert_ne!(transcribed.input_sha256, manual.input_sha256);
    }

    #[cfg(unix)]
    #[test]
    fn phonon2_non_english_original_language_fails_before_audio_work() {
        let _lock = transcription_lock();
        let (temp, paths, entry_uid, entry_id) = youtube_offline_summary_fixture();
        // An unusable (empty) track whose metadata records the original language.
        std::fs::write(paths.store_path.join("raw/empty.de.vtt"), "WEBVTT\n").unwrap();
        let conn = database::open_or_initialize(&paths.archive_path).unwrap();
        database::add_entry_artifact(
            &conn,
            &database::NewArtifact {
                entry_id,
                artifact_role: subtitles::SUBTITLE_ARTIFACT_ROLE.to_string(),
                storage_area: "raw".to_string(),
                relpath: "raw/empty.de.vtt".to_string(),
                blob_id: None,
                logical_path: None,
                metadata_json: Some(
                    serde_json::json!({
                        "language": "de",
                        "kind": "auto",
                        "format": "vtt",
                        "original_language": "de",
                        "origin": "capture",
                    })
                    .to_string(),
                ),
            },
        )
        .unwrap();
        drop(conn);
        let (request, calls) = fake_transcription(
            temp.path(),
            "phonon2",
            "Phonon-2",
            Some(vec!["en".to_string()]),
        );
        let err = build_summary_input_with_subtitle_fetch(
            &paths,
            &entry_uid,
            SummaryBuildOptions::default(),
            &[],
            Some(&request),
        )
        .unwrap_err();
        let msg = transcriber::transcription_user_message(&err).unwrap();
        assert_eq!(
            msg,
            transcriber::transcription_language_unsupported_message(
                "Phonon-2",
                "de",
                &["en".to_string()]
            )
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        let temp_dir = paths.store_path.join("temp");
        assert!(std::fs::read_dir(&temp_dir).map_or(true, |rd| rd
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().starts_with("transcribe-"))));
    }

    #[cfg(unix)]
    #[test]
    fn empty_transcript_after_transcription_is_no_speech_copy() {
        let _lock = transcription_lock();
        let (temp, paths, entry_uid, _entry_id) = youtube_offline_summary_fixture();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ffmpeg = transcriber::test_support::write_stub_script(
            temp.path(),
            "ffmpeg",
            transcriber::test_support::STUB_FFMPEG,
        );
        let request = transcriber::TranscriptionRequest {
            transcriber: Box::new(FakeTranscriber {
                kind: "whisper",
                label: "Whisper",
                vtt: "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\n \n",
                languages: None,
                calls: calls.clone(),
            }),
            settings: transcriber::TranscriptionSettings { ffmpeg },
        };
        let err = build_summary_input_with_subtitle_fetch(
            &paths,
            &entry_uid,
            SummaryBuildOptions::default(),
            &[],
            Some(&request),
        )
        .unwrap_err();
        assert_eq!(
            transcriber::transcription_user_message(&err).as_deref(),
            Some(NO_SUBTITLES_AFTER_TRANSCRIPTION_MESSAGE)
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
