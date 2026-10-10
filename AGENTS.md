# Repository Guidelines

## Project Overview

archivr is a self-hosted archival tool that captures and preserves digital content — YouTube/Twitter/Instagram/TikTok/Reddit posts, arbitrary URLs, full web pages (via SingleFile + Chromium, optionally through a Freedium mirror for paywalled articles), and local files — into self-contained, SQLite-backed archive directories with blob deduplication, hierarchical tags, collections, and role-based auth. Rust workspace + React frontend.

Read `ARCHIVR-MENTAL-MODEL.md` before making structural changes.

For user-facing behavior, API contracts, capture options, shared UI, permissions, docs, or branding, use the [ecosystem change checklist](docs/ecosystem-change-checklist.md) before implementation and again before reporting completion. Record applicability, action or reason, verification, and links for Archivr, the Chrome extension, and the MCP server. This applies to direct agent work as well as PRs; an applicable unfinished companion change needs a named follow-up.

## Architecture & Data Flow

Three crates with a strict ownership split — **core owns truth; CLI and server are adapters**:

- `crates/archivr-core` — domain library: capture orchestration, SQLite schema/CRUD, downloaders, hashing. New archive features start here.
- `crates/archivr-server` — Axum HTTP API + auth + static frontend serving.
- `crates/archivr-cli` — clap-based CLI (`archivr` binary): `init`, `archive`, `yt-dlp status|update` subcommands.

Capture flow: locator → `determine_source()` (`crates/archivr-core/src/capture.rs`) routes by platform/shorthand (`yt:`, `x:`, `tweet:` …) → platform downloader (`downloader/ytdlp.rs`, `tweets.rs`, `singlefile.rs`, `http.rs`, `local.rs`) stages into `temp/` → SHA3-256 dedup (`hash.rs`, `downloader/store.rs`) moves blobs to `raw/A/B/HASH.EXT` → rows written to `archivr.sqlite` (runs, entries, artifacts, blobs) → served via `/api/archives/:id/...`. `CaptureConfig` carries per-request toggles (uBlock, reader mode, Freedium mirror, YouTube subtitles, etc.); when `via_freedium` is set, the fetch URL is rewritten through `freedium-mirror.cfd` while the canonical DB URL stays the original locator.

YouTube playlists and channels produce a **parent container entry** with each video captured as a child entry. `downloader/ytdlp.rs` handles the flat-playlist probe (fetching per-video quality metadata before archiving); the multi-video download loop and sync mode (skipping already-archived videos when re-archiving a playlist or channel) live in `capture.rs`. Child order is persisted in `archived_entries.position` (append-only at insert in `database::create_archived_entry`; rewritten only by `database::reorder_child_entries` behind `PUT …/entries/:entry_uid/children/order` (allowed roles = `InstanceSettings::reorder_children_role_bits`, default ADMIN|OWNER, Owner-editable)).

YouTube videos (`Source::YouTubeVideo`: single videos and playlist/channel children; no other platform) also get up
to two subtitle tracks (English + original language, manual over auto, VTT/SRT) in the same yt-dlp call by default.
`CaptureConfig::download_subtitles` (manual `Default` = true) gates it; the API body field `download_subtitles` (absent
= true), the CaptureDialog "Download subtitles" toggle and CLI `archivr archive --no-subtitles` feed it. Files are
archived into `raw/` and registered as `subtitle` artifacts (`crates/archivr-core/src/subtitles.rs`). Subtitle
failures are `eprintln!` warnings, never capture failures.

Pasted text takes a much shorter path: `perform_text_capture()` (`capture.rs`) skips source detection
and every downloader shell-out — `downloader/text.rs` stages the body under `temp/`, hashes it, and the
blob lands in `raw/` like any other artifact. Entrypoint is
`POST /api/archives/:archive_id/captures/text`. Its body is byte-preserving, its entry has no fabricated
`original_url`, and its normal text preview opens from the entry rail.

LLM summaries are a post-capture, manual-only subsystem: `crates/archivr-core/src/summarizer.rs` behind
`GET`/`POST /api/archives/:archive_id/entries/:entry_uid/summary`, cached in the `entry_summaries`
table per (entry, provider, model, prompt version, input hash). See `ARCHIVR-MENTAL-MODEL.md` for the
provider set and the status lifecycle.

YouTube video entries are summarized from the best-ranked `subtitle` artifact reduced to a transcript, not the mp4.
With no usable track the POST still returns 202: the row is created `pending` with the placeholder `input_sha256`
`pending-subtitle-fetch`, and the background task fetches subtitles only (`build_summary_input_with_subtitle_fetch` →
`subtitles::fetch_subtitles_for_entry`), writes the real hash, then runs the provider. The order is fixed (user
requirement): archived subtitles → fetched subtitles → local transcription (`transcriber::transcribe_entry`, only when
the POST body names a `transcribe_engine` and both earlier steps gave nothing) → error. If there are still none, the
row fails with `NO_SUBTITLES_SUMMARY_MESSAGE` (or a transcription-specific copy when an engine ran) and no provider is
called. Transcripts are `subtitle` artifacts with `kind: "transcribed"`, `origin: "transcription"`, `engine`, `model`.
Spec + deviations: `docs/superpowers/specs/2026-10-05-local-transcription-fallback.md`.

X thread titles: `POST /api/archives/:archive_id/entries/:entry_uid/thread-title` (`ROLE_USER`, same gate as title
PATCH; body `{provider}`) runs `thread_title::generate_thread_title` synchronously in one blocking task and saves
`Thread about <topic> — @author` via `database::update_entry_title`. Never touches `entry_summaries`.

The requested provider model is the cache identity; a provider-returned resolved model is display attribution.
At startup, pending/running attempts interrupted by shutdown are failed. A regeneration keeps the previous completed
summary visible until its replacement completes; public readers receive completed content only, never diagnostics.

`SummaryBuildOptions` keeps summaries text-only unless `include_images` is set. The input digest includes that flag and
the selected blobs' SHA-256, MIME types, and sizes, so a distinct image selection cannot reuse a text-only cache row.
Candidates are `media` artifacts only: `jpg`/`jpeg`, `png`, `webp`, `gif`, and `avif`, capped at four images, 5 MiB each,
and 12 MiB in aggregate. Anthropic HTTP, OpenAI-compatible HTTP, and Codex support images; Claude CLI does not. The core
remains synchronous: the server puts provider work in its blocking boundary rather than introducing async to
`archivr-core`.

Entry free-text search includes summary text (and generated JSON tags inside it) from the latest completed summary only.
Pending and failed rows do not match, and a newer pending or failed request does not hide an older completed summary.

Per-archive layout (created by `archivr init`): `.archivr/` (name, store_path, `archivr.sqlite`) + sibling `store/` (`raw/`, `raw_tweets/`, `structured/`, `temp/`). Server-level auth lives in a **separate** `archivr-auth.sqlite` (users, sessions, API tokens, role bits GUEST=1/USER=2/ADMIN=4/OWNER=8; per-action role masks (e.g. `reorder_children_role_bits`) and per-provider thread-title models (`title_model_<kind>`) live on its `instance_settings` row).

The server mounts multiple archives from a TOML registry (`crates/archivr-server/src/registry.rs`); routes are parameterized by `:archive_id`.

## Key Directories

| Path | Purpose |
|---|---|
| `crates/archivr-core/src/` | Domain logic: `capture.rs`, `archive.rs`, `database.rs` (~2700 lines, all schema/CRUD), `downloader/` |
| `crates/archivr-server/src/` | `main.rs` (bootstrap), `routes.rs` (~3000 lines: AppState, handlers, middleware), `auth.rs`, `registry.rs` |
| `crates/archivr-cli/src/` | CLI entry point |
| `frontend/src/` | React app: `App.jsx` (root state + custom routing), `api.js` (fetch client), `components/`, `styles.css` |
| `docs/` | User docs (`README.md`), `superpowers/plans/` and `superpowers/specs/` (dated design docs — write plans there before large features) |
| `modules/nixos/` | NixOS module (`services.archivr-server`) |
| `.github/workflows/` | `update-ytdlp.yml` — weekly cron that PRs a yt-dlp version bump into `flake.nix` |
| `vendor/twitter/` | Vendored Twitter scraper (active; the Python the server shells out to). Don't refactor casually. |
| `vendor/readability/` | Mozilla `Readability.js`, concatenated into the SingleFile reader-mode browser script by `downloader/singlefile.rs`. |
| `testing/` | Legacy scraping scripts + sample data. `testing/creds.txt` holds real tokens — never read, commit, or print it. |

## Development Commands

```bash
# Rust
cargo build                        # whole workspace
cargo test                         # all unit tests
cargo test -p archivr-core         # single crate
cargo build --release -p archivr-server

# Frontend (Bun, from frontend/)
bun install
bun run dev                        # Vite dev server
bun run build                      # → ../crates/archivr-server/static (gitignored; only needed for bare cargo run)

# Nix
nix develop                        # devshell: yt-dlp, deno, nushell, uv, twitter-api-client
nix build .#archivr-server         # also .#archivr-cli, .#archivr-all; builds frontend automatically
                                   # If frontendDeps hash is stale (after bun.lock/package.json change):
                                   #   nix build 2>&1 | grep "got:" → paste hash into flake.nix frontendDepsHash

# Docker
docker compose up -d               # port 8080; config in ./config/archivr-server.toml (see docker/config.example.toml)

# Run server locally
cargo run -p archivr-server -- path/to/archivr-server.toml
```

No CI is configured; no rustfmt.toml/clippy.toml — default `cargo fmt`/`clippy` settings apply.

## Code Conventions & Common Patterns

- **Errors**: `anyhow::Result<T>` everywhere in core; no custom error enums. The server converts to `ApiError { status, message }` (`routes.rs`) with helpers `not_found`/`bad_request`/`unauthorized`/`forbidden`; any `anyhow` error maps to 500.
- **Async**: Tokio only at the server boundary (`#[tokio::main]`, async handlers, `tokio::spawn` for the 24h session-cleanup task). **Core is synchronous** — blocking `reqwest`, subprocess downloaders, sync `rusqlite`. Keep it that way; don't introduce async into archivr-core.
- **State**: Axum `AppState { registry, auth_db_path, login_attempts }` via `State` extractor; middleware stack = `setup_guard` (503 until owner exists) → `login_rate_limit` (5/15min per IP) → `security_headers`.
- **Auth extraction**: `AuthUser` implements `FromRequestParts` — session cookie (`session`) or `Authorization: Bearer` token (stored SHA3-256-hashed). Passwords are Argon2. Read-scope Bearer tokens are limited to GET/HEAD/OPTIONS by `token_scope.rs`. Target-aware admin rules (manage, not self, last owner) go through `guards.rs`; don't re-implement them inline. Keep `file://` capture locators limited to staged uploads (`temp/uploads/`) in the API; the CLI is the only place local paths are read directly.
- **Logging**: `eprintln!` with `info:`/`warn:` prefixes. No `tracing`/`log` — don't add structured logging piecemeal.
- **External tools by env var**: `ARCHIVR_YT_DLP`, `ARCHIVR_DENO`, `ARCHIVR_JS_RUNTIME`, `ARCHIVR_CHROME`, `ARCHIVR_SINGLE_FILE`, `ARCHIVR_TWEET_PYTHON`, `ARCHIVR_TWEET_SCRAPER`, `ARCHIVR_STATIC_DIR`, `ARCHIVR_BIND`, `ARCHIVR_FFMPEG`. Local transcription: `ARCHIVR_TRANSCRIBE_ENGINES` (the gate; unset = off), `ARCHIVR_TRANSCRIBE_TIMEOUT` (default 3600s, whole job), `ARCHIVR_WHISPER_BACKEND` (`whisper_cpp`|`script`) / `ARCHIVR_WHISPER_CLI` / `ARCHIVR_WHISPER_MODEL` / `ARCHIVR_WHISPER_LANGUAGES`, `ARCHIVR_PARAKEET_CLI` / `ARCHIVR_PARAKEET_MODEL` / `ARCHIVR_PARAKEET_LANGUAGES`, `ARCHIVR_PHONON2_CLI` / `ARCHIVR_PHONON2_MODEL`. Downloaders shell out to subprocesses; resolve binaries through these vars. Env helpers (`required_env`, `env_or`, `optional_env`, `env_timeout`, `resolve_cli`) live in `env_config.rs`; bounded subprocesses go through `process::run_with_timeout` (yt-dlp calls keep `ytdlp.rs`'s private runner).
- **yt-dlp is resolved, not just read**: `ARCHIVR_YT_DLP` (set by the flake wrappers) is only the
  *pinned candidate* handed to `resolve_yt_dlp()` (`downloader/ytdlp.rs`), which compares it against a
  self-updated copy in the state dir. `ARCHIVR_YT_DLP_FORCE` (absolute path) bypasses that comparison
  entirely; `archivr yt-dlp status` shows and chooses that forced candidate when it applies; `ARCHIVR_STATE_DIR`
  relocates the state dir. The JS runtime works the same way: `resolve_js_runtime()` (`downloader/js_runtime.rs`)
  picks the newest Deno ≥ 2.3.0 from `ARCHIVR_DENO` and `<state_dir>/deno/deno` (ties → state dir), then PATH;
  `ARCHIVR_JS_RUNTIME=RUNTIME[:ABS_PATH]` forces it. Both caches are `RwLock`s refreshed by `refresh_yt_dlp()` /
  `refresh_js_runtime()` after each successful component install (`ytdlp_tools::update_tools`), so a UI update needs
  no restart; `resolve_js_runtime()` returns an owned `Option<JsRuntime>`. Never cache a resolved path across
  operations. Never spawn bare yt-dlp — build every command with
  `yt_dlp_command()` so the resolver-chosen binary *and* `--js-runtimes` args are applied.
- **LLM summaries by env var**: `ARCHIVR_ANTHROPIC_API_KEY` / `ARCHIVR_ANTHROPIC_URL` / `ARCHIVR_ANTHROPIC_MODEL`, `ARCHIVR_OPENAI_API_KEY` / `ARCHIVR_OPENAI_URL` / `ARCHIVR_OPENAI_MODEL`, `ARCHIVR_CLAUDE_CLI` / `ARCHIVR_CLAUDE_MODEL`, `ARCHIVR_CODEX_CLI` / `ARCHIVR_CODEX_MODEL`, plus `ARCHIVR_SUMMARY_HTTP_TIMEOUT` (default 120s) and `ARCHIVR_SUMMARY_CLI_TIMEOUT` (default 300s). Thread titles use `ARCHIVR_ANTHROPIC_TITLE_MODEL` (default `claude-haiku-4-5`), `ARCHIVR_OPENAI_TITLE_MODEL` (`gpt-4o-mini`), `ARCHIVR_CLAUDE_TITLE_MODEL` (`haiku`), `ARCHIVR_CODEX_TITLE_MODEL` (`gpt-6-luna`, override if unavailable) — never the summary model; `thread_title.rs` reuses provider transports via `summarizer::complete_plain`. The one exception to env-only config: admins may override the title model per provider in the auth DB (`instance_settings.title_model_{anthropic_http,openai_compatible,claude_cli,codex_cli}`; PATCH `/api/admin/instance-settings` trims, blank clears, rejects overlong or whitespace/control chars; GET returns `title_models.<kind>` with `model`/`source`/`fallback`). Precedence instance > `ARCHIVR_*_TITLE_MODEL` > default; the server passes the instance value to core as an `Option<&str>` override — core never reads the auth DB. Same convention as above — never TOML, which also keeps API keys out of anything the archive persists. Summaries are manual-only: nothing in `capture.rs` triggers them. The two CLI vars are optional overrides: unset, `resolve_cli()` auto-discovers well-known absolute installs first (`/opt/homebrew/bin/claude`, `/usr/local/bin/claude`; `/Applications/ChatGPT.app/Contents/Resources/codex`, `/opt/homebrew/bin/codex`, `/usr/local/bin/codex`), then `$HOME/.local/bin/<name>`, then the bare name on PATH — the absolute defaults matter because the ChatGPT desktop app ships `codex` off PATH. Frontend static output is generated; never hand-edit `crates/archivr-server/static/`.
- **Frontend**: JSX (no TypeScript), PascalCase components in `frontend/src/components/`, kebab-case CSS classes, plain CSS with custom properties in `styles.css` (no Tailwind/CSS-in-JS). No router — `App.jsx` parses `window.location.pathname` + `history.pushState`. State = `useState` + one `AuthContext`; `sessionStorage` for refresh-resilient dialog state (see `CaptureDialog.jsx` job polling, 500ms). All API calls through `frontend/src/api.js` with relative `/api/*` URLs — add new endpoints there, not inline `fetch`. Summary polling and generate callbacks must remain scoped to the currently selected entry; image-inclusion behavior is unchanged. In-progress captures render through `SkeletonEntryRow.jsx`, which is a compact spinner + locator + "Archiving…" line (with a playlist/channel hint), **not** a grey skeleton block — don't reintroduce placeholder shimmer. Layout comes from semantic classes (e.g. `.capture-text-row` in `styles.css`), never from fallthrough on a generic row class; give a new row shape its own class.
- **CLI providers parse a file, not stdout**: `codex` is invoked as `codex exec --output-last-message <tempfile> -` (prompt on stdin) and the reply is read back from that file — raw stdout carries a runtime header, an echo of the user prompt, and a `tokens used` footer that the JSON extractor will happily mistake for the answer. There is a positional-prompt fallback for older builds that reject `-`. Keep any new CLI provider on the same "give me only the final message" contract.
- **Transcription engines follow the same rule**: whisper.cpp writes `-ovtt`; script engines (Whisper `script`, Parakeet) are run as `<script> --input <wav> --output <vtt> --model <m> [--language xx]` and must write WebVTT to `--output` (optional `<output>.lang`); the only stdout parsed is Phonon-2's documented `--json`, converted to VTT by `phonon_json_to_vtt`. Phonon-2 is English-only, hard-coded. One job at a time (process-wide slot).
- **Log prefixes**: `info:`/`warn:` only. The one exception is the yt-dlp installer's `warning: python3 was not found on PATH …` (`ytdlp_tools.rs`), kept verbatim for byte-identical CLI stderr.
- **Naming (Rust)**: standard snake_case/PascalCase; visibility and roles are bitflag `u32`s, not enums.

## Important Files

- `crates/archivr-server/src/main.rs` — server bootstrap: config load, archive mounting, auth DB init, stalled-job recovery (running → failed on startup), X Article title backfill (`capture::backfill_x_article_titles`; idempotent, startup only — CLI-only installs never run it).
- `crates/archivr-server/src/routes.rs` — HTTP router and most handlers; grep here first for API work. Newer account/admin/job handlers live in the modules below, but their routes are still registered in this file's router.
- `crates/archivr-server/src/admin_users.rs` — admin user delete and status, role CRUD (`/api/admin/roles`), and user role assignment.
- `crates/archivr-server/src/credentials.rs` — own sessions (`/api/auth/sessions`), API token scope/expiry, admin password reset and per-user session/token revocation.
- `crates/archivr-server/src/guards.rs` — `ensure_can_manage`, `ensure_not_self`, `ensure_not_last_owner` shared by the admin and credential handlers.
- `crates/archivr-server/src/token_scope.rs` — middleware that enforces read-scope tokens (GET/HEAD/OPTIONS only).
- `crates/archivr-server/src/jobs.rs` — capture job list and detail (`created_by` filter, `items_truncated` at 200 items), with the `created_by` visibility rule.
- `crates/archivr-server/src/effective_config.rs` — `GET /api/admin/effective-config`, the `ENV_VARS` registry and its drift test, and `GET /api/archives/:id/info`.
- `crates/archivr-server/src/entry_access.rs` — router tests for entry visibility. Every handler that takes an entry uid (or a blob sha) must call `routes::ensure_entry_visible` (or `database::caller_can_access_blob`) so a hidden entry answers 404 like a missing one.
- `crates/archivr-server/src/test_support.rs` — shared test helpers (router, auth DB and user/session setup) for the `oneshot` tests.
- `crates/archivr-core/src/capture.rs` — `perform_capture()`, `Source` enum, shorthand parsing; tweet titles prefer an X Article's `article.title` (`<title> — @handle`).
- `crates/archivr-core/src/downloader/ytdlp.rs` — every yt-dlp shell-out (playlist/channel probe and download, sync mode, subtitle planning/args/staging, the combined media+subtitle call with its media-only retry, and the subtitles-only `download_subtitles`) **plus** the binary resolver: `resolve_yt_dlp()`, `state_dir()`, `probe_version()`.
- `crates/archivr-core/src/subtitles.rs` — `subtitle` artifacts: archive/register (deduped per entry+blob), `subtitle_to_transcript()` (VTT/SRT reduction, rolling-caption dedup), `subtitle_track_rank()`, and summary-time `fetch_subtitles_for_entry()`.
- `crates/archivr-core/src/downloader/text.rs` — pasted-text staging + hashing (`save()` → `StagedText`); accepts only `text/plain` and `text/markdown`.
- `crates/archivr-core/src/summarizer.rs` — the `SummaryProvider` trait and its four implementations (Anthropic HTTP, OpenAI-compatible HTTP, `claude` CLI, `codex` CLI), `PROMPT_VERSION`, `resolve_cli()`, prompt assembly, `build_summary_input()` (artifact selection + HTML/text/JSON reduction; YouTube videos via subtitle transcript), `build_summary_input_with_subtitle_fetch()`, and the no-subtitles error/copy.
- `crates/archivr-core/src/transcriber.rs` — local transcription: engine config from env (`transcriber_from_env`, `available_transcribers`), whisper.cpp/script/Phonon-2 adapters, audio acquisition + ffmpeg, the process-wide job slot, `transcribe_entry`, user-facing error copy.
- `crates/archivr-core/src/process.rs` — `run_with_timeout` (kill on deadline, stderr tail) and the `ProcessTimedOut` sentinel; also backs summarizer `run_cli`.
- `crates/archivr-core/src/env_config.rs` — shared `ARCHIVR_*` env helpers (`required_env`, `env_or`, `optional_env`, `env_timeout`, `resolve_cli`).
- `crates/archivr-core/src/thread_title.rs` — thread-title prompt, topic sanitization, `Thread about … — @author` format, per-provider title model via `resolve_title_model` (precedence: instance setting `instance_settings.title_model_<kind>` passed in by the server > `ARCHIVR_*_TITLE_MODEL` > cheap default; core never reads the auth DB).
- `crates/archivr-core/src/downloader/ytdlp_tools.rs` — yt-dlp + Deno update orchestration (`update_tools`, `install_yt_dlp`) and the status model (`tools_status`), shared by `archivr yt-dlp` and `/api/admin/yt-dlp[/update]`.
- `crates/archivr-core/src/downloader/js_runtime.rs` — JS runtime for yt-dlp: `ARCHIVR_JS_RUNTIME` parsing, `DenoVersion`, `deno_candidates()`, `JsRuntimeRole` + `resolve_js_runtime_with_role()` (uncached; reports the winning slot so `status` stars exactly one row), `resolve_js_runtime()` (cached `RwLock`, owned clone, warns per resolution), `refresh_js_runtime()` and `js_runtime_args()`.
- `crates/archivr-cli/src/main.rs` — CLI entry point; `archivr yt-dlp update|status` is a thin renderer over core `ytdlp_tools`.
- `crates/archivr-core/src/downloader/deno_install.rs` — Deno release lookup, per-platform asset, staged + `--version`-verified atomic install into `<state_dir>/deno/deno`.
- `.github/workflows/update-ytdlp.yml` — weekly (`0 6 * * 1`) + manual auto-bump of the `flake.nix` yt-dlp pin.
- `crates/archivr-core/src/database.rs` — single source of truth for all SQLite schema and queries (both archive and auth DBs).
- `frontend/src/App.jsx` / `frontend/src/api.js` — frontend root state and API surface.
- `frontend/src/components/CaptureDialog.jsx` — `CaptureRow` (locator input, playlist quality selectors) and `CaptureTextRow` (the "Add text" flow), plus job polling.
- `frontend/src/components/ContextRail.jsx` — the Summary section (provider selector, local-transcription engine selector, generate/regenerate, polling), the thread **Generate title** button, and the bulk panel's **Generate titles** (`handleBulkGenerateTitles`: ≥2 selected with ≥1 `tweet_thread`; skips non-threads; rail's Summary provider; per-entry `thread-title` calls 2 at a time; progress + `X updated, Y failed`; a selection change stops picking up new entries).
- `frontend/src/components/SettingsView.jsx` — Settings, incl. the admin `YtDlpSection` (Instance › yt-dlp status + update).
- `frontend/src/components/TextPreview.jsx` — preview renderer for text/markdown entries.
- `docker/config.example.toml` — server config schema: `bind`, `auth_db_path`, repeated `[[archives]]` (`id`, `label`, `archive_path`).
- `flake.nix`, `modules/nixos/archivr-server.nix`, `Dockerfile`, `docker-compose.yml` — deployment surfaces; config schema changes must be reflected in all of them plus `docs/README.md`.

## Runtime/Tooling Preferences

- **Rust edition 2024** (root `Cargo.toml`); shared deps live in `[workspace.dependencies]` — add new deps there and reference with `workspace = true`.
- **Bun** is the frontend package manager (`frontend/bun.lock`); use `bun`, not npm/yarn.
- Runtime binaries the app expects on PATH or via env vars: `yt-dlp`, Deno (≥ 2.3.0, YouTube challenge solving), Chromium, `single-file` (Node), Python 3 with `twitter-api-client`, `ffmpeg`. `nix develop` provides the dev subset.
- `.gitignore` is **default-deny with an allowlist** — new top-level files/dirs are invisible to git until explicitly allowed there.
- Frontend build output (`crates/archivr-server/static/`) is generated by the `frontendStatic` Nix derivation; never hand-edit it, and do not commit it — it is excluded from git tracking. `nix build` is the standard workflow everywhere (local and NixOS) and builds the frontend automatically. `bun run build` only needed for bare `cargo run` one-off testing. When `bun.lock` or `package.json` changes, update `frontendDepsHash` in `flake.nix` for each system by running `nix build 2>&1 | grep "got:"` and pasting the reported hash.
- **yt-dlp is pinned to a specific GitHub release** in the `ytDlp` derivation in `flake.nix` (zipapp
  fetched from `github.com/yt-dlp/yt-dlp/releases`, wrapped with `python312` + `ffmpeg`) — not taken
  from nixpkgs. Both the `archivr` and `archivr-server` wrappers set `ARCHIVR_YT_DLP` from it. Three
  ways to bump: the weekly `Update yt-dlp` workflow (automatic PR), `archivr yt-dlp update`
  (per-machine, into the state dir), or editing the three fields of the `ytDlp` block by hand. Which
  binary actually runs is decided at runtime by `resolve_yt_dlp()` in `downloader/ytdlp.rs`; use
  `archivr yt-dlp status` to see the candidates and the winner.
- **Deno** comes from nixpkgs `pkgs.deno` in both wrappers (`ARCHIVR_DENO`, also on their PATH) and the devShell.
  The `Dockerfile` pins Deno 2.9.7 with a sha256 per arch — no auto-bump; change version and both hashes together.
  Docker installs yt-dlp via pip as `"yt-dlp[default]==<version>"`; the `[default]` extra pulls `yt-dlp-ejs`
  (the challenge solver) — dropping it brings back YouTube 403s even with Deno. The weekly `Update yt-dlp`
  workflow only bumps `flake.nix`, never the Dockerfile pin. Docker sets `ARCHIVR_STATE_DIR=/data/archivr-state`
  (on the persistent `/data` volume) so in-container `archivr yt-dlp update` survives restarts. On NixOS
  without `programs.nix-ld` the upstream Deno can't execute: `install_deno` (`archivr-core/src/downloader/deno_install.rs`)
  detects the spawn `NotFound`, skips, and keeps the pinned `ARCHIVR_DENO` if it is usable (≥ 2.3.0) —
  reported as ok, not a failure (exit 0 unless yt-dlp itself failed); with no usable pin it errors.

## Testing & QA

- **Rust**: unit tests only, in `#[cfg(test)]` modules inside source files (e.g. `capture.rs`, `database.rs`, `registry.rs`, `routes.rs`, `hash.rs`, and newer: `summarizer.rs` — 53 tests over provider construction, CLI/env resolution, output extraction, HTML/text/JSON input reduction, tweet-thread joining, YouTube subtitle selection/digest/no-subtitles errors and the transcription fallback order; `downloader/ytdlp.rs` — tests covering resolver priority and refresh, version tie-breaking, `yt_dlp_command_with` args, subtitle planning, argument construction, staging, the media-only retry decision and the timeout runner; `downloader/js_runtime.rs` — runtime spec parsing, Deno version parsing, candidate priority/ties/minimum version and the winning role, PATH fallback via `resolve_js_runtime_with_path` (never mutate the process `PATH`), refresh and `--js-runtimes` args; `downloader/deno_install.rs` — 7 tests: asset selection, release parsing and `verify_staged` (version mismatch, the "cannot execute" case); `downloader/ytdlp_tools.rs` — 2; `subtitles.rs` — 13 tests over VTT/SRT reduction, rolling-caption dedup, track ranking and artifact dedup; `transcriber.rs` — 40 (env config, argument builders, Phonon JSON → VTT against a real sample, language gating, end-to-end with fake engines); `process.rs` — 5; `env_config.rs` — 2; `thread_title.rs` — 8). Test locks: tests that set resolver env vars take `downloader::RESOLVER_ENV_LOCK` and call `refresh_*()` on cleanup so the cache never points into a deleted tempdir; tests that set transcription env vars or run `transcribe_entry` (including `summarizer.rs`'s fallback tests) take `transcriber::TRANSCRIBE_TEST_LOCK`; `summarizer.rs` and `env_config.rs` provider-env tests use their module-local `ENV_LOCK`. Tests that exec a freshly written script write it to a fresh path and wait out ETXTBSY first (`fake_deno`/`deno_install::tests::write_script` in core, `write_script` in the CLI). No `tests/` integration dir. Server `oneshot` tests share their helpers in `crates/archivr-server/src/test_support.rs` (router, auth DB, owner/admin/user/guest session fixtures, API token setup); reuse those rather than copying setup code. Patterns: `tempfile` for scratch archives, config round-trip assertions, regex/parser validation. Run `cargo test` or `cargo test -p <crate>`.
- **Env var registry**: when you add an `ARCHIVR_*` env var, add it to `ENV_VARS` in `crates/archivr-server/src/effective_config.rs` (name, group, description, secret flag, default). `GET /api/admin/effective-config` serves that table (secrets as `set` only, URLs without userinfo/query), and a drift test greps every crate's `src/**/*.rs` for `"ARCHIVR_*"` literals and fails on any missing entry (test-only `ARCHIVR_TEST_*` names are ignored).
- **Frontend**: component render tests are colocated `*.test.jsx` files on `bun:test` (`bun test` from `frontend/`); there is no Storybook.
- Manual smoke test for server changes: build frontend, `cargo run -p archivr-server -- <config.toml>`, exercise `/api/*`.
