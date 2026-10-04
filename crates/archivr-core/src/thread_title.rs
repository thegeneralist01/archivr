//! On-demand titles for archived X threads.
//!
//! A user asks for a title from the entry rail; the ordered thread text is sent
//! to the selected summary provider with a cheap per-provider model and the
//! result is saved as `Thread about <topic> — @author` (just `Thread about
//! <topic>` when the author is unknown).
//!
//! The title model never inherits the summary model (`ARCHIVR_*_MODEL`). It is
//! resolved as: the admin's instance setting (passed in by the caller; core
//! never reads the auth DB) > `ARCHIVR_ANTHROPIC_TITLE_MODEL` /
//! `ARCHIVR_OPENAI_TITLE_MODEL` / `ARCHIVR_CLAUDE_TITLE_MODEL` /
//! `ARCHIVR_CODEX_TITLE_MODEL` > a built-in small default. Endpoint, key, CLI
//! path and timeout still come from `summarizer::provider_from_env`.
//!
//! The model returns only the topic phrase; the server builds the rest so the
//! title format (prefix and author suffix) is guaranteed regardless of output.

use anyhow::{Context, Result, bail};
use rusqlite::OptionalExtension;
use std::path::Path;

use crate::archive::ArchivePaths;
use crate::database;
use crate::env_config::optional_env;
use crate::summarizer::{self, ProviderConfig};

pub const TITLE_MAX_TOKENS: u32 = 64;
const MAX_TITLE_INPUT_CHARS: usize = 8_000;
const MAX_TOPIC_WORDS: usize = 10;
const MAX_TOPIC_CHARS: usize = 80;

const TITLE_SYSTEM_PROMPT: &str = "You name archived X (Twitter) threads for a personal archive index. Reply with ONLY a short topic phrase of 3 to 8 words that completes the sentence 'Thread about …' (for example: migrating a home server to NixOS). Plain text on one line: no quotes, no markdown, no hashtags, no emoji, no @mentions, no trailing punctuation, and do not repeat the words 'Thread about'.";

const QUOTE_CHARS: &[char] = &['"', '\'', '`', '“', '”', '‘', '’', '«', '»', '*', '_', '#'];
const TRAILING_PUNCT: &[char] = &['.', ',', ';', ':', '!', '?', '…'];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadTitleInput {
    pub entry_uid: String,
    /// Empty when no status JSON names the author.
    pub author: String,
    pub content: String,
}

pub fn title_model_env(kind: &str) -> Option<&'static str> {
    match kind {
        "anthropic_http" => Some("ARCHIVR_ANTHROPIC_TITLE_MODEL"),
        "openai_compatible" => Some("ARCHIVR_OPENAI_TITLE_MODEL"),
        "claude_cli" => Some("ARCHIVR_CLAUDE_TITLE_MODEL"),
        "codex_cli" => Some("ARCHIVR_CODEX_TITLE_MODEL"),
        _ => None,
    }
}

pub fn default_title_model(kind: &str) -> Option<&'static str> {
    match kind {
        "anthropic_http" => Some("claude-haiku-4-5"),
        "openai_compatible" => Some("gpt-4o-mini"),
        "claude_cli" => Some("haiku"),
        "codex_cli" => Some("gpt-6-luna"),
        _ => None,
    }
}

pub fn with_title_model(cfg: ProviderConfig, model: String) -> ProviderConfig {
    match cfg {
        ProviderConfig::AnthropicHttp(mut c) => {
            c.model = model;
            ProviderConfig::AnthropicHttp(c)
        }
        ProviderConfig::OpenAiCompatible(mut c) => {
            c.model = model;
            ProviderConfig::OpenAiCompatible(c)
        }
        ProviderConfig::ClaudeCli(mut c) => {
            c.model = Some(model);
            ProviderConfig::ClaudeCli(c)
        }
        ProviderConfig::CodexCli(mut c) => {
            c.model = Some(model);
            ProviderConfig::CodexCli(c)
        }
    }
}

/// Where an effective title model came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleModelSource {
    Instance,
    Env,
    Default,
}

impl TitleModelSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Instance => "instance",
            Self::Env => "env",
            Self::Default => "default",
        }
    }
}

/// Effective title model for `kind`: non-empty trimmed `instance_override` >
/// non-empty title env var > built-in default. `None` for unknown kinds.
pub fn resolve_title_model(
    kind: &str,
    instance_override: Option<&str>,
) -> Option<(String, TitleModelSource)> {
    let (var, default) = (title_model_env(kind)?, default_title_model(kind)?);
    if let Some(m) = instance_override.map(str::trim).filter(|m| !m.is_empty()) {
        return Some((m.to_string(), TitleModelSource::Instance));
    }
    if let Some(m) = optional_env(var).map(|m| m.trim().to_string()).filter(|m| !m.is_empty()) {
        return Some((m, TitleModelSource::Env));
    }
    Some((default.to_string(), TitleModelSource::Default))
}

/// Provider config for title generation: transport settings from the summary
/// env, model from [`resolve_title_model`].
pub fn title_provider_from_env(
    kind: &str,
    instance_override: Option<&str>,
) -> Result<ProviderConfig> {
    // Validates `kind` and keeps the summary path's missing-key messages.
    let cfg = summarizer::provider_from_env(kind)?;
    let Some((model, _)) = resolve_title_model(kind, instance_override) else {
        bail!("unknown summary provider: {kind}");
    };
    Ok(with_title_model(cfg, model))
}

/// Expected, user-facing failure of [`load_thread_title_input`] (entry is not a
/// thread, or has no archived text). Anything else is an internal error.
#[derive(Debug)]
pub struct ThreadTitleUserError(pub String);

impl std::fmt::Display for ThreadTitleUserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ThreadTitleUserError {}

/// The [`ThreadTitleUserError`] message carried by `error`, if any.
pub fn thread_title_user_message(error: &anyhow::Error) -> Option<String> {
    error.downcast_ref::<ThreadTitleUserError>().map(|m| m.0.clone())
}

/// Loads the thread text and author. `Ok(None)` means the entry does not exist.
pub fn load_thread_title_input(
    paths: &ArchivePaths,
    entry_uid: &str,
) -> Result<Option<ThreadTitleInput>> {
    let conn = database::open_or_initialize(&paths.archive_path)?;
    let Some((id, entity_kind, source_metadata_json)) = conn
        .query_row(
            "SELECT id, entity_kind, source_metadata_json FROM archived_entries WHERE entry_uid = ?1",
            [entry_uid],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)),
        )
        .optional()?
    else {
        return Ok(None);
    };
    if entity_kind != "tweet_thread" {
        return Err(anyhow::Error::new(ThreadTitleUserError(format!(
            "entry is '{entity_kind}', not an X thread; titles can only be generated for threads"
        ))));
    }

    let content = summarizer::artifact_text_content(&conn, &paths.store_path, id, &entity_kind)
        .map_err(|e| {
            if summarizer::is_unsupported_summary_content_error(&e) {
                anyhow::Error::new(ThreadTitleUserError(
                    "this thread has no archived text to generate a title from".to_string(),
                ))
            } else {
                e
            }
        })?;
    let content: String = content.chars().take(MAX_TITLE_INPUT_CHARS).collect();

    let root_tweet_id = serde_json::from_str::<serde_json::Value>(&source_metadata_json)
        .ok()
        .and_then(|v| v["tweet_id"].as_str().map(str::to_string));
    let author = thread_author(
        &conn,
        &paths.store_path,
        id,
        root_tweet_id.as_deref(),
        entry_uid,
    )?;

    Ok(Some(ThreadTitleInput {
        entry_uid: entry_uid.to_string(),
        author,
        content,
    }))
}

/// Author of the root status (`tweet-<source_metadata.tweet_id>.json`, as in
/// capture's `Thread by @…`), else of the first readable status JSON.
fn thread_author(
    conn: &rusqlite::Connection,
    store_path: &Path,
    entry_id: i64,
    root_tweet_id: Option<&str>,
    entry_uid: &str,
) -> Result<String> {
    let root_file = root_tweet_id.map(|id| format!("tweet-{id}.json"));
    for role in ["raw_tweet_json", "primary_media"] {
        let mut artifacts: Vec<_> = database::list_entry_artifacts_by_role(conn, entry_id, role)?
            .into_iter()
            .filter(|a| a.relpath.ends_with(".json"))
            .collect();
        if artifacts.is_empty() {
            continue;
        }
        // Root status first; the rest keep insertion order (stable sort).
        if let Some(root_file) = root_file.as_deref() {
            artifacts.sort_by_key(|a| {
                !(Path::new(&a.relpath).file_name().and_then(|n| n.to_str()) == Some(root_file))
            });
        }
        for artifact in &artifacts {
            let abs = store_path.join(&artifact.relpath);
            let parsed = std::fs::read_to_string(&abs)
                .with_context(|| format!("failed to read {}", abs.display()))
                .and_then(|raw| {
                    serde_json::from_str::<serde_json::Value>(&raw)
                        .with_context(|| format!("{} is not valid JSON", abs.display()))
                });
            let json = match parsed {
                Ok(json) => json,
                Err(e) => {
                    eprintln!("warn: thread title {entry_uid}: {e:#}");
                    continue;
                }
            };
            let name = json["author"]["screen_name"]
                .as_str()
                .map(|s| s.trim().trim_start_matches('@').trim())
                .filter(|s| !s.is_empty());
            if let Some(name) = name {
                return Ok(name.to_string());
            }
        }
        // Only the first role that has JSON artifacts is consulted, matching
        // `artifact_text_content`'s legacy `primary_media` fallback.
        break;
    }
    Ok(String::new())
}

pub fn build_title_user_prompt(input: &ThreadTitleInput) -> String {
    if input.author.is_empty() {
        format!("Thread:\n{}\n", input.content)
    } else {
        format!("Author: @{}\n\nThread:\n{}\n", input.author, input.content)
    }
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then(|| &s[prefix.len()..])
}

fn trim_trailing_punct(s: &str) -> &str {
    s.trim_end_matches(|c: char| TRAILING_PUNCT.contains(&c) || QUOTE_CHARS.contains(&c))
        .trim_end()
}

/// Reduces a model reply to a single short topic phrase.
pub fn sanitize_topic(raw: &str) -> Result<String> {
    let line = raw
        .lines()
        .filter(|l| !l.trim().starts_with("```"))
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let line: String = line.chars().filter(|c| !c.is_control()).collect();

    let mut s: &str = line.trim();
    for prefix in ["title:", "topic:"] {
        if let Some(rest) = strip_prefix_ci(s, prefix) {
            s = rest;
            break;
        }
    }
    loop {
        let before = s;
        s = s.trim().trim_matches(|c: char| QUOTE_CHARS.contains(&c));
        if let Some(rest) = strip_prefix_ci(s, "thread about ") {
            s = rest;
        }
        if let Some(rest) = strip_prefix_ci(s, "about ") {
            s = rest;
        }
        if s == before {
            break;
        }
    }
    for sep in [" — @", " – @", " - @"] {
        if let Some(idx) = s.find(sep) {
            s = &s[..idx];
        }
    }

    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = trim_trailing_punct(&collapsed);
    let mut topic = trimmed
        .split(' ')
        .filter(|w| !w.is_empty())
        .take(MAX_TOPIC_WORDS)
        .collect::<Vec<_>>()
        .join(" ");

    if topic.chars().count() > MAX_TOPIC_CHARS {
        let head: String = topic.chars().take(MAX_TOPIC_CHARS).collect();
        // `head` is MAX_TOPIC_CHARS chars and the next char exists, so a space
        // at the cut point is preserved by checking the following char too.
        let next_is_space = topic.chars().nth(MAX_TOPIC_CHARS) == Some(' ');
        let cut = if next_is_space {
            head.as_str()
        } else {
            match head.rfind(' ') {
                Some(idx) => &head[..idx],
                None => head.as_str(),
            }
        };
        topic = trim_trailing_punct(cut).to_string();
    }

    if topic.is_empty() {
        bail!("title provider returned no usable title");
    }
    Ok(topic)
}

/// `author` is empty when unknown; the ` — @…` suffix is then omitted.
pub fn format_thread_title(topic: &str, author: &str) -> String {
    if author.is_empty() {
        format!("Thread about {topic}")
    } else {
        format!("Thread about {topic} — @{author}")
    }
}

fn short(s: &str) -> String {
    s.chars().take(200).collect()
}

/// Asks the provider for a topic and builds the final title.
pub fn generate_thread_title(cfg: &ProviderConfig, input: &ThreadTitleInput) -> Result<String> {
    let out = summarizer::complete_plain(
        cfg,
        TITLE_SYSTEM_PROMPT,
        &build_title_user_prompt(input),
        TITLE_MAX_TOKENS,
    )
    .context("title generation failed")?;
    let topic =
        sanitize_topic(&out.text).with_context(|| format!("raw reply: {}", short(&out.text)))?;
    Ok(format_thread_title(&topic, &input.author))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::summarizer::{CliProviderConfig, HttpProviderConfig};

    #[test]
    fn sanitize_topic_cleans_model_replies() {
        let ok = |raw: &str| sanitize_topic(raw).unwrap();
        assert_eq!(ok("\"Rust async runtimes compared.\""), "Rust async runtimes compared");
        assert_eq!(ok("Thread about NixOS on a Pi"), "NixOS on a Pi");
        assert_eq!(ok("```\nTitle: **Home lab networking**\n```"), "Home lab networking");
        assert_eq!(ok("\nFirst line topic\nSecond line"), "First line topic");
        assert_eq!(ok("Foo bar — @alice"), "Foo bar");
        let twenty = (1..=20).map(|i| format!("w{i}")).collect::<Vec<_>>().join(" ");
        assert_eq!(ok(&twenty), "w1 w2 w3 w4 w5 w6 w7 w8 w9 w10");
        let long_word = "x".repeat(120);
        assert!(ok(&long_word).chars().count() <= MAX_TOPIC_CHARS);
        let long_words = vec!["abcdefghijk"; 10].join(" ");
        let cut = ok(&long_words);
        assert!(cut.chars().count() <= MAX_TOPIC_CHARS);
        assert!(!cut.ends_with(' '));
        assert!(sanitize_topic("   ").is_err());
        assert!(sanitize_topic("\"\"").is_err());
    }

    #[test]
    fn format_thread_title_uses_em_dash_suffix() {
        assert_eq!(format_thread_title("x y", "bob"), "Thread about x y — @bob");
        assert_eq!(format_thread_title("x y", ""), "Thread about x y");
    }

    #[test]
    fn title_models_cover_all_provider_kinds() {
        for kind in summarizer::PROVIDER_KINDS {
            assert!(title_model_env(kind).is_some(), "{kind}");
            assert!(default_title_model(kind).is_some(), "{kind}");
        }
        assert_eq!(title_model_env("claude_cli"), Some("ARCHIVR_CLAUDE_TITLE_MODEL"));
        assert_eq!(default_title_model("codex_cli"), Some("gpt-6-luna"));
        assert_eq!(default_title_model("anthropic_http"), Some("claude-haiku-4-5"));
        assert_eq!(title_model_env("gemini"), None);
        assert_eq!(default_title_model("gemini"), None);
    }

    #[test]
    fn with_title_model_sets_model_on_every_variant() {
        let http = HttpProviderConfig {
            endpoint: "https://example.invalid".into(),
            api_key: "k".into(),
            model: "big".into(),
            timeout_secs: 1,
        };
        let cli = CliProviderConfig {
            executable: "claude".into(),
            model: None,
            timeout_secs: 1,
        };
        let m = || "small".to_string();
        match with_title_model(ProviderConfig::AnthropicHttp(http.clone()), m()) {
            ProviderConfig::AnthropicHttp(c) => assert_eq!(c.model, "small"),
            other => panic!("{other:?}"),
        }
        match with_title_model(ProviderConfig::OpenAiCompatible(http), m()) {
            ProviderConfig::OpenAiCompatible(c) => assert_eq!(c.model, "small"),
            other => panic!("{other:?}"),
        }
        match with_title_model(ProviderConfig::ClaudeCli(cli.clone()), m()) {
            ProviderConfig::ClaudeCli(c) => assert_eq!(c.model.as_deref(), Some("small")),
            other => panic!("{other:?}"),
        }
        match with_title_model(ProviderConfig::CodexCli(cli), m()) {
            ProviderConfig::CodexCli(c) => assert_eq!(c.model.as_deref(), Some("small")),
            other => panic!("{other:?}"),
        }
    }

    /// Archive with one entry of `entity_kind` and the given raw tweet JSON files.
    fn fixture(
        entity_kind: &str,
        tweets: &[(&str, serde_json::Value)],
    ) -> (tempfile::TempDir, ArchivePaths, database::ArchivedEntry) {
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
            "x",
            entity_kind,
            Some("9001"),
            Some("https://x.com/alice/status/9001"),
            "x:thread:9001",
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
                source_kind: "x".to_string(),
                entity_kind: entity_kind.to_string(),
                title: Some("Thread by @alice".to_string()),
                visibility: "private".to_string(),
                representation_kind: entity_kind.to_string(),
                source_metadata_json: r#"{"tweet_id":"9001"}"#.to_string(),
                display_metadata_json: None,
            },
        )
        .unwrap();
        std::fs::create_dir_all(paths.store_path.join("raw_tweets")).unwrap();
        for (relpath, body) in tweets {
            std::fs::write(paths.store_path.join(relpath), body.to_string()).unwrap();
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
        (temp, paths, entry)
    }

    fn alice_thread() -> Vec<(&'static str, serde_json::Value)> {
        vec![
            (
                "raw_tweets/tweet-9001.json",
                serde_json::json!({
                    "full_text": "1/ Comparing Rust async runtimes.",
                    "author": { "screen_name": "@alice" }
                }),
            ),
            (
                "raw_tweets/tweet-9002.json",
                serde_json::json!({
                    "full_text": "2/ Tokio wins on ecosystem.",
                    "author": { "screen_name": "alice" }
                }),
            ),
        ]
    }

    #[test]
    fn load_thread_title_input_reads_thread_text_and_author() {
        let (_temp, paths, entry) = fixture("tweet_thread", &alice_thread());
        let input = load_thread_title_input(&paths, &entry.entry_uid)
            .unwrap()
            .unwrap();
        assert_eq!(input.entry_uid, entry.entry_uid);
        assert_eq!(input.author, "alice");
        assert!(
            input
                .content
                .contains("1/ Comparing Rust async runtimes.\n\n---\n\n2/ Tokio wins on ecosystem."),
            "{}",
            input.content
        );
        assert!(!input.content.contains("Thread by @alice"));
    }

    #[test]
    fn load_thread_title_input_prefers_root_status_author() {
        let tweets = vec![
            (
                "raw_tweets/tweet-8000.json",
                serde_json::json!({ "full_text": "quoted", "author": { "screen_name": "bob" } }),
            ),
            (
                "raw_tweets/tweet-9001.json",
                serde_json::json!({ "full_text": "root", "author": { "screen_name": "alice" } }),
            ),
        ];
        let (_temp, paths, entry) = fixture("tweet_thread", &tweets);
        let input = load_thread_title_input(&paths, &entry.entry_uid)
            .unwrap()
            .unwrap();
        assert_eq!(input.author, "alice");
    }

    #[test]
    fn load_thread_title_input_rejects_non_threads_and_empty_threads() {
        let (_temp, paths, entry) = fixture("page", &[]);
        let err = load_thread_title_input(&paths, &entry.entry_uid).unwrap_err();
        assert!(format!("{err:#}").contains("not an X thread"), "{err:#}");
        assert!(thread_title_user_message(&err).is_some());
        assert!(load_thread_title_input(&paths, "no-such-uid").unwrap().is_none());

        let empty = vec![(
            "raw_tweets/tweet-9001.json",
            serde_json::json!({ "full_text": "", "author": { "screen_name": "alice" } }),
        )];
        let (_temp2, paths2, entry2) = fixture("tweet_thread", &empty);
        let err = load_thread_title_input(&paths2, &entry2.entry_uid).unwrap_err();
        assert!(format!("{err:#}").contains("no archived text"), "{err:#}");
        assert!(thread_title_user_message(&err).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn generate_thread_title_runs_claude_cli_with_title_model() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-claude");
        crate::downloader::write_script(
            &script,
            "#!/bin/sh\ncat >/dev/null\ncase \" $* \" in *\" --model haiku \"*) ;; *) echo \"bad args: $*\" >&2; exit 3;; esac\nprintf '%s\\n' '\"Rust async runtimes compared.\"'\n",
        );
        let cfg = ProviderConfig::ClaudeCli(CliProviderConfig {
            executable: script,
            model: Some("haiku".into()),
            timeout_secs: 30,
        });
        let input = ThreadTitleInput {
            entry_uid: "uid".into(),
            author: "alice".into(),
            content: "1/ Comparing Rust async runtimes.".into(),
        };
        // Parallel tests forking while the script fd was open can briefly make
        // exec fail with ETXTBSY (rust-lang/rust#114554); retry that case only.
        let mut attempt = 0;
        let title = loop {
            match generate_thread_title(&cfg, &input) {
                Err(e) if attempt < 20 && format!("{e:#}").contains("Text file busy") => {
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                other => break other.unwrap(),
            }
        };
        assert_eq!(title, "Thread about Rust async runtimes compared — @alice");
    }

    #[test]
    fn resolve_title_model_prefers_instance_then_env_then_default() {
        // Only this test touches the codex title env var; restored below.
        let var = "ARCHIVR_CODEX_TITLE_MODEL";
        let previous = std::env::var_os(var);
        unsafe { std::env::remove_var(var) };
        assert_eq!(
            resolve_title_model("codex_cli", None),
            Some(("gpt-6-luna".into(), TitleModelSource::Default))
        );
        assert_eq!(
            resolve_title_model("codex_cli", Some("  ")),
            Some(("gpt-6-luna".into(), TitleModelSource::Default))
        );
        unsafe { std::env::set_var(var, " env-model ") };
        assert_eq!(
            resolve_title_model("codex_cli", None),
            Some(("env-model".into(), TitleModelSource::Env))
        );
        assert_eq!(
            resolve_title_model("codex_cli", Some(" inst-model ")),
            Some(("inst-model".into(), TitleModelSource::Instance))
        );
        unsafe {
            match previous {
                Some(v) => std::env::set_var(var, v),
                None => std::env::remove_var(var),
            }
        }
        assert_eq!(resolve_title_model("gemini", Some("x")), None);
        assert_eq!(TitleModelSource::Env.as_str(), "env");
    }
}
