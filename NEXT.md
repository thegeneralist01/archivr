# Archivr Next Work Plan

> **For future agentic workers:** Read `ARCHIVR-MENTAL-MODEL.md` first, then this file.
> All six tracks from the previous handoff are complete. This document is the current roadmap.

## Current State

All five product tracks from the prior session are done:

| Feature | Status |
|---|---|
| Entry detail + artifact serving | Done |
| Server-side search with prefix filters | Done |
| Hierarchical tags (CRUD, tree, entry assignment, filter) | Done |
| Browser capture button and POST endpoint | Done |
| Local-only auth boundary, bind address config, docs | Done |

The frontend is a React + Vite app (`frontend/`) that builds into `crates/archivr-server/static/`.

The remaining gaps come from `docs/README.md` milestones that were never in the prior tracks.

---

## Remaining Work (in order)

### ~~1. Generic URL capture — plain file download~~ ✅ Done

**Implemented:** `crates/archivr-core/src/downloader/http.rs` (`download` fn, extension helpers,
HTML rejection), `Source::Url` variant in `capture.rs`, `determine_source` routes all unmatched
`http://`/`https://` URLs to `Source::Url`, `perform_capture` arm calls the downloader and flows
through the existing temp → hash → raw store pipeline. `reqwest 0.12` (blocking) added to workspace.
`docs/README.md` "URLs" milestone checked off. 112 tests green.

---

### ~~2. Web page archiving — single-file HTML snapshots~~ ✅ Done

**Implemented:** `crates/archivr-core/src/downloader/singlefile.rs` (new module; shells out to
`single-file-cli` via `ARCHIVR_SINGLE_FILE` env var; launches headless Chromium via
`ARCHIVR_CHROME`; flags: `--browser-wait-until=networkidle2`, `--remove-unused-styles=false`,
`--block-scripts=false`, `--remove-alternative-medias=false` for full CSS/font fidelity).
`Source::WebPage` variant added to `capture.rs`; HEAD-first probe (`probe_url_kind` in `http.rs`)
routes `text/html` responses to `Source::WebPage` and everything else to `Source::Url`.
Font deduplication implemented as a follow-on: `font_extractor.rs` post-processes the saved HTML,
extracts embedded `@font-face` base64 blobs, stores each as a deduplicated artifact in the raw
store, and rewrites the HTML to reference them via `/api/archives/:id/blobs/:sha256`.
`hash_bytes` added to `hash.rs`; `get_blob_by_sha256` added to `database.rs`;
`GET /api/archives/:id/blobs/:sha256` route added to `routes.rs`.
`flake.nix` updated: `pkgs.single-file-cli` and (Linux-only) `pkgs.chromium` added to
`buildInputs`; `ARCHIVR_SINGLE_FILE` and `ARCHIVR_CHROME` env vars wired into both wrappers.
`docs/README.md` "Archive web pages" milestone checked off. 143 tests green.

---

### 3. Async capture jobs

**What:** Capture currently runs synchronously on the HTTP request thread. A YouTube video,
a large tweet thread, or a slow monolith page will stall the browser request. This track
adds a job queue so `POST /api/archives/:id/captures` returns immediately and the UI polls
for completion.

**Where to edit:**

| File | Change |
|---|---|
| `crates/archivr-core/src/database.rs` | Add `capture_jobs` table to the schema: `(id, job_uid, run_uid, archive_id, status TEXT CHECK(status IN ('pending','running','completed','failed')), error_text, created_at, updated_at)`. Add `create_capture_job`, `update_capture_job_status`, `get_capture_job` helpers. |
| `crates/archivr-core/src/archive.rs` | Add `CaptureJob` summary type and `get_capture_job` query. |
| `crates/archivr-server/src/routes.rs` | `POST /api/archives/:id/captures` → insert job row, spawn `tokio::task::spawn_blocking` for the capture call, return `{ job_uid, status: "pending" }` immediately. Add `GET /api/archives/:id/capture_jobs/:job_uid` route. |
| `frontend/src/api.js` | Add `pollCaptureJob(archiveId, jobUid)` |
| `frontend/src/components/CaptureDialog.jsx` | After submit: enter polling loop (500 ms interval), show "Running…" state, handle `completed` (close + refresh) and `failed` (show error). |

**Acceptance criteria:**
- `POST /captures` returns within 50 ms regardless of capture duration.
- UI shows "Running…" and updates when the job finishes.
- Failed captures surface the error message in the dialog.
- A server restart while a job is `running` leaves it stalled as `running`; on next startup,
  jobs stuck in `running` should be marked `failed` with a note ("interrupted by server restart").
- Existing `cargo test` passes.

**Key risk:** `spawn_blocking` runs on a Tokio thread pool thread. A very long capture
(15-minute YouTube video) will occupy one thread for the duration. For now that is acceptable.
A proper queue (channel + worker task) can replace it later without changing the DB schema or API.

---

### ~~4. Auth foundation — session + role + setup~~ ✅ Done

**Implemented:** `crates/archivr-server/src/auth.rs` (new; `AuthUser` extractor, Argon2id password
hashing, token generation, role bit constants). `crates/archivr-core/src/database.rs`:
`initialize_auth_schema` (roles, user_roles, sessions, api_tokens, instance_settings tables);
`open_auth_db` (separate server-level auth SQLite); `create_owner`, `compute_role_bits`,
`create_session`, `get_session`, `create_api_token`, `get_user_for_token` and related helpers.
`crates/archivr-server/src/routes.rs`: `AppState` gains `auth_db_path`; `setup_guard` middleware
returns 503 until owner created; `POST /api/auth/login`, `POST /api/auth/logout`,
`GET /api/auth/me`, `GET|POST /api/auth/setup`, `GET|POST|DELETE /api/auth/tokens` endpoints;
WRITE routes (captures, tags) guarded by `ROLE_USER`. `crates/archivr-server/src/main.rs`:
computes auth DB path from config directory; session cleanup background task.
Frontend: `LoginPage.jsx`, `SetupPage.jsx`, `AuthContext` in `App.jsx`, user menu in `Topbar.jsx`.
`docs/specs/2026-06-25-auth-foundation-design.md` has the full design. Roles use a bitmask:
guest=1, user=2, admin=4, owner=8. All tests green.

---

### ~~5. User management~~ ✅ Done

**Implemented:** `crates/archivr-core/src/database.rs`: `UserSummary` and `RoleRecord` structs;
10 new pub functions (`invalidate_user_sessions`, `get_user_id_by_uid`, `list_users`,
`get_user_by_uid`, `create_user`, `set_user_status`, `assign_role`, `remove_role`, `list_roles`,
`create_custom_role`). `crates/archivr-server/src/routes.rs`: 5 admin routes
(`GET|POST /api/admin/users`, `PATCH /api/admin/users/:uid/status`,
`POST|DELETE /api/admin/users/:uid/roles`, `GET|POST /api/admin/roles`); 7 handler functions;
request body structs. `frontend/src/api.js`: 7 admin helpers (`listAdminUsers`,
`createAdminUser`, `setUserStatus`, `assignRole`, `removeRole`, `listRoles`, `createRole`).
`frontend/src/components/AdminView.jsx`: two-tab admin panel (Users / Roles) with ban/unban,
create user form, create custom role form. Role bitmask: guest=1, user=2, admin=4, owner=8;
custom roles get bit_position≥4. Ban invalidates all sessions. Only-owner guard on role removal.
169 tests green.

---

### ~~6. Permissions & visibility — collection model~~ ✅ Done

**Implemented:** `database.rs`: `collections` + `collection_entries` tables; seed default collection ('All Entries');
migration inserts existing entries into default collection with `visibility_bits` derived from legacy `visibility` string
(`'public'`→3, `'unlisted'`→2, `'private'`→0). 8 new pub functions (`ensure_default_collection`, `create_collection`,
`list_collections`, `get_collection_by_uid`, `add_entry_to_collection`, `update_collection_entry_visibility`,
`remove_entry_from_collection`, `get_entry_collection_memberships`). `create_archived_entry` auto-enrolls new entries
into the default collection. `archive.rs`: `list_root_entries` + `search_entries` accept `caller_bits: u32` and enforce
collection visibility (`admin/owner bypass`; others filtered by `visibility_bits & caller_bits`). `list_entries_for_collection`,
`get_entry_collections`, `EntryCollectionMembership` added. `routes.rs`: read routes extract auth and pass caller_bits;
5 collection-entry route groups (`GET|POST /api/archives/:id/collections`, `GET /api/archives/:id/collections/:uid`,
`POST|DELETE|PATCH /api/archives/:id/collections/:uid/entries(/:entry_uid)`,
`GET /api/archives/:id/entries/:uid/collections`). Frontend: `CollectionsView.jsx` (list + detail + create form);
`collections` nav item in Topbar; entry collection memberships + visibility shown in ContextRail. 169 tests green.
Depends on Track 5.

---

### ~~7. Settings~~ ✅ Done

**Implemented:** `database.rs`: `display_name TEXT` column migration (ALTER TABLE, idempotent);
`InstanceSettings` struct; 6 new pub fns (`get_instance_settings`, `update_instance_settings`,
`update_user_display_name`, `update_user_password`, `get_user_password_hash`,
`get_user_display_name`). `routes.rs`: `PATCH /api/auth/me` (display name update + current-password-
verified password change); `GET|PATCH /api/admin/instance-settings` (ROLE_ADMIN);
`auth_me` now returns `display_name`. `frontend/src/api.js`: 7 new helpers (`updateProfile`,
`changePassword`, `listTokens`, `createToken`, `deleteToken`, `getInstanceSettings`,
`updateInstanceSettings`). `frontend/src/components/SettingsView.jsx`: tabbed settings page
(Profile — display name + password change; API Tokens — create/list/revoke with one-time reveal;
Instance — public index, public content, open registration toggles + default visibility select).
`Topbar.jsx`: settings nav button; user-menu shows display_name ?? username.
`App.jsx`: renders SettingsView for `view === 'settings'`. 176 tests green. Depends on Track 5.

---

### 8. Collections UI

**What:** Full collection management — create, rename, delete, add/remove entries,
set per-entry visibility within a collection, make collections public.
Depends on Tracks 5–6.

---

### 9. Cloud backup — S3-compatible

**What:** A command or scheduled operation that syncs the archive store to an S3-compatible
bucket (AWS S3, Cloudflare R2, Backblaze B2). Incremental: only uploads blobs not already
present in the bucket. The DB is also backed up.

**Design decisions to make before implementing:**
- CLI subcommand (`archivr backup`) vs. scheduled via the server vs. both.
- Per-archive config vs. a global backup profile.
- Whether to back up only the blob store or also the SQLite DB.

**Recommended shape:**

```toml
# In the archive's own config or in archivr-server.toml
[backup.s3]
bucket = "my-archivr-backup"
prefix = "personal/"
endpoint = "https://..."          # optional, for R2/B2
region = "us-east-1"
# credentials via AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY env vars
```

**Where to edit (when ready):**

| File | Change |
|---|---|
| `crates/archivr-core/src/backup.rs` | New module. Walk `store/raw/` tree, list objects in bucket, diff, upload new blobs. Also snapshot and upload `archivr.sqlite`. |
| `crates/archivr-cli/src/main.rs` | Add `archivr backup [--archive <path>]` subcommand |
| `Cargo.toml` | Add `aws-sdk-s3` or `object_store` crate dependency |
| `docs/README.md` | Document backup config and command |

**Acceptance criteria:**
- `archivr backup` uploads all blobs not already in the bucket.
- A second run uploads nothing (idempotent).
- DB snapshot is included.
- Missing or invalid credentials return a clear error before any uploads begin.

---

### 10. Cloud storage archiving (Google Drive, Dropbox, OneDrive)

Deferred. Each requires per-service OAuth, API clients, and download logic. Implement after
Tracks 1–4 are stable.

When approached, add a `Source::GoogleDrive`, `Source::Dropbox`, etc. variant in `capture.rs`
and a corresponding downloader module. Consider `rclone` as a shell-out strategy analogous to
`yt-dlp` and `monolith` — it handles auth and download for all three services.

---

## What to Do First

Tracks 1, 2, 3, 4, 5, and 6 are complete. Track 7 (settings) is the next priority.

Open the next thread with:

```text
Read ARCHIVR-MENTAL-MODEL.md and NEXT.md. I want to implement Track 7: settings —
account profile, password change, instance settings UI, API token management.
Create a task-level implementation plan first, then wait for approval.
```
