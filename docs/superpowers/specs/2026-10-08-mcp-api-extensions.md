# Spec: Archivr API extensions for the MCP server

- **Status:** Contract (implemented on `mcp-api-extensions`, archivr HEAD 10afef9). Where the code and this text differed, the text now describes the code (see the "as implemented" notes in 1.5, 1.6 and 2.10).
- **Date:** 2026-10-08
- **Audience:** the Archivr (Rust) streams R1-R4 and the MCP (TypeScript) streams M0-M5. The MCP server is built against this document with a mocked `fetch`; the Rust streams implement it. Anything not written here is not part of the contract.
- **Read first:** `AGENTS.md`, `ARCHIVR-MENTAL-MODEL.md`.

## 0. Conventions

- Base path `/api`. JSON in and out unless noted. Timestamps are RFC 3339 strings (`2026-10-08T12:34:56.789+00:00`). `uid` values are opaque strings.
- **Auth.** Session cookie (`session`) or `Authorization: Bearer <raw_token>`. The MCP uses Bearer.
- **Errors.** Body is always `{"error": "<message>"}`. Status codes: `400` validation, `401` unauthenticated, `403` role/ownership/scope, `404` missing, `409` state conflict (self-target, last owner, update already running), `500` unexpected.
- **Role bits** (`u32`, from `/api/auth/me.role_bits`): `GUEST=1`, `USER=2`, `ADMIN=4`, `OWNER=8`; custom roles take bits from `16` up. Owners also hold `admin` and `user`; admins also hold `user`.
- **Role column** below: `any` = authenticated (any role incl. guest), `USER`/`ADMIN`/`OWNER` = caller must hold that bit, `own` = authenticated, acts only on the caller's own resources.
- `nullable` means the key is always present and may be `null`. `optional` means the key may be absent.
- **Never returned by any endpoint, ever:** `session_uid` (it is the cookie value), `password_hash`, `token_hash`, raw secrets/env values flagged `secret`, cookie-rule cookie values in MCP output.

### 0.1 Guard order for endpoints that target another user

For every endpoint that takes a `:uid` of a target user (U1-U5 and the hardened status/roles endpoints), checks run in this order, and the first failure wins:

1. Caller role (`401` / `403`).
2. Target lookup (`404 user not found`).
3. `ensure_not_self(caller_id, target_id)` -> `409` (only where the table says "self-guard").
4. `ensure_can_manage(caller_bits, target_bits)` -> `403`: **OWNER bit required if the target holds the OWNER or ADMIN bit** (strict rule). Admins manage plain/custom-role users only.
5. `ensure_not_last_owner(conn, target_id)` -> `409` (only where the table says "last-owner guard"): fails if the target is an *active* owner and no other *active* owner exists.

(Self before can-manage, so an admin targeting themselves gets `409`, not `403`.)

## 1. New and extended endpoints

### 1.1 Users (admin) - R1 and R2

| # | Method + path | Role | Guards |
|---|---|---|---|
| U1 | `DELETE /api/admin/users/:uid` | ADMIN | self, can-manage, last-owner |
| U2 | `POST /api/admin/users/:uid/password` | ADMIN | self, can-manage |
| U3 | `DELETE /api/admin/users/:uid/sessions` | ADMIN | self, can-manage |
| U4 | `GET /api/admin/users/:uid/tokens` | ADMIN | can-manage |
| U5 | `DELETE /api/admin/users/:uid/tokens/:token_uid` | ADMIN | can-manage |

**U1.** No body. Transaction: `UPDATE user_roles SET assigned_by_user_id = NULL WHERE assigned_by_user_id = <target>` (the FK has no `ON DELETE`), then delete the user; sessions, tokens and `user_roles` rows cascade. Response `204`, no body. Errors: 401/403/404/409.

**U2.** Request `{"new_password": string (>= 8 chars), "revoke_tokens": boolean (optional, default false)}`. Always deletes all of the target's sessions; with `revoke_tokens: true` also deletes all their API tokens. Response `200`:
```json
{"user_uid": "usr_...", "sessions_revoked": 2, "tokens_revoked": 0}
```
Errors: `400` (`new_password` shorter than 8, or blank), 401/403/404/409 (self: use `PATCH /api/auth/me`).

**U3.** No body. Deletes all of the target's sessions. Response `200` `{"revoked": <integer>}`.

**U4.** Response `200`, array (newest first), never contains hashes or raw tokens:
```json
[{"token_uid": "tok_...", "name": "string", "created_at": "ts", "last_used_at": "ts|null", "expires_at": "ts|null", "scope": "full|read"}]
```

**U5.** Response `204`. `404` if the token does not exist **or does not belong to `:uid`**.

### 1.2 Roles (admin) - R1

| # | Method + path | Role |
|---|---|---|
| R1 | `PATCH /api/admin/roles/:slug` | ADMIN |
| R2 | `DELETE /api/admin/roles/:slug` | OWNER |

**R1.** Request `{"name": string}` (trimmed, non-empty). Custom roles only; the slug is immutable. Response `200`, the updated role:
```json
{"role_uid": "string", "slug": "string", "name": "string", "level": 0, "bit_position": 4, "is_builtin": false}
```
Errors: `400` blank name or a built-in role (`guest/user/admin/owner`), 401/403/404 (unknown slug).

**R2.** No body. Custom roles only (`400` for built-ins, `404` unknown). One transaction: delete the role's `user_roles` rows, delete the sessions of every holder (their cached `role_bits` are stale), clear the role's bit from `instance_settings.reorder_children_role_bits`, delete the role. Response `200` `{"slug": string, "users_affected": <integer>, "reorder_mask_cleared": <boolean>}`.

### 1.3 Own sessions - R2

`session_handle` = first 16 hex characters of `hash_token(session_uid)` (the SHA3-256 hex helper already used for API tokens). The handle is stable and not reversible to the cookie. **`session_uid` is never returned.**

| # | Method + path | Role |
|---|---|---|
| S1 | `GET /api/auth/sessions` | own |
| S2 | `DELETE /api/auth/sessions/:handle` | own |
| S3 | `DELETE /api/auth/sessions` | own |

**S1.** Response `200`, array (newest `last_seen_at` first), only the caller's sessions:
```json
[{"session_handle": "0123456789abcdef", "created_at": "ts", "last_seen_at": "ts", "expires_at": "ts", "user_agent": "string|null", "current": true}]
```
`current` is true for the session whose cookie made this request; it is `false` for every row on Bearer-authenticated requests.

**S2.** `204`. `404` if the handle matches none of the caller's sessions (another user's handle is also `404`). Deleting the current session is allowed (equivalent to logout; the cookie is not cleared by this call).

**S3.** Revokes all of the caller's sessions **except the current one** (all of them on a Bearer request). Response `200` `{"revoked": <integer>}`.

### 1.4 Own tokens and identity - R2 (extend existing handlers in place)

| # | Method + path | Role |
|---|---|---|
| T1 | `POST /api/auth/tokens` (extended) | own |
| T2 | `GET /api/auth/tokens` (extended) | own |
| M1 | `GET /api/auth/me` (extended, additive) | any |

**T1.** Request:
```json
{"name": "string (required, non-blank)", "expires_in_days": "integer 1..=3650 | null (optional; absent/null = never expires)", "scope": "\"full\" | \"read\" (optional, default \"full\")"}
```
Response `201`:
```json
{"token_uid": "tok_...", "raw_token": "string (shown once)", "name": "string", "expires_at": "ts|null", "scope": "full|read"}
```
Errors: `400` blank name, `expires_in_days` out of range, unknown `scope`. A `read` token cannot call this endpoint (see 3.8).

**T2.** Response `200`, array (newest first), same item shape as U4.

**T1/T2 unchanged:** `DELETE /api/auth/tokens/:token_uid` -> `204` / `404`.

**M1.** Existing fields stay: `role_bits: integer`, `username: string`, `display_name: string|null`, `humanize_slugs: boolean`, `can_reorder_children: boolean`. Added:
```json
{"user_uid": "usr_...", "roles": ["user", "admin"]}
```
`roles` = the caller's role slugs.

### 1.5 Capture jobs and runs - R3

| # | Method + path | Role |
|---|---|---|
| J1 | `GET /api/archives/:id/capture_jobs?status&limit&offset&created_by` | USER |
| J2 | `GET /api/archives/:id/capture_jobs/:job_uid` (extended) | USER |
| J3 | `GET /api/archives/:id/runs` (filtered) | any authenticated |

Job and run visibility: a caller sees a job if they created it (`capture_jobs.created_by == their user_uid`) or hold ADMIN. Rows with `created_by IS NULL` (pre-migration jobs, CLI-created) are visible to ADMIN only.

**J1.** Query: `status` (optional; `pending|running|completed|failed`, else `400`), `limit` (default 50, max 200, clamped), `offset` (default 0), `created_by` (optional user_uid; ADMIN may filter by anyone, a non-admin may only pass their own uid, otherwise `403`). Ordered `created_at DESC, id DESC`. Response `200`:
```json
[{"job_uid": "job_...", "archive_id": "string", "run_uid": "string|null", "status": "pending|running|completed|failed",
  "error_text": "string|null", "notes_json": "string|null", "created_at": "ts", "updated_at": "ts", "created_by": "usr_...|null"}]
```
Errors: 400, 401, 403, 404 (unknown archive).

**J2.** Existing response fields unchanged (`job_uid, archive_id, run_uid, status, error_text, notes_json, created_at, updated_at`). Added:
```json
{"entry_uids": ["ent_..."],
 "items": [{"item_uid": "string", "ordinal": 0, "requested_locator": "string", "canonical_locator": "string|null",
            "source_kind": "string", "entity_kind": "string", "status": "pending|in_progress|completed|failed",
            "error_text": "string|null", "entry_uid": "string|null"}]}
```
`items` come from `archive_run_items` of the job's run (empty while `run_uid` is null), ordered by `ordinal`; `entry_uid` is the produced entry (`produced_entry_id`), and `entry_uids` is the de-duplicated non-null list in item order. A caller who is neither the creator nor ADMIN gets `404` (not `403`), so job existence is not disclosed.

As implemented, J2 also returns `created_by` (the submitter's `user_uid`, or `null`) and `items_truncated: bool`. `items` is capped at 200 (`JOB_ITEMS_MAX`); `items_truncated` is `true` when the run had more items than were returned. `created_by` is also present on every row of J1.

**J3.** Response unchanged in shape (array of run summaries):
```json
[{"run_uid": "string", "started_at": "ts", "finished_at": "ts|null", "status": "in_progress|completed|failed",
  "requested_count": 0, "discovered_count": 0, "completed_count": 0, "failed_count": 0, "error_summary": "string|null"}]
```
but filtered: a run is visible if the caller is ADMIN, **or** created the job that owns the run (`capture_jobs.run_uid`), **or** can see at least one entry produced by the run's items (reuse the existing entry-visibility/collection filter). Runs with no job row (pre-migration) follow entry access only. Authenticated-only (guests still get `401`).

### 1.6 Archive info and effective config - R4

| # | Method + path | Role |
|---|---|---|
| I1 | `GET /api/archives/:id/info` | ADMIN |
| I2 | `GET /api/admin/effective-config` | ADMIN |

**I1.** Counts and sizes only. `name` is the archive's display name from its own metadata; no filesystem paths are returned. Response `200`:
```json
{"archive_id": "string", "label": "string", "name": "string|null",
 "entry_count": 0, "root_entry_count": 0, "child_entry_count": 0,
 "artifact_count": 0, "blob_count": 0, "blob_bytes": 0,
 "tag_count": 0, "collection_count": 0, "run_count": 0, "summary_count": 0,
 "job_counts": {"pending": 0, "running": 0, "completed": 0, "failed": 0},
 "db_bytes": 0}
```
`child_entry_count` = `entry_count - root_entry_count`. `db_bytes` is the size of the archive's SQLite file (0 if it cannot be read). Errors: 401/403/404 (unknown archive).

**I2.** Read-only view of the server's configuration, from a static `ENV_VARS` table in `effective_config.rs` (every `ARCHIVR_*` variable the code reads; a unit test greps the crates' source for `ARCHIVR_*` literals and fails on drift). Response `200`, top-level keys as below:
```json
{"server": {"version": "string", "bind": {"value": "string", "source": "env|toml|default"},
            "archives": [{"id": "string", "label": "string"}]},
 "env_vars": [{"name": "ARCHIVR_ANTHROPIC_API_KEY", "group": "summaries", "description": "string",
               "secret": true, "default": "string|null", "set": true, "value": "string|null"}],
 "summary_providers": [{"kind": "anthropic_http", "configured": true, "model": "string|null", "error": "string|null"}],
 "title_models": {"anthropic_http": {"model": "string", "source": "instance|env|default"}},
 "transcription_engines": [{"kind": "string", "enabled": true, "configured": true, "label": "string|null",
                            "english_only": "bool|null", "languages": "string[]|null", "error": "string|null"}],
 "extensions": {"ublock": {"available": true}, "cookie_consent": {"available": false}}}
```
- `secret: true` entries never carry a value: `value` is `null`, only `set` (true when the variable is set and non-empty) is reported.
- Non-secret `value` is the effective string, or `null` when unset. URL-valued variables lose userinfo and query string (`https://user:pw@host/p?k=v` -> `https://host/p`).
- `group` is one of `server`, `tools`, `summaries`, `titles`, `transcription`.
- `summary_providers[].error` is the (secret-free) reason `provider_from_env` fails, e.g. which variable is missing.
- `server` and `extensions` are not in the original R4 list; they were added during implementation. `server.bind.value` is the effective bind address and never carries secrets.

## 2. Hardening of existing endpoints

Behavior changes to endpoints that already exist (R1 owns status/roles, R2 owns auth, R3 owns capture/archives):

1. `PATCH /api/admin/users/:uid/status`: runs the guard sequence of 0.1 with self-guard and last-owner guard. Disabling oneself -> `409`; disabling the last active owner -> `409`; ADMIN targeting an OWNER/ADMIN -> `403`. `status: "active"` re-enable needs only can-manage.
2. `POST /api/admin/users/:uid/roles` (`{"role_slug": ...}`) and `DELETE /api/admin/users/:uid/roles/:role_slug`: granting or removing `owner` **or** `admin` requires OWNER (`403` otherwise); can-manage applies to the target; unknown role slug -> `404` (currently `500`); removing `owner` from the last active owner -> `409` (currently `500`).
3. `POST /api/archives/:id/captures`: a `file://` locator is accepted **only** if it points at a staged upload previously created through `POST /api/archives/:id/uploads` (under the archive's `temp/uploads/`); any other `file://` locator (e.g. `file:///etc/passwd`) -> `400`. A bare path locator (absolute, or relative to the server's cwd, e.g. `/etc/hosts`) is refused with `400` as well, because core classifies any existing path as a local file. The CLI is unaffected. (`rearchive`, text capture unchanged.)
4. Bearer auth now calls `touch_token`: `api_tokens.last_used_at` is set on use, throttled so it is written at most once per 60 s per token. Visible in T2/U4.
5. `PATCH /api/auth/me`: `new_password` must be >= 8 characters (`400`), and a successful password change deletes every *other* session of the user (the caller's current session survives).
6. `GET /api/archives` (unauthenticated today): `archive_path` is omitted from each item unless the caller is ADMIN. Items are `{"id", "label"}` for everyone else. (The admin view in the frontend reads `archive_path` as an admin, so it keeps working.)
7. **Read-scope tokens**: Bearer requests authenticated by a token with `scope = "read"` get `403 {"error": "read-only token"}` on any method other than `GET`, `HEAD`, `OPTIONS`. Cookie sessions are unaffected. Implemented as middleware in `token_scope.rs`.
8. Bearer tokens past `expires_at` are rejected: the request is treated as unauthenticated (`401` on endpoints that require auth). (Already enforced in `database::get_user_for_token`; covered by a regression test.)
9. `capture_jobs.created_by` is now recorded (the submitter's `user_uid`) by `capture_handler`, text capture, and rearchive via `database::create_capture_job_as`; other callers keep `create_capture_job` (NULL owner).
10. **Inert instance settings (as implemented).** `public_index_enabled`, `public_entry_content_enabled` and `open_registration_enabled` (column `public_archive_submission_enabled`) are stored and returned by `PATCH` / `GET` instance settings, but no server logic reads them yet. Setting them changes nothing in behavior. Do not document them as enforced until a stream wires them in.
11. **`file://` rule (as implemented).** The check of item 3 lives in the capture handler in `routes.rs`; the bare-path check uses `capture::locator_is_local_path`. Regression tests are in `jobs.rs` (`file:///etc/hosts`, bare `/etc/hosts` and `Cargo.toml` -> `400`, staged upload -> accepted).

Out of scope (not proposed): entry metadata edits beyond title, job cancel, server-side entry pagination, frontend/UI updates, admin edit of email/display name.

## 3. Foundation delivered by R0 (what the streams build on)

### 3.1 Schema (existing convention: `let _ = ALTER TABLE ... ADD COLUMN`, no version table)

- `capture_jobs.created_by TEXT` (auth `users.user_uid`; NULL for legacy/CLI rows) + `idx_capture_jobs_created_by`. In `initialize_schema`; the index is created *after* the `ALTER` so legacy tables migrate.
- `api_tokens.scope TEXT NOT NULL DEFAULT 'full'` (`full` | `read`). In `initialize_auth_schema`. `api_tokens.expires_at` already existed.
- Migration tests: `initialize_schema_migrates_created_by_for_existing_capture_jobs`, `initialize_auth_schema_migrates_scope_for_existing_api_tokens` (legacy table built by hand, init run twice).

### 3.2 `archivr_core::database` signatures

```rust
pub struct ApiTokenRecord { token_uid, name, created_at, last_used_at: Option<String>, expires_at: Option<String>, scope: String }
pub fn create_api_token(conn, user_id: i64, token_hash: &str, name: &str, expires_at: Option<&str>, scope: &str) -> Result<String>;
pub fn get_user_for_token(conn, token_hash: &str) -> Result<Option<(i64 /*user_id*/, String /*token_uid*/)>>;
pub fn touch_token(conn, token_uid: &str) -> Result<()>;            // existing; not yet called from auth
pub fn create_capture_job_as(conn, archive_id: &str, created_by: Option<&str>) -> Result<String>;
pub fn create_capture_job(conn, archive_id: &str) -> Result<String>; // delegates with None
pub fn get_user_uid(conn, user_id: i64) -> Result<Option<String>>;
```

`list_user_tokens` and therefore the existing `GET /api/auth/tokens` response gained `expires_at` and `scope` (additive; this is T2). `CaptureJobRecord` is unchanged (add `created_by` there in R3 if needed).

### 3.3 Modules

R0 created these as stubs; all are now implemented. Server: `admin_users.rs` (U1-U5, R1, R2), `credentials.rs` (S1-S3, T1/T2), `jobs.rs` (J1-J3), `effective_config.rs` (I1, I2), `guards.rs` (0.1), `token_scope.rs` (read-scope middleware, item 2.7), each merged in `app_with_state`. Core: `auth_users.rs`, `auth_credentials.rs`, `capture_jobs.rs`. `test_support` is `#[cfg(test)]`.

### 3.4 `guards.rs` (implemented)

```rust
pub fn ensure_can_manage(caller_bits: u32, target_bits: u32) -> Result<(), ApiError>; // 403
pub fn ensure_not_self(caller_id: i64, target_id: i64) -> Result<(), ApiError>;       // 409
pub fn ensure_not_last_owner(conn: &rusqlite::Connection, target_id: i64) -> Result<(), ApiError>; // 409, auth DB conn
```

### 3.5 `routes.rs` visibility

`AppState.registry` is `pub(crate)`; `ApiError::{not_found, bad_request, internal, conflict}` and the `status`/`message` fields are `pub(crate)` (`unauthorized`/`forbidden` were already `pub`); `mounted_archive(&AppState, &str)` and `auth_to_caller_bits(&AuthUser)` are `pub(crate)`. Role checks are the existing `AuthUser::{require_auth, require_role, has_role}`. `ROLE_*` consts are re-exported from `routes` and live in `auth`.

### 3.6 Test helpers (`crates/archivr-server/src/test_support.rs`)

```rust
pub(crate) fn make_test_registry(&TempDir) -> (ServerRegistry, PathBuf /*archive*/, PathBuf /*auth db*/); // owner "testowner" seeded
pub(crate) fn make_test_session(&Path) -> String;                       // owner cookie
pub(crate) fn make_role_session(&Path, username: &str, roles: &[&str]) -> String;
pub(crate) fn owner_session(&Path) -> String;  admin_session / user_session / guest_session (fixed usernames test-admin / test-user / test-guest; once per auth DB)
pub(crate) fn make_api_token(&Path, username: &str, expires: Option<&str>, scope: &str) -> String; // raw token
pub(crate) async fn body_json(Response) -> serde_json::Value;
pub(crate) fn json_body(&serde_json::Value) -> Body;
```
The originals in `routes.rs` `mod tests` are untouched (copies, not moves).

### 3.7 Axum `Router::merge` finding (axum 0.7.9, verified by tests in `test_support.rs`)

- Merging routers that register **different methods on the same path** works: the method routers are combined (`GET`+`POST` on `/api/auth/tokens` from the base router plus `DELETE` on the same path from a merged router gives all three, and other methods return `405`). The same holds for two merged routers that each add one method to a brand-new path.
- Registering the **same method on the same path twice** (e.g. a second `GET /api/auth/tokens`) **panics at router construction** ("Overlapping method route"), i.e. at server startup and in every test that builds the app.
- Consequences for the streams: (a) new paths and new methods on existing paths may go in the stream's own `routes()`; (b) **extending an existing handler (T1, T2, M1, J2, `PATCH /api/auth/me`, the status/roles/capture hardening, `GET /api/archives`) must be done by editing the handler in `routes.rs` in place**, never by re-registering the same method+path from a stream module; (c) `.merge(...)` calls sit before `.fallback_service(...)` and the `.layer(...)`s in `app_with_state`, so streams' routes get the existing middleware (`setup_guard`, `login_rate_limit`, `security_headers`).
- Current paths by owner: `DELETE /api/admin/users/:uid`, `.../password`, `.../sessions`, `.../tokens[/:token_uid]`, `PATCH|DELETE /api/admin/roles/:slug`, `/api/auth/sessions[/:handle]`, `/api/archives/:id/capture_jobs` (list), `/api/archives/:id/info`, `/api/admin/effective-config` are all **new paths** (no existing registration at the same path), so streams can add them via `routes()` without conflict. `/api/admin/users/:uid` (bare) does not exist today.

### 3.8 Read-scope reminder

A `read` token cannot create tokens, change passwords, or capture (all non-GET). It can poll jobs and read everything its user can read.

## 4. Test requirements (all streams)

Follow the `oneshot` style in `routes.rs` `mod tests`, helpers from `test_support.rs`. For every new endpoint: guest 401, USER 403, ADMIN 200/201/204. For user-targeting endpoints additionally: ADMIN-vs-OWNER 403, ADMIN-vs-ADMIN 403, self 409, last-owner 409 where applicable. Regressions: user delete after assigning roles (FK `assigned_by_user_id`), `session_uid` never in any response body, secret canary env value absent from effective-config, back-dated `expires_at` -> 401, read-scope token 403 on POST, job and run scoping between two users, `file:///etc/hosts` capture -> 400, disabled/deleted user's tokens and sessions stop working.

## 5. Entry access rules (added after review of the first implementation)

The first implementation hid entries only in lists and search; fetching by uid still worked. These rules close that gap.

1. **Hidden means 404.** For a logged-in caller who is neither ADMIN nor OWNER, every endpoint that takes an entry uid answers `404 entry not found` when none of the entry's collection memberships (or its parent entry's) has `visibility_bits & caller_bits != 0`. This covers entry detail, artifacts, summary (GET and POST), tags (GET, POST, DELETE), entry collections, media token, favicon, PATCH and DELETE entry, thread title, rearchive, and collection add, remove and visibility. `GET /api/archives/:id/blobs/:sha256` allows a blob when at least one entry that uses it is visible. Guests keep `is_entry_publicly_accessible`. Rearchive of an unknown entry is `404` for everyone.
2. **Tag counts.** `GET .../tags` counts only entries the caller can see. Tag names remain visible to every signed-in user.
3. **No self-lockout.** A non-admin whose `PATCH .../collections/:c/entries/:uid` or `DELETE` would leave the entry invisible to the caller's own roles gets `400` and nothing changes. Admins and owners are exempt.
4. **Run link at job start.** `CaptureConfig.job_uid` carries the job into the capture; `capture_jobs.run_uid` is written as soon as the run exists, so `GET .../runs` shows the creator an in-progress run (rule J3). Rearchive passes no job uid.
5. **Default collection.** `DELETE .../collections/<default>` answers `400`.
6. **Known limit.** A media token issued before an entry was hidden stays valid until it expires (two hours).

