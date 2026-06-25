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

### 4. Cloud backup — S3-compatible

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

### 5. Cloud storage archiving (Google Drive, Dropbox, OneDrive)

Deferred. Each requires per-service OAuth, API clients, and download logic. Implement after
Tracks 1–4 are stable.

When approached, add a `Source::GoogleDrive`, `Source::Dropbox`, etc. variant in `capture.rs`
and a corresponding downloader module. Consider `rclone` as a shell-out strategy analogous to
`yt-dlp` and `monolith` — it handles auth and download for all three services.

---

## What to Do First

Open the next thread with:

```text
Read ARCHIVR-MENTAL-MODEL.md and NEXT.md. I want to implement Track 1: generic URL capture
(plain file download over HTTP/S). Create a task-level implementation plan first, then wait
for approval.
```

For Track 2:

```text
Read ARCHIVR-MENTAL-MODEL.md and NEXT.md. I want to implement Track 2: web page archiving
via monolith. Start by deciding the URL classification strategy (head-first vs. extension
heuristic vs. user prefix), then write the implementation plan.
```
