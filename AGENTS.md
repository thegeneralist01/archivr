# Repository Guidelines

## Project Overview

Archivr is a self-hosted archival tool that captures and preserves digital content — YouTube/Twitter/Instagram/TikTok/Reddit posts, arbitrary URLs, full web pages (via SingleFile + Chromium), and local files — into self-contained, SQLite-backed archive directories with blob deduplication, hierarchical tags, collections, and role-based auth. Rust workspace + React frontend.

Read `ARCHIVR-MENTAL-MODEL.md` before making structural changes; `NEXT.md` tracks the roadmap (Tracks 1–7 done, Track 8 = Collections UI is next).

## Architecture & Data Flow

Three crates with a strict ownership split — **core owns truth; CLI and server are adapters**:

- `crates/archivr-core` — domain library: capture orchestration, SQLite schema/CRUD, downloaders, hashing. New archive features start here.
- `crates/archivr-server` — Axum HTTP API + auth + static frontend serving.
- `crates/archivr-cli` — clap-based CLI (`archivr` binary): `init`, `archive` subcommands.

Capture flow: locator → `determine_source()` (`crates/archivr-core/src/capture.rs`) routes by platform/shorthand (`yt:`, `x:`, `tweet:` …) → platform downloader (`downloader/ytdlp.rs`, `tweets.rs`, `singlefile.rs`, `http.rs`, `local.rs`) stages into `temp/` → SHA3-256 dedup (`hash.rs`, `downloader/store.rs`) moves blobs to `raw/A/B/HASH.EXT` → rows written to `archivr.sqlite` (runs, entries, artifacts, blobs) → served via `/api/archives/:id/...`.

Per-archive layout (created by `archivr init`): `.archivr/` (name, store_path, `archivr.sqlite`) + sibling `store/` (`raw/`, `raw_tweets/`, `structured/`, `temp/`). Server-level auth lives in a **separate** `archivr-auth.sqlite` (users, sessions, API tokens, role bits GUEST=1/USER=2/ADMIN=4/OWNER=8).

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
| `vendor/twitter/` | Vendored Twitter scraper (active; the Python the server shells out to). Don't refactor casually. |
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
bun run storybook                  # Storybook on :6006

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
- **Frontend**: JSX (no TypeScript), PascalCase components in `frontend/src/components/`, kebab-case CSS classes, plain CSS with custom properties in `styles.css` (no Tailwind/CSS-in-JS). No router — `App.jsx` parses `window.location.pathname` + `history.pushState`. State = `useState` + one `AuthContext`; `sessionStorage` for refresh-resilient dialog state (see `CaptureDialog.jsx` job polling, 500ms). All API calls through `frontend/src/api.js` with relative `/api/*` URLs — add new endpoints there, not inline `fetch`.
- **Naming (Rust)**: standard snake_case/PascalCase; visibility and roles are bitflag `u32`s, not enums.

## Important Files

- `crates/archivr-server/src/main.rs` — server bootstrap: config load, archive mounting, auth DB init, stalled-job recovery (running → failed on startup).
- `crates/archivr-server/src/routes.rs` — all HTTP handlers and the router; grep here first for API work.
- `crates/archivr-core/src/capture.rs` — `perform_capture()`, `Source` enum, shorthand parsing.
- `crates/archivr-core/src/database.rs` — single source of truth for all SQLite schema and queries (both archive and auth DBs).
- `frontend/src/App.jsx` / `frontend/src/api.js` — frontend root state and API surface.
- `docker/config.example.toml` — server config schema: `bind`, `auth_db_path`, repeated `[[archives]]` (`id`, `label`, `archive_path`).
- `flake.nix`, `modules/nixos/archivr-server.nix`, `Dockerfile`, `docker-compose.yml` — deployment surfaces; config schema changes must be reflected in all of them plus `docs/README.md`.

## Runtime/Tooling Preferences

- **Rust edition 2024** (root `Cargo.toml`); shared deps live in `[workspace.dependencies]` — add new deps there and reference with `workspace = true`.
- **Bun** is the frontend package manager (`frontend/bun.lock`); use `bun`, not npm/yarn.
- Runtime binaries the app expects on PATH or via env vars: `yt-dlp`, Chromium, `single-file` (Node), Python 3 with `twitter-api-client`, `ffmpeg`. `nix develop` provides the dev subset.
- `.gitignore` is **default-deny with an allowlist** — new top-level files/dirs are invisible to git until explicitly allowed there.
- Frontend build output (`crates/archivr-server/static/`) is generated; never hand-edit it.

## Testing & QA

- **Rust**: unit tests only, in `#[cfg(test)]` modules inside source files (e.g. `capture.rs`, `database.rs`, `registry.rs`, `routes.rs`, `hash.rs`). No `tests/` integration dir. Patterns: `tempfile` for scratch archives, config round-trip assertions, regex/parser validation. Run `cargo test` or `cargo test -p <crate>`.
- **Frontend**: no test framework. Storybook (`bun run storybook`) is the component QA surface — stories are colocated `*.stories.jsx` files; add one when adding a nontrivial component.
- Manual smoke test for server changes: build frontend, `cargo run -p archivr-server -- <config.toml>`, exercise `/api/*`.
