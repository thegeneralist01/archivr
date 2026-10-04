# Repository Guidelines

## Project Overview

archivr is a self-hosted archival tool that captures and preserves digital content — YouTube/Twitter/Instagram/TikTok/Reddit posts, arbitrary URLs, full web pages (via SingleFile + Chromium, optionally through a Freedium mirror for paywalled articles), and local files — into self-contained, SQLite-backed archive directories with blob deduplication, hierarchical tags, collections, and role-based auth. Rust workspace + React frontend.

Read `ARCHIVR-MENTAL-MODEL.md` before making structural changes.

## Architecture & Data Flow

Three crates with a strict ownership split — **core owns truth; CLI and server are adapters**:

- `crates/archivr-core` — domain library: capture orchestration, SQLite schema/CRUD, downloaders, hashing. New archive features start here.
- `crates/archivr-server` — Axum HTTP API + auth + static frontend serving.
- `crates/archivr-cli` — clap-based CLI (`archivr` binary): `init`, `archive` subcommands.

Capture flow: locator → `determine_source()` (`crates/archivr-core/src/capture.rs`) routes by platform/shorthand (`yt:`, `x:`, `tweet:` …) → platform downloader (`downloader/ytdlp.rs`, `tweets.rs`, `singlefile.rs`, `http.rs`, `local.rs`) stages into `temp/` → SHA3-256 dedup (`hash.rs`, `downloader/store.rs`) moves blobs to `raw/A/B/HASH.EXT` → rows written to `archivr.sqlite` (runs, entries, artifacts, blobs) → served via `/api/archives/:id/...`. `CaptureConfig` carries per-request toggles (uBlock, reader mode, Freedium mirror, etc.); when `via_freedium` is set, the fetch URL is rewritten through `freedium-mirror.cfd` while the canonical DB URL stays the original locator.

YouTube playlists and channels produce a **parent container entry** with each video captured as a child entry. `downloader/ytdlp.rs` handles the flat-playlist probe (fetching per-video quality metadata before archiving); the multi-video download loop and sync mode (skipping already-archived videos when re-archiving a playlist or channel) live in `capture.rs`. Child order is persisted in `archived_entries.position` (append-only at insert in `database::create_archived_entry`; rewritten only by `database::reorder_child_entries` behind `PUT …/entries/:entry_uid/children/order` (allowed roles = `InstanceSettings::reorder_children_role_bits`, default ADMIN|OWNER, Owner-editable)).

Pasted text takes a much shorter path: `perform_text_capture()` (`capture.rs`) skips source detection
and every downloader shell-out — `downloader/text.rs` stages the body under `temp/`, hashes it, and the
blob lands in `raw/` like any other artifact. Entrypoint is
`POST /api/archives/:archive_id/captures/text`. Its body is byte-preserving, its entry has no fabricated
`original_url`, and its normal text preview opens from the entry rail.

LLM summaries are a post-capture, manual-only subsystem: `crates/archivr-core/src/summarizer.rs` behind
`GET`/`POST /api/archives/:archive_id/entries/:entry_uid/summary`, cached in the `entry_summaries`
table per (entry, provider, model, prompt version, input hash). See `ARCHIVR-MENTAL-MODEL.md` for the
provider set and the status lifecycle.

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

Per-archive layout (created by `archivr init`): `.archivr/` (name, store_path, `archivr.sqlite`) + sibling `store/` (`raw/`, `raw_tweets/`, `structured/`, `temp/`). Server-level auth lives in a **separate** `archivr-auth.sqlite` (users, sessions, API tokens, role bits GUEST=1/USER=2/ADMIN=4/OWNER=8; per-action role masks (e.g. `reorder_children_role_bits`) live on its `instance_settings` row).

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
bun run build                      # → ../crates/archivr-server/static (served by the server)

# Nix
nix develop                        # devshell: yt-dlp, nushell, uv, twitter-api-client
nix build .#archivr-server         # also .#archivr-cli, .#archivr-all

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
- **Auth extraction**: `AuthUser` implements `FromRequestParts` — session cookie (`session`) or `Authorization: Bearer` token (stored SHA3-256-hashed). Passwords are Argon2.
- **Logging**: `eprintln!` with `info:`/`warn:` prefixes. No `tracing`/`log` — don't add structured logging piecemeal.
- **External tools by env var**: `ARCHIVR_YT_DLP`, `ARCHIVR_CHROME`, `ARCHIVR_SINGLE_FILE`, `ARCHIVR_TWEET_PYTHON`, `ARCHIVR_TWEET_SCRAPER`, `ARCHIVR_STATIC_DIR`, `ARCHIVR_BIND`. Downloaders shell out to subprocesses; resolve binaries through these vars.
- **yt-dlp is resolved, not just read**: `ARCHIVR_YT_DLP` (set by the flake wrappers) is only the
  *pinned candidate* handed to `resolve_yt_dlp()` (`downloader/ytdlp.rs`), which compares it against a
  self-updated copy in the state dir. `ARCHIVR_YT_DLP_FORCE` (absolute path) bypasses that comparison
  entirely; `archivr yt-dlp status` shows and chooses that forced candidate when it applies; `ARCHIVR_STATE_DIR`
  relocates the state dir. Never spawn bare `yt-dlp` — call the resolver.
- **LLM summaries by env var**: `ARCHIVR_ANTHROPIC_API_KEY` / `ARCHIVR_ANTHROPIC_URL` / `ARCHIVR_ANTHROPIC_MODEL`, `ARCHIVR_OPENAI_API_KEY` / `ARCHIVR_OPENAI_URL` / `ARCHIVR_OPENAI_MODEL`, `ARCHIVR_CLAUDE_CLI` / `ARCHIVR_CLAUDE_MODEL`, `ARCHIVR_CODEX_CLI` / `ARCHIVR_CODEX_MODEL`, plus `ARCHIVR_SUMMARY_HTTP_TIMEOUT` (default 120s) and `ARCHIVR_SUMMARY_CLI_TIMEOUT` (default 300s). Same convention as above — never TOML, which also keeps API keys out of anything the archive persists. Summaries are manual-only: nothing in `capture.rs` triggers them. The two CLI vars are optional overrides: unset, `resolve_cli()` auto-discovers well-known absolute installs first (`/opt/homebrew/bin/claude`, `/usr/local/bin/claude`; `/Applications/ChatGPT.app/Contents/Resources/codex`, `/opt/homebrew/bin/codex`, `/usr/local/bin/codex`), then `$HOME/.local/bin/<name>`, then the bare name on PATH — the absolute defaults matter because the ChatGPT desktop app ships `codex` off PATH. Frontend static output is generated; never hand-edit `crates/archivr-server/static/`.
- **Frontend**: JSX (no TypeScript), PascalCase components in `frontend/src/components/`, kebab-case CSS classes, plain CSS with custom properties in `styles.css` (no Tailwind/CSS-in-JS). No router — `App.jsx` parses `window.location.pathname` + `history.pushState`. State = `useState` + one `AuthContext`; `sessionStorage` for refresh-resilient dialog state (see `CaptureDialog.jsx` job polling, 500ms). All API calls through `frontend/src/api.js` with relative `/api/*` URLs — add new endpoints there, not inline `fetch`. Summary polling and generate callbacks must remain scoped to the currently selected entry; image-inclusion behavior is unchanged. In-progress captures render through `SkeletonEntryRow.jsx`, which is a compact spinner + locator + "Archiving…" line (with a playlist/channel hint), **not** a grey skeleton block — don't reintroduce placeholder shimmer. Layout comes from semantic classes (e.g. `.capture-text-row` in `styles.css`), never from fallthrough on a generic row class; give a new row shape its own class.
- **CLI providers parse a file, not stdout**: `codex` is invoked as `codex exec --output-last-message <tempfile> -` (prompt on stdin) and the reply is read back from that file — raw stdout carries a runtime header, an echo of the user prompt, and a `tokens used` footer that the JSON extractor will happily mistake for the answer. There is a positional-prompt fallback for older builds that reject `-`. Keep any new CLI provider on the same "give me only the final message" contract.
- **Naming (Rust)**: standard snake_case/PascalCase; visibility and roles are bitflag `u32`s, not enums.

## Important Files

- `crates/archivr-server/src/main.rs` — server bootstrap: config load, archive mounting, auth DB init, stalled-job recovery (running → failed on startup).
- `crates/archivr-server/src/routes.rs` — all HTTP handlers and the router; grep here first for API work.
- `crates/archivr-core/src/capture.rs` — `perform_capture()`, `Source` enum, shorthand parsing.
- `crates/archivr-core/src/downloader/ytdlp.rs` — every yt-dlp shell-out (playlist/channel probe and download, sync mode) **plus** the binary resolver: `resolve_yt_dlp()`, `state_dir()`, `probe_version()`.
- `crates/archivr-core/src/downloader/text.rs` — pasted-text staging + hashing (`save()` → `StagedText`); accepts only `text/plain` and `text/markdown`.
- `crates/archivr-core/src/summarizer.rs` — the `SummaryProvider` trait and its four implementations (Anthropic HTTP, OpenAI-compatible HTTP, `claude` CLI, `codex` CLI), `PROMPT_VERSION`, `resolve_cli()`, prompt assembly, and `build_summary_input()` (artifact selection + HTML/text/JSON reduction).
- `crates/archivr-cli/src/main.rs` — CLI entry point, including the `archivr yt-dlp update|status` subcommand (staged, atomic zipapp install into the state dir).
- `.github/workflows/update-ytdlp.yml` — weekly (`0 6 * * 1`) + manual auto-bump of the `flake.nix` yt-dlp pin.
- `crates/archivr-core/src/database.rs` — single source of truth for all SQLite schema and queries (both archive and auth DBs).
- `frontend/src/App.jsx` / `frontend/src/api.js` — frontend root state and API surface.
- `frontend/src/components/CaptureDialog.jsx` — `CaptureRow` (locator input, playlist quality selectors) and `CaptureTextRow` (the "Add text" flow), plus job polling.
- `frontend/src/components/ContextRail.jsx` — the Summary section: provider selector, generate/regenerate, and summary polling.
- `frontend/src/components/TextPreview.jsx` — preview renderer for text/markdown entries.
- `docker/config.example.toml` — server config schema: `bind`, `auth_db_path`, repeated `[[archives]]` (`id`, `label`, `archive_path`).
- `flake.nix`, `modules/nixos/archivr-server.nix`, `Dockerfile`, `docker-compose.yml` — deployment surfaces; config schema changes must be reflected in all of them plus `docs/README.md`.

## Runtime/Tooling Preferences

- **Rust edition 2024** (root `Cargo.toml`); shared deps live in `[workspace.dependencies]` — add new deps there and reference with `workspace = true`.
- **Bun** is the frontend package manager (`frontend/bun.lock`); use `bun`, not npm/yarn.
- Runtime binaries the app expects on PATH or via env vars: `yt-dlp`, Chromium, `single-file` (Node), Python 3 with `twitter-api-client`, `ffmpeg`. `nix develop` provides the dev subset.
- `.gitignore` is **default-deny with an allowlist** — new top-level files/dirs are invisible to git until explicitly allowed there.
- Frontend build output (`crates/archivr-server/static/`) is generated; never hand-edit it.
- **yt-dlp is pinned to a specific GitHub release** in the `ytDlp` derivation in `flake.nix` (zipapp
  fetched from `github.com/yt-dlp/yt-dlp/releases`, wrapped with `python312` + `ffmpeg`) — not taken
  from nixpkgs. Both the `archivr` and `archivr-server` wrappers set `ARCHIVR_YT_DLP` from it. Three
  ways to bump: the weekly `Update yt-dlp` workflow (automatic PR), `archivr yt-dlp update`
  (per-machine, into the state dir), or editing the three fields of the `ytDlp` block by hand. Which
  binary actually runs is decided at runtime by `resolve_yt_dlp()` in `downloader/ytdlp.rs`; use
  `archivr yt-dlp status` to see the candidates and the winner.

## Testing & QA

- **Rust**: unit tests only, in `#[cfg(test)]` modules inside source files (e.g. `capture.rs`, `database.rs`, `registry.rs`, `routes.rs`, `hash.rs`, and newer: `summarizer.rs` — 23 tests over provider construction, CLI/env resolution, output extraction, HTML/text/JSON input reduction and tweet-thread joining; `downloader/ytdlp.rs` — 13 tests, several covering resolver priority and version tie-breaking). No `tests/` integration dir. Patterns: `tempfile` for scratch archives, config round-trip assertions, regex/parser validation. Run `cargo test` or `cargo test -p <crate>`.
- **Frontend**: component render tests are colocated `*.test.jsx` files on `bun:test` (`bun test` from `frontend/`); there is no Storybook.
- Manual smoke test for server changes: build frontend, `cargo run -p archivr-server -- <config.toml>`, exercise `/api/*`.
