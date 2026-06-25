# Auth Foundation Design

**Track:** 4 of the roadmap (inserted after Track 3: Async capture jobs)
**Date:** 2026-06-25
**Status:** Approved for implementation

---

## Context & Roadmap Position

Archivr is evolving from a local-only tool (single hard-coded user, 127.0.0.1 binding) into a
self-hosted multi-user platform — think ArchiveBox but with real accounts, roles, and
public/private visibility. This track lays the foundation. All subsequent tracks depend on it.

**Full decomposition:**

| Track | Scope | Depends on |
|---|---|---|
| 4 (this) | Auth foundation | — |
| 5 | User management — registration, custom roles, admin panel | Track 4 |
| 6 | Permissions & visibility — collection model, per-membership visibility | Track 5 |
| 7 | Settings — account profile, instance-wide toggles | Track 5 |
| 8 | Collections UI | Tracks 5–6 |

---

## Goals

- Password-protected login with cookie sessions and API tokens
- Role table with bitmask-based visibility (extensible to custom roles in Track 5)
- Auth middleware that protects write/admin routes
- First-run owner setup wizard
- Frontend login page and session-aware API calls

## Non-Goals (explicitly deferred)

- Custom role creation UI → Track 5
- User registration flow → Track 5
- Visibility enforcement on queries → Track 6
- Collection model (replacing `archived_entries.visibility`) → Track 6
- Account settings page → Track 7
- API token management UI → Track 7

---

## Schema

### New table: `roles`

```sql
CREATE TABLE IF NOT EXISTS roles (
    id          INTEGER PRIMARY KEY,
    role_uid    TEXT NOT NULL UNIQUE,
    slug        TEXT NOT NULL UNIQUE,   -- 'guest', 'user', 'admin', 'owner', or custom
    name        TEXT NOT NULL,
    level       INTEGER NOT NULL,       -- ordering: guest=0, user=1, admin=3, owner=4
    bit_position INTEGER NOT NULL UNIQUE, -- position in visibility bitmask
    is_builtin  INTEGER NOT NULL DEFAULT 0 CHECK (is_builtin IN (0, 1))
);
```

**Built-in rows seeded at schema init:**

| slug | level | bit_position | bit value | is_builtin |
|---|---|---|---|---|
| guest | 0 | 0 | 1 | 1 |
| user | 1 | 1 | 2 | 1 |
| admin | 3 | 2 | 4 | 1 |
| owner | 4 | 3 | 8 | 1 |

Bit position 2 (value 4) is reserved for `admin`. Bit positions 4+ (values 16, 32, …) are assigned
to custom roles in Track 5. Level 2 is reserved for custom roles sitting between `user` and `admin`.

**Visibility bitmask semantics:**

A visibility value is an `INTEGER` where each bit indicates whether that role can see the content.

- `0`  = nobody (completely private)
- `2`  = logged-in users and above (`user` bit set)
- `6`  = users + admins (`user` + `admin` bits)
- `14` = everyone except guests (`user` + `admin` + `owner`)
- `15` = public (all bits set)

`is_builtin = 1` rows cannot be deleted.

### New table: `user_roles`

```sql
CREATE TABLE IF NOT EXISTS user_roles (
    user_id             INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role_id             INTEGER NOT NULL REFERENCES roles(id),
    assigned_at         TEXT NOT NULL,
    assigned_by_user_id INTEGER REFERENCES users(id),
    PRIMARY KEY (user_id, role_id)
);
```

A user can hold multiple roles. Their effective `role_bits` is the OR of all assigned roles'
bit values. `guest` is never assigned via this table — it is the implicit role for unauthenticated
requests.

### New table: `sessions`

```sql
CREATE TABLE IF NOT EXISTS sessions (
    id           INTEGER PRIMARY KEY,
    session_uid  TEXT NOT NULL UNIQUE,
    user_id      INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role_bits    INTEGER NOT NULL,   -- snapshot of bitmask at login time; role changes take effect on next login
    created_at   TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    expires_at   TEXT NOT NULL,      -- 30 days from last_seen_at
    user_agent   TEXT
);
CREATE INDEX IF NOT EXISTS idx_sessions_user_id ON sessions(user_id);
```

### New table: `api_tokens`

```sql
CREATE TABLE IF NOT EXISTS api_tokens (
    id           INTEGER PRIMARY KEY,
    token_uid    TEXT NOT NULL UNIQUE,
    user_id      INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash   TEXT NOT NULL UNIQUE,  -- SHA-256 of raw token; raw token never stored
    name         TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    last_used_at TEXT,
    expires_at   TEXT                   -- NULL = never expires
);
CREATE INDEX IF NOT EXISTS idx_api_tokens_user_id ON api_tokens(user_id);
```

### `users` table — existing, minimally changed

The existing `role TEXT CHECK('admin','user')` column is **kept but inert** — auth middleware
reads from `user_roles`, not this column. It will be removed in Track 5 cleanup when a proper
migration is warranted. No `ALTER TABLE` needed now.

`ensure_default_user` is replaced by `ensure_owner_exists` which returns `false` if no owner
exists yet (triggers setup mode). The old local-admin stub is never created on fresh instances.

### `instance_settings` — one new column

```sql
ALTER TABLE instance_settings ADD COLUMN
    default_entry_visibility INTEGER NOT NULL DEFAULT 2;
-- Default 2 = 'user' bit only: logged-in users can see entries by default
```

### `archived_entries.visibility` — deprecated, not removed

Flagged with a `-- DEPRECATED: replaced by collection_entries.visibility in Track 6` comment in
`initialize_schema`. No data migration needed yet; Track 6 handles it.

---

## Auth Flow

### Login

```
POST /api/auth/login
Body: { username: string, password: string }
```

1. Look up user by username.
2. Verify password with Argon2id (`argon2` crate).
3. Compute `role_bits` = OR of all `user_roles` bit values for this user.
4. Insert `sessions` row (`session_uid` = UUID, `expires_at` = now + 30 days).
5. Return `Set-Cookie: session=<session_uid>; HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000`.
6. Return `200 { user_uid, username, role_bits }`.

On failure: `401 { error: "invalid_credentials" }` — same message for unknown user and wrong
password (no user enumeration).

### Logout

```
POST /api/auth/logout
```

Deletes the `sessions` row for the current session cookie. Responds with
`Set-Cookie: session=; Max-Age=0` to clear the browser cookie. Returns `204`.

### Current user

```
GET /api/auth/me
```

Returns `200 { user_uid, username, role_bits }` for an authenticated request, or `401` for a
guest. The frontend calls this once on mount to restore session state.

### First-run setup

```
GET /api/auth/setup  → 200 { setup_required: bool }
POST /api/auth/setup → 201 { user_uid, username }
Body: { username: string, password: string }
```

`setup_required` is `true` when the `users` table has no row with the `owner` role. On `POST`,
the server creates the user, assigns the `owner` role, and seeds `instance_settings` if not
already present. After setup, the normal login flow applies.

All non-setup API routes return `503 { error: "setup_required" }` until setup is complete.
The following routes are **exempt** from the 503 check: `GET /api/auth/setup`,
`POST /api/auth/setup`, `GET /` (static), `GET /assets/*` (static).

### API tokens

```
POST   /api/auth/tokens          → 201 { token_uid, raw_token, name, created_at }
GET    /api/auth/tokens          → 200 [{ token_uid, name, created_at, last_used_at }]
DELETE /api/auth/tokens/:token_uid → 204
```

`raw_token` is a cryptographically random 32-byte value, base64url-encoded, returned once. The
server stores only its SHA-256 hash. The management UI for these endpoints is in Track 7; the
endpoints are implemented here.

### Password hashing

Argon2id with default parameters from the `argon2` crate (memory=19 MiB, iterations=2,
parallelism=1). The current `"disabled-local-password"` sentinel in `ensure_default_user` becomes
irrelevant once setup is required on fresh instances.

### Session expiry & cleanup

`last_seen_at` is updated on every authenticated request (max once per minute to avoid write
amplification). `expires_at` = `last_seen_at + 30 days`. A background task in `archivr-server/src/main.rs`
runs `DELETE FROM sessions WHERE expires_at < now()` at startup and every 24 hours via
`tokio::time::interval`.

---

## Auth Extractor

New file: `crates/archivr-server/src/auth.rs`

```rust
pub enum AuthUser {
    Guest,
    Authenticated { user_id: i64, role_bits: u32 },
}

impl AuthUser {
    pub fn require_auth(&self) -> Result<(i64, u32), ApiError>  // 401 if Guest
    pub fn require_role(&self, bit: u32) -> Result<(), ApiError> // 403 if bit not set
    pub fn has_role(&self, bit: u32) -> bool
}

// Role bit constants
pub const ROLE_GUEST: u32  = 1;
pub const ROLE_USER: u32   = 2;
pub const ROLE_ADMIN: u32  = 4;
pub const ROLE_OWNER: u32  = 8;
```

Implemented as an Axum `FromRequestParts` extractor. Tries `session` cookie first, then
`Authorization: Bearer` header. A missing or invalid credential resolves to `AuthUser::Guest`
(never a hard error at extraction time — handlers decide what to do with a guest).

---

## Route Protection Tiers

The existing security-boundary comment block in `routes.rs` is updated:

| Tier | Requirement | Examples |
|---|---|---|
| `STATIC` | none | `GET /`, `GET /assets/*` |
| `PUBLIC_READ` | none (visibility filtering deferred to Track 6) | `GET /api/archives/:id/entries` |
| `AUTH_READ` | `ROLE_USER` bit | authenticated entry access |
| `WRITE` | `ROLE_USER` bit | `POST /api/archives/:id/captures`, tag mutations |
| `ADMIN` | `ROLE_ADMIN` bit | `GET /api/admin/archives`, user management |
| `OWNER` | `ROLE_OWNER` bit | instance settings, ownership transfer |

**Error responses:**
- No/invalid session → `401` (frontend redirects to login)
- Valid session, insufficient role → `403`
- Private resource accessed without sufficient role → `404` (do not reveal existence)

Track 4 applies `ROLE_USER` enforcement to all existing `WRITE` routes and `ROLE_ADMIN` to
`/api/admin/*`. `PUBLIC_READ` routes return all data for now; Track 6 adds visibility filters.

---

## Frontend Changes

### New components

| Component | Purpose |
|---|---|
| `SetupPage.jsx` | First-run owner account creation wizard |
| `LoginPage.jsx` | Username/password login form |

### App.jsx changes

- On mount: call `GET /api/auth/setup`; if `setup_required`, render `<SetupPage>` and nothing else.
- Otherwise: call `GET /api/auth/me`; store result as `currentUser` state (null = guest).
- Pass `currentUser` down via React context (`AuthContext`).
- Any `401` response from any API call sets `currentUser` to null → triggers `<LoginPage>`.

### api.js changes

- Thin response interceptor: if status is `401`, dispatch a global `auth:expired` event that
  `App.jsx` listens to and handles by clearing `currentUser`.
- No token storage in JS — cookies are handled entirely by the browser.

### Topbar.jsx changes

- When `currentUser` is set: show `username` and a **Log out** button.
- Log out calls `POST /api/auth/logout`, then clears `currentUser`.

### What is NOT in Track 4 frontend

- Settings page (Track 7)
- User management UI (Track 5)
- Role or visibility controls (Track 6)
- API token management UI (Track 7)

---

## New Dependencies

| Crate | Purpose |
|---|---|
| `argon2` | Password hashing (Argon2id) |
| `rand` | Cryptographically random token generation |
| `tower-cookies` | Cookie extraction in Axum (or use `axum-extra`) |

Add to `archivr-server/Cargo.toml` and workspace `Cargo.toml` as needed.

---

## Files Changed

| File | Change |
|---|---|
| `crates/archivr-core/src/database.rs` | Add `roles`, `user_roles`, `sessions`, `api_tokens` tables; seed built-in roles; add `instance_settings.default_entry_visibility`; replace `ensure_default_user` with `ensure_owner_exists`; add session/token CRUD helpers |
| `crates/archivr-server/src/auth.rs` | New: `AuthUser` extractor, role bit constants, session/token lookup |
| `crates/archivr-server/src/routes.rs` | Add auth endpoints (`/api/auth/*`); apply `AuthUser` extractor to WRITE/ADMIN routes; update security-boundary comment |
| `crates/archivr-server/src/main.rs` | Session cleanup background task |
| `frontend/src/App.jsx` | Setup check, auth state, `AuthContext` |
| `frontend/src/api.js` | 401 interceptor |
| `frontend/src/components/LoginPage.jsx` | New |
| `frontend/src/components/SetupPage.jsx` | New |
| `frontend/src/components/Topbar.jsx` | User menu + logout |
| `Cargo.toml` | Add `argon2`, `rand`, `tower-cookies` (or `axum-extra`) |

---

## Test Coverage

- `database.rs`: role seeding, `ensure_owner_exists`, session CRUD, token hash round-trip
- `auth.rs`: extractor resolves cookie → session → user; extractor resolves Bearer → token → user;
  missing credential → Guest; expired session → Guest
- `routes.rs`: login happy path; login wrong password returns 401; logout clears session;
  setup endpoint returns 503 after setup complete; WRITE route returns 401 for Guest;
  WRITE route returns 403 for insufficient role; setup flow end-to-end

---

## Track Numbering Update for NEXT.md

Original tracks 4 and 5 shift to 8 and 9. Collections is a named future track (no number until
scoped):

| # | Track |
|---|---|
| 3 | Async capture jobs |
| **4** | **Auth foundation (this spec)** |
| **5** | **User management** |
| **6** | **Permissions & visibility (collection model)** |
| **7** | **Settings** |
| **8** | **Collections UI** |
| 9 | Cloud backup (was 4) |
| 10 | Cloud storage (was 5) |
