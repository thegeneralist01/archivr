//! Read-only admin views of the running instance:
//!
//! - `GET /api/admin/effective-config` (I2): every `ARCHIVR_*` environment variable the
//!   code reads (from the static [`ENV_VARS`] table), with secrets reduced to `set` and URL
//!   values stripped of userinfo/query, plus derived state (summary providers, title models,
//!   transcription engines, browser extensions).
//! - `GET /api/archives/:archive_id/info` (I1): counts and sizes for one archive, never any
//!   filesystem path.
//!
//! TOML-only values (`auth_db_path`, archive filesystem paths) are deliberately never shown.
//! When you add an `ARCHIVR_*` env var anywhere in the workspace, add it to [`ENV_VARS`];
//! the `env_var_table_covers_every_literal_in_the_source` test fails otherwise.
use archivr_core::{database, summarizer, thread_title, transcriber};
use axum::{
    Json, Router,
    extract::{Path, State},
    routing::get,
};
use rusqlite::Connection;
use serde_json::{Value, json};

use crate::auth::{AuthUser, ROLE_ADMIN};
use crate::routes::{ApiError, AppState, mounted_archive};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/admin/effective-config", get(effective_config_handler))
        .route("/api/archives/:archive_id/info", get(archive_info_handler))
}

// ── Env var table ───────────────────────────────────────────────────────────

/// One `ARCHIVR_*` environment variable read by the workspace.
pub(crate) struct EnvSpec {
    pub name: &'static str,
    /// One of `server`, `tools`, `summaries`, `titles`, `transcription`.
    pub group: &'static str,
    pub description: &'static str,
    /// Secret variables only ever report `set`; their value is never serialized.
    pub secret: bool,
    /// Literal built-in default, when the code has one (shown as `default`, and as the
    /// effective `value` when the variable is unset).
    pub default: Option<&'static str>,
}

const fn spec(
    name: &'static str,
    group: &'static str,
    default: Option<&'static str>,
    description: &'static str,
) -> EnvSpec {
    EnvSpec { name, group, description, secret: false, default }
}

const fn secret(name: &'static str, group: &'static str, description: &'static str) -> EnvSpec {
    EnvSpec { name, group, description, secret: true, default: None }
}

/// Every `ARCHIVR_*` variable the workspace reads. Keep in sync: the drift test in this
/// module greps the crates' source for `"ARCHIVR_*"` literals and fails on anything missing.
pub(crate) static ENV_VARS: &[EnvSpec] = &[
    // server
    spec("ARCHIVR_BIND", "server", Some("127.0.0.1:8080"), "Bind address; overrides `bind` in the server TOML."),
    spec("ARCHIVR_STATIC_DIR", "server", None, "Pre-built frontend asset directory (defaults to the directory bundled with the server)."),
    spec("ARCHIVR_STATE_DIR", "server", None, "Mutable state directory where self-updated yt-dlp and Deno are installed (defaults to the platform state dir)."),
    // tools
    spec("ARCHIVR_YT_DLP", "tools", Some("yt-dlp"), "yt-dlp binary used for video and social downloads."),
    spec("ARCHIVR_YT_DLP_FORCE", "tools", None, "Absolute path to a yt-dlp binary that must be used, bypassing the resolver."),
    spec("ARCHIVR_DENO", "tools", None, "Pinned Deno binary offered to the JS runtime resolver."),
    spec("ARCHIVR_JS_RUNTIME", "tools", None, "Forced JS runtime for yt-dlp as RUNTIME[:ABS_PATH] (deno, node, bun, quickjs)."),
    spec("ARCHIVR_SINGLE_FILE", "tools", Some("single-file"), "single-file-cli binary for web page archiving."),
    spec("ARCHIVR_CHROME", "tools", Some("chromium"), "Chromium executable passed to single-file."),
    spec("ARCHIVR_CHROME_ARGS", "tools", None, "Extra space-separated Chromium flags."),
    spec("ARCHIVR_UBLOCK", "tools", Some("true"), "Default for the uBlock extension toggle when no instance setting overrides it (false or 0 disables)."),
    spec("ARCHIVR_UBLOCK_EXT", "tools", None, "Directory of the unpacked uBlock extension."),
    spec("ARCHIVR_COOKIE_CONSENT", "tools", Some("true"), "Default for the cookie-consent extension toggle when no instance setting overrides it (false or 0 disables)."),
    spec("ARCHIVR_COOKIE_EXT", "tools", None, "Directory of the unpacked cookie-consent extension."),
    spec("ARCHIVR_MODAL_CLOSER", "tools", Some("true"), "Default for the modal-closer script toggle when no instance setting overrides it (false or 0 disables)."),
    secret("ARCHIVR_TWITTER_CREDENTIALS_FILE", "tools", "Cookies file for tweet/thread scraping (path is never shown)."),
    spec("ARCHIVR_TWEET_SCRAPER", "tools", Some("vendor/twitter/scrape_user_tweet_contents.py"), "Tweet scraper script path."),
    spec("ARCHIVR_TWEET_PYTHON", "tools", Some("python3"), "Python executable for the tweet scraper."),
    // summaries
    secret("ARCHIVR_ANTHROPIC_API_KEY", "summaries", "API key for the Anthropic Messages API (required for anthropic_http)."),
    spec("ARCHIVR_ANTHROPIC_URL", "summaries", Some("https://api.anthropic.com/v1/messages"), "Anthropic endpoint override."),
    spec("ARCHIVR_ANTHROPIC_MODEL", "summaries", Some("claude-3-5-sonnet-latest"), "Model id used for Anthropic summaries."),
    secret("ARCHIVR_OPENAI_API_KEY", "summaries", "API key for an OpenAI-compatible endpoint (required for openai_compatible)."),
    spec("ARCHIVR_OPENAI_URL", "summaries", Some("https://api.openai.com/v1/chat/completions"), "OpenAI-compatible endpoint override."),
    spec("ARCHIVR_OPENAI_MODEL", "summaries", Some("gpt-4o-mini"), "Model id used for OpenAI-compatible summaries."),
    spec("ARCHIVR_CLAUDE_CLI", "summaries", None, "Path to a local `claude` binary (auto-discovered when unset)."),
    spec("ARCHIVR_CLAUDE_MODEL", "summaries", None, "Optional model override for the local Claude CLI."),
    spec("ARCHIVR_CODEX_CLI", "summaries", None, "Path to a local `codex` binary (auto-discovered when unset)."),
    spec("ARCHIVR_CODEX_MODEL", "summaries", None, "Optional model override for the local Codex CLI."),
    spec("ARCHIVR_SUMMARY_HTTP_TIMEOUT", "summaries", Some("120"), "Seconds before an HTTP-provider summary is killed."),
    spec("ARCHIVR_SUMMARY_CLI_TIMEOUT", "summaries", Some("300"), "Seconds before a CLI-provider summary is killed."),
    // titles
    spec("ARCHIVR_ANTHROPIC_TITLE_MODEL", "titles", Some("claude-haiku-4-5"), "Anthropic model used only for thread-title generation."),
    spec("ARCHIVR_OPENAI_TITLE_MODEL", "titles", Some("gpt-4o-mini"), "OpenAI-compatible model used only for thread-title generation."),
    spec("ARCHIVR_CLAUDE_TITLE_MODEL", "titles", Some("haiku"), "Claude CLI model used only for thread-title generation."),
    spec("ARCHIVR_CODEX_TITLE_MODEL", "titles", Some("gpt-6-luna"), "Codex CLI model used only for thread-title generation."),
    // transcription
    spec("ARCHIVR_TRANSCRIBE_ENGINES", "transcription", None, "Comma-separated enabled local transcription engines: whisper, parakeet, phonon2."),
    spec("ARCHIVR_TRANSCRIBE_TIMEOUT", "transcription", Some("3600"), "Seconds for one whole transcription job."),
    spec("ARCHIVR_FFMPEG", "transcription", Some("ffmpeg"), "ffmpeg binary used to extract 16 kHz mono WAV."),
    spec("ARCHIVR_WHISPER_BACKEND", "transcription", Some("whisper_cpp"), "Whisper backend: whisper_cpp or script."),
    spec("ARCHIVR_WHISPER_CLI", "transcription", None, "whisper.cpp binary, or the wrapper script for the script backend."),
    spec("ARCHIVR_WHISPER_MODEL", "transcription", None, "Whisper model (required for whisper)."),
    spec("ARCHIVR_WHISPER_LANGUAGES", "transcription", None, "Optional allowlist of base language codes for Whisper."),
    spec("ARCHIVR_PARAKEET_CLI", "transcription", None, "Parakeet wrapper script (required for parakeet)."),
    spec("ARCHIVR_PARAKEET_MODEL", "transcription", Some("nvidia/parakeet-tdt-0.6b-v3"), "Parakeet model passed as --model."),
    spec("ARCHIVR_PARAKEET_LANGUAGES", "transcription", None, "Optional allowlist of base language codes for Parakeet."),
    spec("ARCHIVR_PHONON2_CLI", "transcription", None, "The `fermion` CLI used by Phonon-2 (auto-discovered when unset)."),
    spec("ARCHIVR_PHONON2_MODEL", "transcription", Some("phonon-2"), "Model passed to `fermion transcribe`."),
];

/// Variables that appear as `"ARCHIVR_*"` literals only in test code.
#[cfg(test)]
const TEST_ONLY_PREFIXES: &[&str] = &["ARCHIVR_TEST_"];

/// Non-empty (after trim) value of an env var, matching the core `optional_env` semantics.
fn env_value(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// Drops userinfo, query, and fragment from a URL-ish string
/// (`https://user:pw@host/p?k=v#f` -> `https://host/p`).
pub(crate) fn redact_url(raw: &str) -> String {
    let raw = raw.trim();
    let (scheme, rest) = match raw.split_once("://") {
        Some((scheme, rest)) => (Some(scheme), rest),
        None => (None, raw),
    };
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let (authority, path) = match rest.find('/') {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    match scheme {
        Some(scheme) => format!("{scheme}://{host}{path}"),
        None => format!("{host}{path}"),
    }
}

fn is_url_var(name: &str) -> bool {
    name.ends_with("_URL")
}

fn env_var_json(spec: &EnvSpec) -> Value {
    let raw = env_value(spec.name);
    let set = raw.is_some();
    let (value, source) = match (&raw, spec.default) {
        _ if spec.secret => (None, if set { "env" } else { "unset" }),
        (Some(v), _) if is_url_var(spec.name) => (Some(redact_url(v)), "env"),
        (Some(v), _) => (Some(v.clone()), "env"),
        (None, Some(default)) => (Some(default.to_string()), "default"),
        (None, None) => (None, "unset"),
    };
    json!({
        "name": spec.name,
        "group": spec.group,
        "description": spec.description,
        "secret": spec.secret,
        "default": spec.default,
        "set": set,
        "source": source,
        "value": value,
    })
}

// ── Derived state ───────────────────────────────────────────────────────────

fn summary_providers_json() -> Vec<Value> {
    summarizer::PROVIDER_KINDS
        .iter()
        .map(|kind| match summarizer::provider_from_env(kind) {
            Ok(summarizer::ProviderConfig::AnthropicHttp(c))
            | Ok(summarizer::ProviderConfig::OpenAiCompatible(c)) => json!({
                "kind": kind, "configured": true, "model": c.model,
                "error": null, "missing_env": [],
            }),
            Ok(summarizer::ProviderConfig::ClaudeCli(c))
            | Ok(summarizer::ProviderConfig::CodexCli(c)) => {
                let found = crate::routes::cli_executable_available(
                    &c.executable,
                    std::env::var_os("PATH").as_deref(),
                );
                let cli_var = if *kind == "claude_cli" { "ARCHIVR_CLAUDE_CLI" } else { "ARCHIVR_CODEX_CLI" };
                json!({
                    "kind": kind, "configured": found, "model": c.model,
                    "error": if found { Value::Null } else {
                        Value::String(format!("CLI executable not found; set {cli_var} or install it"))
                    },
                    "missing_env": [],
                })
            }
            Err(error) => {
                let key_var = match *kind {
                    "anthropic_http" => Some("ARCHIVR_ANTHROPIC_API_KEY"),
                    "openai_compatible" => Some("ARCHIVR_OPENAI_API_KEY"),
                    _ => None,
                };
                let missing: Vec<&str> = key_var.filter(|v| env_value(v).is_none()).into_iter().collect();
                // `provider_from_env` errors only name the variable, never its value.
                json!({
                    "kind": kind, "configured": false, "model": null,
                    "error": error.to_string(), "missing_env": missing,
                })
            }
        })
        .collect()
}

fn title_models_json(settings: &database::InstanceSettings) -> Value {
    let mut map = serde_json::Map::new();
    for kind in summarizer::PROVIDER_KINDS {
        let Some((model, source)) =
            thread_title::resolve_title_model(kind, settings.title_model_override(kind))
        else {
            continue;
        };
        map.insert(
            kind.to_string(),
            json!({
                "model": model,
                "source": source.as_str(),
                "env_var": thread_title::title_model_env(kind),
            }),
        );
    }
    Value::Object(map)
}

fn transcription_engines_json() -> Vec<Value> {
    let enabled = transcriber::enabled_engine_kinds();
    let available = transcriber::available_transcribers();
    transcriber::TRANSCRIBE_ENGINE_KINDS
        .iter()
        .map(|kind| {
            let is_enabled = enabled.contains(kind);
            let info = available.iter().find(|info| info.kind == *kind);
            let error = if is_enabled && info.is_none() {
                transcriber::transcriber_from_env(kind).err().map(|e| e.to_string())
            } else {
                None
            };
            json!({
                "kind": kind,
                "enabled": is_enabled,
                "configured": info.is_some(),
                "label": info.map(|i| i.label),
                "english_only": info.map(|i| i.english_only),
                "languages": info.and_then(|i| i.languages.clone()),
                "error": error,
            })
        })
        .collect()
}

/// Same availability test the instance-settings GET uses: the env var names an existing directory.
fn extension_available(var: &str) -> bool {
    env_value(var).is_some_and(|p| std::path::Path::new(&p).is_dir())
}

/// Where the running server's bind address comes from. Mirrors `main.rs`:
/// `ARCHIVR_BIND` > `bind` in the server TOML > the built-in default. The address is read
/// from the same inputs (process env and the loaded registry), not from the live listener,
/// so it matches what the server bound at startup as long as the env was not mutated since.
fn bind_json(state: &AppState) -> Value {
    let (value, source) = if let Ok(v) = std::env::var("ARCHIVR_BIND") {
        (v, "env")
    } else if let Some(v) = state.registry.bind.clone() {
        (v, "toml")
    } else {
        (crate::DEFAULT_BIND.to_string(), "default")
    };
    json!({ "value": value, "source": source })
}

async fn effective_config_handler(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<Value>, ApiError> {
    auth.require_role(ROLE_ADMIN)?;
    let conn = database::open_auth_db(&state.auth_db_path)?;
    let settings = database::get_instance_settings(&conn)?;
    let archives: Vec<Value> = state
        .registry
        .archives
        .iter()
        .map(|a| json!({ "id": a.id, "label": a.label }))
        .collect();
    Ok(Json(json!({
        "server": {
            "version": env!("CARGO_PKG_VERSION"),
            "bind": bind_json(&state),
            "archives": archives,
        },
        "env_vars": ENV_VARS.iter().map(env_var_json).collect::<Vec<_>>(),
        "summary_providers": summary_providers_json(),
        "title_models": title_models_json(&settings),
        "transcription_engines": transcription_engines_json(),
        "extensions": {
            "ublock": { "available": extension_available("ARCHIVR_UBLOCK_EXT") },
            "cookie_consent": { "available": extension_available("ARCHIVR_COOKIE_EXT") },
        },
    })))
}

// ── Archive info ────────────────────────────────────────────────────────────

fn count(conn: &Connection, sql: &str) -> Result<i64, ApiError> {
    Ok(conn.query_row(sql, [], |row| row.get(0))?)
}

async fn archive_info_handler(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(archive_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    auth.require_role(ROLE_ADMIN)?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;

    let entry_count = count(&conn, "SELECT COUNT(*) FROM archived_entries")?;
    let root_entry_count =
        count(&conn, "SELECT COUNT(*) FROM archived_entries WHERE parent_entry_id IS NULL")?;
    let job_count = |status: &str| -> Result<i64, ApiError> {
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM capture_jobs WHERE status = ?1",
            [status],
            |row| row.get(0),
        )?)
    };
    // The archive's display name is its own metadata; filesystem paths are never returned.
    let name = archivr_core::archive::read_archive_paths(&mounted.archive_path)
        .ok()
        .map(|paths| paths.name);
    let db_bytes = std::fs::metadata(database::database_path(&mounted.archive_path))
        .map(|m| m.len())
        .unwrap_or(0);

    Ok(Json(json!({
        "archive_id": mounted.id,
        "label": mounted.label,
        "name": name,
        "entry_count": entry_count,
        "root_entry_count": root_entry_count,
        "child_entry_count": entry_count - root_entry_count,
        "artifact_count": count(&conn, "SELECT COUNT(*) FROM entry_artifacts")?,
        "blob_count": count(&conn, "SELECT COUNT(*) FROM blobs")?,
        "blob_bytes": count(&conn, "SELECT COALESCE(SUM(byte_size), 0) FROM blobs")?,
        "tag_count": count(&conn, "SELECT COUNT(*) FROM tags")?,
        "collection_count": count(&conn, "SELECT COUNT(*) FROM collections")?,
        "run_count": count(&conn, "SELECT COUNT(*) FROM archive_runs")?,
        "summary_count": count(&conn, "SELECT COUNT(*) FROM entry_summaries")?,
        "job_counts": {
            "pending": job_count("pending")?,
            "running": job_count("running")?,
            "completed": job_count("completed")?,
            "failed": job_count("failed")?,
        },
        "db_bytes": db_bytes,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::app;
    use crate::test_support::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use std::collections::BTreeSet;
    use tower::ServiceExt;

    /// Serializes tests in this module that mutate env. Only `ARCHIVR_ANTHROPIC_*` variables are
    /// touched, which no other module's tests read or write.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    const MUTATED: &[&str] = &[
        "ARCHIVR_ANTHROPIC_API_KEY",
        "ARCHIVR_ANTHROPIC_URL",
        "ARCHIVR_ANTHROPIC_TITLE_MODEL",
        "ARCHIVR_ANTHROPIC_MODEL",
    ];

    /// Holds the lock, clears the mutated vars, restores them on drop.
    struct EnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl EnvGuard {
        fn new() -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let saved = MUTATED.iter().map(|n| (*n, std::env::var_os(n))).collect();
            for name in MUTATED {
                unsafe { std::env::remove_var(name) };
            }
            Self { _lock: lock, saved }
        }
        fn set(&self, name: &str, value: &str) {
            unsafe { std::env::set_var(name, value) };
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (name, previous) in &self.saved {
                unsafe {
                    match previous {
                        Some(v) => std::env::set_var(name, v),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }
    }

    fn get(uri: &str, cookie: Option<&str>) -> Request<Body> {
        let mut b = Request::builder().uri(uri);
        if let Some(c) = cookie {
            b = b.header("cookie", c);
        }
        b.body(Body::empty()).unwrap()
    }

    async fn status_and_body(
        registry: &crate::registry::ServerRegistry,
        auth_path: &std::path::Path,
        uri: &str,
        cookie: Option<&str>,
    ) -> (StatusCode, Value) {
        let resp = app(registry.clone(), auth_path.to_path_buf())
            .oneshot(get(uri, cookie))
            .await
            .unwrap();
        let status = resp.status();
        let body = if status == StatusCode::NO_CONTENT { Value::Null } else { body_json(resp).await };
        (status, body)
    }

    fn env_entry<'a>(body: &'a Value, name: &str) -> &'a Value {
        body["env_vars"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == name)
            .unwrap_or_else(|| panic!("{name} missing from env_vars"))
    }

    #[tokio::test]
    async fn effective_config_requires_admin() {
        let _env = EnvGuard::new();
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let guest = guest_session(&auth_path);
        let user = user_session(&auth_path);
        let admin = admin_session(&auth_path);
        let uri = "/api/admin/effective-config";

        let (s, _) = status_and_body(&registry, &auth_path, uri, None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let (s, _) = status_and_body(&registry, &auth_path, uri, Some(&guest)).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let (s, _) = status_and_body(&registry, &auth_path, uri, Some(&user)).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let (s, body) = status_and_body(&registry, &auth_path, uri, Some(&admin)).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(body["server"]["archives"][0]["id"], "test");
        assert_eq!(body["server"]["archives"][0]["label"], "Test");
        assert!(body["server"]["version"].is_string());
        assert_eq!(body["env_vars"].as_array().unwrap().len(), ENV_VARS.len());
        for key in ["summary_providers", "title_models", "transcription_engines", "extensions"] {
            assert!(body.get(key).is_some(), "missing {key}");
        }
        // TOML-only values and filesystem paths never appear.
        let text = body.to_string();
        assert!(!text.contains(&dir.path().display().to_string()), "{text}");
        assert!(!text.contains("auth_db_path"));
    }

    #[tokio::test]
    async fn effective_config_hides_secrets_and_strips_url_userinfo() {
        let env = EnvGuard::new();
        env.set("ARCHIVR_ANTHROPIC_API_KEY", "sekret-canary");
        env.set("ARCHIVR_ANTHROPIC_URL", "https://u:p@host.example/v1?key=1#frag");
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let admin = admin_session(&auth_path);

        let (s, body) = status_and_body(&registry, &auth_path, "/api/admin/effective-config", Some(&admin)).await;
        assert_eq!(s, StatusCode::OK);
        let text = body.to_string();
        assert!(!text.contains("sekret-canary"), "{text}");
        assert!(!text.contains("u:p"), "{text}");
        assert!(!text.contains("key=1"), "{text}");

        let key = env_entry(&body, "ARCHIVR_ANTHROPIC_API_KEY");
        assert_eq!(key["secret"], true);
        assert_eq!(key["set"], true);
        assert_eq!(key["source"], "env");
        assert!(key["value"].is_null());

        let url = env_entry(&body, "ARCHIVR_ANTHROPIC_URL");
        assert_eq!(url["secret"], false);
        assert_eq!(url["set"], true);
        assert_eq!(url["value"], "https://host.example/v1");

        let provider = body["summary_providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["kind"] == "anthropic_http")
            .unwrap();
        assert_eq!(provider["configured"], true);
        assert!(provider["error"].is_null());

        // Unset key: not set, provider unconfigured with the missing variable named.
        drop(env);
        let _env = EnvGuard::new();
        let (_, body) = status_and_body(&registry, &auth_path, "/api/admin/effective-config", Some(&admin)).await;
        let key = env_entry(&body, "ARCHIVR_ANTHROPIC_API_KEY");
        assert_eq!(key["set"], false);
        assert_eq!(key["source"], "unset");
        assert!(key["value"].is_null());
        let model = env_entry(&body, "ARCHIVR_ANTHROPIC_MODEL");
        assert_eq!(model["source"], "default");
        assert_eq!(model["value"], "claude-3-5-sonnet-latest");
        assert_eq!(model["set"], false);
        let provider = body["summary_providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["kind"] == "anthropic_http")
            .unwrap();
        assert_eq!(provider["configured"], false);
        assert_eq!(provider["missing_env"][0], "ARCHIVR_ANTHROPIC_API_KEY");
        assert!(provider["error"].as_str().unwrap().contains("ARCHIVR_ANTHROPIC_API_KEY"));
    }

    #[tokio::test]
    async fn effective_config_title_model_sources() {
        let env = EnvGuard::new();
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let admin = admin_session(&auth_path);
        let uri = "/api/admin/effective-config";
        let anthropic = |body: &Value| body["title_models"]["anthropic_http"].clone();

        let (_, body) = status_and_body(&registry, &auth_path, uri, Some(&admin)).await;
        let t = anthropic(&body);
        assert_eq!(t["source"], "default");
        assert_eq!(t["model"], "claude-haiku-4-5");
        assert_eq!(t["env_var"], "ARCHIVR_ANTHROPIC_TITLE_MODEL");

        env.set("ARCHIVR_ANTHROPIC_TITLE_MODEL", "env-title-model");
        let (_, body) = status_and_body(&registry, &auth_path, uri, Some(&admin)).await;
        let t = anthropic(&body);
        assert_eq!(t["source"], "env");
        assert_eq!(t["model"], "env-title-model");

        // An instance setting (set through the instance-settings PATCH) wins over the env var.
        let patch = Request::builder()
            .method("PATCH")
            .uri("/api/admin/instance-settings")
            .header("cookie", &admin)
            .header("content-type", "application/json")
            .body(json_body(&json!({ "title_model_anthropic_http": "instance-model" })))
            .unwrap();
        let resp = app(registry.clone(), auth_path.clone()).oneshot(patch).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let (_, body) = status_and_body(&registry, &auth_path, uri, Some(&admin)).await;
        let t = anthropic(&body);
        assert_eq!(t["source"], "instance");
        assert_eq!(t["model"], "instance-model");
        for kind in summarizer::PROVIDER_KINDS {
            assert!(body["title_models"][kind]["model"].is_string(), "{kind}");
        }
    }

    #[tokio::test]
    async fn effective_config_bind_source() {
        let dir = tempfile::tempdir().unwrap();
        let (mut registry, _, auth_path) = make_test_registry(&dir);
        let admin = admin_session(&auth_path);
        let uri = "/api/admin/effective-config";
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var_os("ARCHIVR_BIND");
        unsafe { std::env::remove_var("ARCHIVR_BIND") };

        let (_, body) = status_and_body(&registry, &auth_path, uri, Some(&admin)).await;
        assert_eq!(body["server"]["bind"], json!({"value": "127.0.0.1:8080", "source": "default"}));
        registry.bind = Some("0.0.0.0:9000".into());
        let (_, body) = status_and_body(&registry, &auth_path, uri, Some(&admin)).await;
        assert_eq!(body["server"]["bind"], json!({"value": "0.0.0.0:9000", "source": "toml"}));
        unsafe { std::env::set_var("ARCHIVR_BIND", "127.0.0.1:7777") };
        let (_, body) = status_and_body(&registry, &auth_path, uri, Some(&admin)).await;
        assert_eq!(body["server"]["bind"], json!({"value": "127.0.0.1:7777", "source": "env"}));

        unsafe {
            match saved {
                Some(v) => std::env::set_var("ARCHIVR_BIND", v),
                None => std::env::remove_var("ARCHIVR_BIND"),
            }
        }
    }

    #[test]
    fn redact_url_strips_userinfo_query_and_fragment() {
        assert_eq!(redact_url("https://user:pw@host/p?k=v"), "https://host/p");
        assert_eq!(redact_url("http://127.0.0.1:8080/v1/chat#x"), "http://127.0.0.1:8080/v1/chat");
        assert_eq!(redact_url("https://tok@host?x=1"), "https://host");
        assert_eq!(redact_url("https://host/a@b"), "https://host/a@b");
        assert_eq!(redact_url("u:p@host/v1?k=1"), "host/v1");
    }

    #[test]
    fn env_table_is_well_formed() {
        let groups = ["server", "tools", "summaries", "titles", "transcription"];
        let mut seen = BTreeSet::new();
        for spec in ENV_VARS {
            assert!(seen.insert(spec.name), "duplicate {}", spec.name);
            assert!(groups.contains(&spec.group), "{} has group {}", spec.name, spec.group);
            assert!(!spec.description.is_empty(), "{}", spec.name);
            let sensitive = spec.name.ends_with("_API_KEY") || spec.name.ends_with("_CREDENTIALS_FILE");
            assert_eq!(spec.secret, sensitive, "{} secret flag", spec.name);
            assert!(!(spec.secret && spec.default.is_some()), "{}", spec.name);
        }
        // Title-model defaults must stay in sync with the resolver.
        for kind in summarizer::PROVIDER_KINDS {
            let var = thread_title::title_model_env(kind).unwrap();
            let spec = ENV_VARS.iter().find(|s| s.name == var).unwrap();
            assert_eq!(spec.default, thread_title::default_title_model(kind), "{var}");
        }
    }

    /// Collects `"ARCHIVR_[A-Z0-9_]+"` string literals from `*/src/**/*.rs` of every crate.
    fn source_env_literals() -> BTreeSet<String> {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else { return };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }
        let re = regex::Regex::new(r#""(ARCHIVR_[A-Z0-9_]+)""#).unwrap();
        let crates_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut files = Vec::new();
        for krate in std::fs::read_dir(&crates_dir).unwrap().flatten() {
            walk(&krate.path().join("src"), &mut files);
        }
        assert!(files.len() > 10, "source walk found only {} files", files.len());
        let mut found = BTreeSet::new();
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap_or_default();
            for cap in re.captures_iter(&text) {
                found.insert(cap[1].to_string());
            }
        }
        found
    }

    #[test]
    fn env_var_table_covers_every_literal_in_the_source() {
        let known: BTreeSet<&str> = ENV_VARS.iter().map(|s| s.name).collect();
        let missing: Vec<String> = source_env_literals()
            .into_iter()
            .filter(|name| !TEST_ONLY_PREFIXES.iter().any(|p| name.starts_with(p)))
            .filter(|name| !known.contains(name.as_str()))
            .collect();
        assert!(
            missing.is_empty(),
            "ARCHIVR_* env vars read in source but missing from ENV_VARS in effective_config.rs: {missing:?}"
        );
    }

    #[test]
    fn env_var_table_has_no_stale_entries() {
        let found = source_env_literals();
        let stale: Vec<&str> = ENV_VARS
            .iter()
            .map(|s| s.name)
            .filter(|name| !found.contains(*name))
            .collect();
        assert!(stale.is_empty(), "ENV_VARS entries no longer present in the source: {stale:?}");
    }

    #[tokio::test]
    async fn archive_info_requires_admin_and_known_archive() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let guest = guest_session(&auth_path);
        let user = user_session(&auth_path);
        let admin = admin_session(&auth_path);
        let uri = "/api/archives/test/info";

        let (s, _) = status_and_body(&registry, &auth_path, uri, None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let (s, _) = status_and_body(&registry, &auth_path, uri, Some(&guest)).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let (s, _) = status_and_body(&registry, &auth_path, uri, Some(&user)).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let (s, _) = status_and_body(&registry, &auth_path, "/api/archives/nope/info", Some(&admin)).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (s, body) = status_and_body(&registry, &auth_path, uri, Some(&admin)).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(body["archive_id"], "test");
        assert_eq!(body["label"], "Test");
        assert_eq!(body["entry_count"], 0);
        assert_eq!(body["job_counts"]["pending"], 0);
        assert!(body["db_bytes"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn archive_info_counts_match_seeded_data() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let admin = admin_session(&auth_path);
        let conn = database::open_or_initialize(&archive_path).unwrap();
        let user_id = database::ensure_default_user(&conn).unwrap();
        let run = database::create_archive_run(&conn, user_id, 1).unwrap();
        let si = database::upsert_source_identity(
            &conn, "web", "page", None, Some("https://example.com/a"), "https://example.com/a",
        )
        .unwrap();
        let new_entry = |parent: Option<i64>, root: Option<i64>| database::NewEntry {
            source_identity_id: si,
            archive_run_id: run.id,
            parent_entry_id: parent,
            root_entry_id: root,
            created_by_user_id: user_id,
            owned_by_user_id: user_id,
            source_kind: "web".to_string(),
            entity_kind: "page".to_string(),
            title: Some("t".to_string()),
            visibility: "private".to_string(),
            representation_kind: "html".to_string(),
            source_metadata_json: "{}".to_string(),
            display_metadata_json: None,
        };
        let root = database::create_archived_entry(&conn, &new_entry(None, None)).unwrap();
        let _root2 = database::create_archived_entry(&conn, &new_entry(None, None)).unwrap();
        let _child = database::create_archived_entry(&conn, &new_entry(Some(root.id), Some(root.id))).unwrap();
        database::create_tag_path(&conn, "a/b").unwrap();
        conn.execute(
            "INSERT INTO blobs (sha256, byte_size, raw_relpath, created_at) VALUES ('aa', 100, 'r/aa', 'now'), ('bb', 23, 'r/bb', 'now')",
            [],
        )
        .unwrap();
        let tag_count: i64 = conn.query_row("SELECT COUNT(*) FROM tags", [], |r| r.get(0)).unwrap();
        let collection_count: i64 = conn.query_row("SELECT COUNT(*) FROM collections", [], |r| r.get(0)).unwrap();
        let _pending = database::create_capture_job(&conn, "test").unwrap();
        let failed = database::create_capture_job(&conn, "test").unwrap();
        conn.execute("UPDATE capture_jobs SET status = 'failed' WHERE job_uid = ?1", [&failed]).unwrap();
        drop(conn);

        let (s, body) = status_and_body(&registry, &auth_path, "/api/archives/test/info", Some(&admin)).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(body["entry_count"], 3);
        assert_eq!(body["root_entry_count"], 2);
        assert_eq!(body["child_entry_count"], 1);
        assert_eq!(body["blob_count"], 2);
        assert_eq!(body["blob_bytes"], 123);
        assert_eq!(body["tag_count"], tag_count);
        assert!(tag_count >= 2);
        assert_eq!(body["collection_count"], collection_count);
        assert_eq!(body["run_count"], 1);
        assert_eq!(body["summary_count"], 0);
        assert_eq!(body["artifact_count"], 0);
        assert_eq!(body["job_counts"], json!({"pending": 1, "running": 0, "completed": 0, "failed": 1}));
        // Counts and sizes only: no filesystem paths anywhere in the body.
        let text = body.to_string();
        assert!(!text.contains(&dir.path().display().to_string()), "{text}");
        assert!(body.get("archive_path").is_none() && body.get("store_path").is_none());
    }
}
