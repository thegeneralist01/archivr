# Auth Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add cookie-session + API-token authentication, a role bitmask system, a first-run setup wizard, and auth-protected routes to the Archivr server.

**Architecture:** Auth state lives in a dedicated `archivr-auth.sqlite` file next to the server config, separate from the per-archive store DBs. `AppState` gains an `auth_db_path`. Route handlers call `database::open_auth_db()` to get a connection; a new `AuthUser` Axum extractor validates session cookies and Bearer tokens for every protected handler.

**Tech Stack:** Rust/Axum, rusqlite (existing), argon2 (Argon2id hashing), rand (token generation), axum-extra (cookie extraction), React/Vite (existing frontend)

**Spec:** `docs/superpowers/specs/2026-06-25-auth-foundation-design.md`

---

## File Map

| File | Action | What changes |
|---|---|---|
| `Cargo.toml` | Modify | Add `argon2`, `rand`, `axum-extra` to workspace deps |
| `crates/archivr-server/Cargo.toml` | Modify | Pull `argon2`, `rand`, `axum-extra` from workspace |
| `crates/archivr-core/src/database.rs` | Modify | `initialize_auth_schema`, auth CRUD helpers, new record types |
| `crates/archivr-server/src/auth.rs` | **Create** | `AuthUser` extractor, password helpers, token generation, role constants |
| `crates/archivr-server/src/routes.rs` | Modify | `AppState` + `app()` gain `auth_db_path`; auth endpoints; route protection; `ApiError` gets JSON body + `unauthorized`/`forbidden` constructors |
| `crates/archivr-server/src/registry.rs` | Modify | `ServerRegistry` gains optional `auth_db_path` |
| `crates/archivr-server/src/main.rs` | Modify | Compute auth DB path; pass to `app()`; session cleanup task; remove non-loopback auth warning |
| `frontend/src/App.jsx` | Modify | Setup check on mount; `AuthContext`; 401 handling |
| `frontend/src/api.js` | Modify | 401 interceptor; auth helper calls |
| `frontend/src/components/LoginPage.jsx` | **Create** | Login form |
| `frontend/src/components/SetupPage.jsx` | **Create** | First-run owner creation wizard |
| `frontend/src/components/Topbar.jsx` | Modify | User menu + logout button |

---

## Task 1: Add dependencies

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/archivr-server/Cargo.toml`

- [ ] **Step 1: Add workspace dependencies**

Open `Cargo.toml`. In `[workspace.dependencies]`, add after the `base64` line:

```toml
argon2 = { version = "0.5", features = ["std"] }
rand = { version = "0.8", features = ["std"] }
axum-extra = { version = "0.9", features = ["cookie"] }
```

- [ ] **Step 2: Pull into server crate**

Open `crates/archivr-server/Cargo.toml`. Add to `[dependencies]`:

```toml
argon2.workspace = true
rand.workspace = true
axum-extra.workspace = true
```

- [ ] **Step 3: Verify compilation**

```bash
cd ~/personal/archivr && cargo check -p archivr-server
```

Expected: compiles with no errors.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml crates/archivr-server/Cargo.toml Cargo.lock
git commit -m "feat(auth): add argon2, rand, axum-extra dependencies"
```

---

## Task 2: Auth schema in `database.rs`

Add `initialize_auth_schema` and update `instance_settings`.

**Files:**
- Modify: `crates/archivr-core/src/database.rs`

- [ ] **Step 1: Write failing test for role seeding**

At the bottom of the `#[cfg(test)]` block in `database.rs`, add:

```rust
#[test]
fn auth_schema_seeds_builtin_roles() {
    let conn = Connection::open_in_memory().unwrap();
    initialize_auth_schema(&conn).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM roles WHERE is_builtin = 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 4);
    let owner_bits: i64 = conn
        .query_row("SELECT bit_position FROM roles WHERE slug = 'owner'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(owner_bits, 3);
}

#[test]
fn auth_schema_is_idempotent() {
    let conn = Connection::open_in_memory().unwrap();
    initialize_auth_schema(&conn).unwrap();
    initialize_auth_schema(&conn).unwrap(); // must not panic
}
```

- [ ] **Step 2: Run to confirm they fail**

```bash
cd ~/personal/archivr && cargo test -p archivr-core auth_schema 2>&1 | tail -5
```

Expected: FAILED — `initialize_auth_schema` not found.

- [ ] **Step 3: Add `initialize_auth_schema`**

After the closing `}` of `initialize_schema` in `database.rs`, add:

```rust
pub fn initialize_auth_schema(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS roles (
            id           INTEGER PRIMARY KEY,
            role_uid     TEXT NOT NULL UNIQUE,
            slug         TEXT NOT NULL UNIQUE,
            name         TEXT NOT NULL,
            level        INTEGER NOT NULL,
            bit_position INTEGER NOT NULL UNIQUE,
            is_builtin   INTEGER NOT NULL DEFAULT 0 CHECK (is_builtin IN (0, 1))
        );

        INSERT OR IGNORE INTO roles (role_uid, slug, name, level, bit_position, is_builtin) VALUES
            ('role-guest', 'guest', 'Guest',  0, 0, 1),
            ('role-user',  'user',  'User',   1, 1, 1),
            ('role-admin', 'admin', 'Admin',  3, 2, 1),
            ('role-owner', 'owner', 'Owner',  4, 3, 1);

        CREATE TABLE IF NOT EXISTS user_roles (
            user_id             INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            role_id             INTEGER NOT NULL REFERENCES roles(id),
            assigned_at         TEXT NOT NULL,
            assigned_by_user_id INTEGER REFERENCES users(id),
            PRIMARY KEY (user_id, role_id)
        );

        CREATE TABLE IF NOT EXISTS sessions (
            id           INTEGER PRIMARY KEY,
            session_uid  TEXT NOT NULL UNIQUE,
            user_id      INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            role_bits    INTEGER NOT NULL,
            created_at   TEXT NOT NULL,
            last_seen_at TEXT NOT NULL,
            expires_at   TEXT NOT NULL,
            user_agent   TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_sessions_user_id ON sessions(user_id);

        CREATE TABLE IF NOT EXISTS api_tokens (
            id           INTEGER PRIMARY KEY,
            token_uid    TEXT NOT NULL UNIQUE,
            user_id      INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            token_hash   TEXT NOT NULL UNIQUE,
            name         TEXT NOT NULL,
            created_at   TEXT NOT NULL,
            last_used_at TEXT,
            expires_at   TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_api_tokens_user_id ON api_tokens(user_id);

        CREATE TABLE IF NOT EXISTS instance_settings (
            id                                 INTEGER PRIMARY KEY CHECK (id = 1),
            public_index_enabled               INTEGER NOT NULL DEFAULT 0 CHECK (public_index_enabled IN (0, 1)),
            public_entry_content_enabled       INTEGER NOT NULL DEFAULT 0 CHECK (public_entry_content_enabled IN (0, 1)),
            public_archive_submission_enabled  INTEGER NOT NULL DEFAULT 0 CHECK (public_archive_submission_enabled IN (0, 1)),
            default_entry_visibility           INTEGER NOT NULL DEFAULT 2
        );

        INSERT OR IGNORE INTO instance_settings
            (id, public_index_enabled, public_entry_content_enabled,
             public_archive_submission_enabled, default_entry_visibility)
        VALUES (1, 0, 0, 0, 2);

        CREATE TABLE IF NOT EXISTS users (
            id            INTEGER PRIMARY KEY,
            user_uid      TEXT NOT NULL UNIQUE,
            username      TEXT NOT NULL UNIQUE,
            email         TEXT UNIQUE,
            password_hash TEXT NOT NULL,
            status        TEXT NOT NULL CHECK (status IN ('active', 'disabled')),
            role          TEXT NOT NULL CHECK (role IN ('admin', 'user')),
            created_at    TEXT NOT NULL,
            last_login_at TEXT
        );
        "#,
    )?;
    Ok(())
}
```

Note: `users` is duplicated here with `CREATE TABLE IF NOT EXISTS` so the auth DB is self-contained without needing `initialize_schema`.

- [ ] **Step 4: Run tests**

```bash
cd ~/personal/archivr && cargo test -p archivr-core auth_schema 2>&1 | tail -5
```

Expected: 2 tests pass.

- [ ] **Step 5: Add `open_auth_db` function**

After `open_or_initialize`, add:

```rust
pub fn open_auth_db(auth_db_path: &Path) -> Result<Connection> {
    if let Some(parent) = auth_db_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("failed to create auth DB directory {}", parent.display())
        })?;
    }
    let conn = Connection::open(auth_db_path).with_context(|| {
        format!("failed to open auth database at {}", auth_db_path.display())
    })?;
    initialize_auth_schema(&conn)?;
    Ok(conn)
}
```

- [ ] **Step 6: Verify compilation and commit**

```bash
cd ~/personal/archivr && cargo test -p archivr-core 2>&1 | tail -3
```

Expected: all existing tests pass.

```bash
git add crates/archivr-core/src/database.rs
git commit -m "feat(auth): add initialize_auth_schema and open_auth_db"
```

---

## Task 3: User and role DB helpers

**Files:**
- Modify: `crates/archivr-core/src/database.rs`

- [ ] **Step 1: Add record types**

After the existing struct definitions (before `pub fn database_path`), add:

```rust
#[derive(Debug, Clone)]
pub struct AuthUserRecord {
    pub id: i64,
    pub user_uid: String,
    pub username: String,
    pub password_hash: String,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct SessionRecord {
    pub user_id: i64,
    pub role_bits: u32,
    pub last_seen_at: String,
    pub session_uid: String,
}

#[derive(Debug, Clone)]
pub struct ApiTokenRecord {
    pub token_uid: String,
    pub name: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
}
```

- [ ] **Step 2: Write failing tests for user helpers**

In the `#[cfg(test)]` block, add:

```rust
fn make_auth_conn() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    initialize_auth_schema(&conn).unwrap();
    conn
}

#[test]
fn ensure_owner_exists_returns_false_when_no_owner() {
    let conn = make_auth_conn();
    assert!(!ensure_owner_exists(&conn).unwrap());
}

#[test]
fn create_owner_then_ensure_returns_true() {
    let conn = make_auth_conn();
    create_owner(&conn, "alice", "hashed_pw").unwrap();
    assert!(ensure_owner_exists(&conn).unwrap());
}

#[test]
fn create_owner_assigns_cumulative_roles() {
    let conn = make_auth_conn();
    let user_id = create_owner(&conn, "alice", "hashed_pw").unwrap();
    let bits = compute_role_bits(&conn, user_id).unwrap();
    // guest=1, user=2, admin=4, owner=8 → 15
    assert_eq!(bits, 15u32);
}

#[test]
fn get_user_by_username_returns_none_for_unknown() {
    let conn = make_auth_conn();
    assert!(get_user_by_username(&conn, "nobody").unwrap().is_none());
}
```

- [ ] **Step 3: Run to confirm they fail**

```bash
cd ~/personal/archivr && cargo test -p archivr-core ensure_owner 2>&1 | tail -5
```

Expected: FAILED.

- [ ] **Step 4: Implement user/role helpers**

After `open_auth_db`, add:

```rust
/// Returns true if an owner account exists.
pub fn ensure_owner_exists(conn: &Connection) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM user_roles ur
         JOIN roles r ON r.id = ur.role_id
         WHERE r.slug = 'owner'",
        [],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Creates a user and assigns all roles from `user` up to `owner` (cumulative).
/// `password_hash` must already be hashed by the caller.
pub fn create_owner(conn: &Connection, username: &str, password_hash: &str) -> Result<i64> {
    let user_uid = public_id("usr");
    conn.execute(
        "INSERT INTO users (user_uid, username, email, password_hash, status, role, created_at)
         VALUES (?1, ?2, NULL, ?3, 'active', 'admin', ?4)",
        params![user_uid, username, password_hash, now_timestamp()],
    )?;
    let user_id = conn.last_insert_rowid();
    // Assign user, admin, owner (cumulative)
    for slug in &["user", "admin", "owner"] {
        let role_id: i64 = conn.query_row(
            "SELECT id FROM roles WHERE slug = ?1",
            [slug],
            |row| row.get(0),
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO user_roles (user_id, role_id, assigned_at)
             VALUES (?1, ?2, ?3)",
            params![user_id, role_id, now_timestamp()],
        )?;
    }
    Ok(user_id)
}

pub fn get_user_by_username(conn: &Connection, username: &str) -> Result<Option<AuthUserRecord>> {
    conn.query_row(
        "SELECT id, user_uid, username, password_hash, status FROM users WHERE username = ?1",
        [username],
        |row| {
            Ok(AuthUserRecord {
                id: row.get(0)?,
                user_uid: row.get(1)?,
                username: row.get(2)?,
                password_hash: row.get(3)?,
                status: row.get(4)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

/// Computes role_bits = ROLE_GUEST | OR(assigned role bit values).
/// ROLE_GUEST (bit 0, value 1) is always included as the implicit floor.
pub fn compute_role_bits(conn: &Connection, user_id: i64) -> Result<u32> {
    let mut stmt = conn.prepare(
        "SELECT (1 << r.bit_position) FROM user_roles ur
         JOIN roles r ON r.id = ur.role_id
         WHERE ur.user_id = ?1",
    )?;
    let bits: u32 = stmt
        .query_map([user_id], |row| row.get::<_, i64>(0))?
        .try_fold(1u32, |acc, val| val.map(|v| acc | v as u32))?;
    Ok(bits)
}
```

- [ ] **Step 5: Run tests**

```bash
cd ~/personal/archivr && cargo test -p archivr-core ensure_owner create_owner get_user compute_role 2>&1 | tail -5
```

Expected: all 4 new tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/archivr-core/src/database.rs
git commit -m "feat(auth): user and role DB helpers (create_owner, compute_role_bits)"
```

---

## Task 4: Session and token DB helpers

**Files:**
- Modify: `crates/archivr-core/src/database.rs`

- [ ] **Step 1: Write failing session tests**

```rust
#[test]
fn create_and_get_session() {
    let conn = make_auth_conn();
    let user_id = create_owner(&conn, "alice", "pw").unwrap();
    let uid = create_session(&conn, user_id, 15, None).unwrap();
    let sess = get_session(&conn, &uid).unwrap().unwrap();
    assert_eq!(sess.user_id, user_id);
    assert_eq!(sess.role_bits, 15);
}

#[test]
fn get_session_returns_none_for_unknown() {
    let conn = make_auth_conn();
    assert!(get_session(&conn, "nonexistent").unwrap().is_none());
}

#[test]
fn delete_session_removes_it() {
    let conn = make_auth_conn();
    let user_id = create_owner(&conn, "alice", "pw").unwrap();
    let uid = create_session(&conn, user_id, 15, None).unwrap();
    delete_session(&conn, &uid).unwrap();
    assert!(get_session(&conn, &uid).unwrap().is_none());
}

#[test]
fn token_hash_round_trips() {
    let conn = make_auth_conn();
    let user_id = create_owner(&conn, "alice", "pw").unwrap();
    create_api_token(&conn, user_id, "hash_abc", "My Token").unwrap();
    let found_id = get_user_for_token(&conn, "hash_abc").unwrap();
    assert_eq!(found_id, Some(user_id));
}

#[test]
fn get_user_for_token_returns_none_for_unknown() {
    let conn = make_auth_conn();
    assert!(get_user_for_token(&conn, "unknown").unwrap().is_none());
}
```

- [ ] **Step 2: Run to confirm failure**

```bash
cd ~/personal/archivr && cargo test -p archivr-core create_and_get_session token_hash 2>&1 | tail -5
```

- [ ] **Step 3: Implement session helpers**

After `compute_role_bits`, add:

```rust
/// Returns a new session_uid (UUID).
pub fn create_session(
    conn: &Connection,
    user_id: i64,
    role_bits: u32,
    user_agent: Option<&str>,
) -> Result<String> {
    let session_uid = public_id("sess");
    let now = now_timestamp();
    // expires_at = 30 days from now (approximate via string arithmetic is fragile;
    // compute with chrono instead)
    let expires_at = chrono::Utc::now()
        .checked_add_signed(chrono::Duration::days(30))
        .unwrap()
        .format("%Y-%m-%dT%H-%M-%S%.3f")
        .to_string();
    conn.execute(
        "INSERT INTO sessions (session_uid, user_id, role_bits, created_at, last_seen_at, expires_at, user_agent)
         VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6)",
        params![session_uid, user_id, role_bits as i64, now, expires_at, user_agent],
    )?;
    Ok(session_uid)
}

/// Returns session if it exists, the user is active, and it has not expired.
pub fn get_session(conn: &Connection, session_uid: &str) -> Result<Option<SessionRecord>> {
    let now = now_timestamp();
    conn.query_row(
        "SELECT s.user_id, s.role_bits, s.last_seen_at, s.session_uid
         FROM sessions s
         JOIN users u ON u.id = s.user_id
         WHERE s.session_uid = ?1
           AND u.status = 'active'
           AND s.expires_at > ?2",
        params![session_uid, now],
        |row| {
            Ok(SessionRecord {
                user_id: row.get(0)?,
                role_bits: row.get::<_, i64>(1)? as u32,
                last_seen_at: row.get(2)?,
                session_uid: row.get(3)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

pub fn delete_session(conn: &Connection, session_uid: &str) -> Result<()> {
    conn.execute("DELETE FROM sessions WHERE session_uid = ?1", [session_uid])?;
    Ok(())
}

/// Updates last_seen_at and extends expires_at by 30 days.
pub fn touch_session(conn: &Connection, session_uid: &str) -> Result<()> {
    let now = now_timestamp();
    let new_expires = chrono::Utc::now()
        .checked_add_signed(chrono::Duration::days(30))
        .unwrap()
        .format("%Y-%m-%dT%H-%M-%S%.3f")
        .to_string();
    conn.execute(
        "UPDATE sessions SET last_seen_at = ?1, expires_at = ?2 WHERE session_uid = ?3",
        params![now, new_expires, session_uid],
    )?;
    Ok(())
}

pub fn delete_expired_sessions(conn: &Connection) -> Result<usize> {
    let now = now_timestamp();
    let n = conn.execute("DELETE FROM sessions WHERE expires_at <= ?1", [now])?;
    Ok(n)
}
```

- [ ] **Step 4: Implement token helpers**

```rust
/// Creates an API token. `token_hash` is SHA3-256 hex of the raw token.
/// Returns the token_uid.
pub fn create_api_token(
    conn: &Connection,
    user_id: i64,
    token_hash: &str,
    name: &str,
) -> Result<String> {
    let token_uid = public_id("tok");
    conn.execute(
        "INSERT INTO api_tokens (token_uid, user_id, token_hash, name, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![token_uid, user_id, token_hash, name, now_timestamp()],
    )?;
    Ok(token_uid)
}

/// Returns the user_id for a given token hash, if the token is valid and user is active.
pub fn get_user_for_token(conn: &Connection, token_hash: &str) -> Result<Option<i64>> {
    let now = now_timestamp();
    conn.query_row(
        "SELECT t.user_id FROM api_tokens t
         JOIN users u ON u.id = t.user_id
         WHERE t.token_hash = ?1
           AND u.status = 'active'
           AND (t.expires_at IS NULL OR t.expires_at > ?2)",
        params![token_hash, now],
        |row| row.get(0),
    )
    .optional()
    .map_err(Into::into)
}

pub fn touch_token(conn: &Connection, token_uid: &str) -> Result<()> {
    conn.execute(
        "UPDATE api_tokens SET last_used_at = ?1 WHERE token_uid = ?2",
        params![now_timestamp(), token_uid],
    )?;
    Ok(())
}

/// Returns true if the token was found and deleted (user_id must match).
pub fn delete_api_token(conn: &Connection, token_uid: &str, user_id: i64) -> Result<bool> {
    let n = conn.execute(
        "DELETE FROM api_tokens WHERE token_uid = ?1 AND user_id = ?2",
        params![token_uid, user_id],
    )?;
    Ok(n > 0)
}

pub fn list_user_tokens(conn: &Connection, user_id: i64) -> Result<Vec<ApiTokenRecord>> {
    let mut stmt = conn.prepare(
        "SELECT token_uid, name, created_at, last_used_at
         FROM api_tokens WHERE user_id = ?1 ORDER BY created_at DESC",
    )?;
    let records = stmt
        .query_map([user_id], |row| {
            Ok(ApiTokenRecord {
                token_uid: row.get(0)?,
                name: row.get(1)?,
                created_at: row.get(2)?,
                last_used_at: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(records)
}
```

- [ ] **Step 5: Run all new tests**

```bash
cd ~/personal/archivr && cargo test -p archivr-core 2>&1 | tail -5
```

Expected: all tests pass (including all previously passing ones).

- [ ] **Step 6: Commit**

```bash
git add crates/archivr-core/src/database.rs
git commit -m "feat(auth): session and token DB helpers"
```

---

## Task 5: Auth DB path in AppState, registry, and main.rs

**Files:**
- Modify: `crates/archivr-server/src/registry.rs`
- Modify: `crates/archivr-server/src/routes.rs`
- Modify: `crates/archivr-server/src/main.rs`

- [ ] **Step 1: Add `auth_db_path` to `ServerRegistry`**

In `registry.rs`, update the `ServerRegistry` struct:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ServerRegistry {
    #[serde(default)]
    pub archives: Vec<MountedArchive>,
    /// Optional bind address. Defaults to `127.0.0.1:8080`.
    #[serde(default)]
    pub bind: Option<String>,
    /// Path to the server-level auth database.
    /// Defaults to `archivr-auth.sqlite` in the same directory as the config file.
    #[serde(default)]
    pub auth_db_path: Option<std::path::PathBuf>,
}
```

- [ ] **Step 2: Update `AppState` in `routes.rs`**

Change `AppState`:

```rust
#[derive(Clone)]
pub struct AppState {
    registry: Arc<ServerRegistry>,
    pub auth_db_path: Arc<std::path::PathBuf>,
}
```

Update `app()` signature and body:

```rust
pub fn app(registry: ServerRegistry, auth_db_path: std::path::PathBuf) -> Router {
    let state = AppState {
        registry: Arc::new(registry),
        auth_db_path: Arc::new(auth_db_path),
    };
    // ... rest unchanged
```

- [ ] **Step 3: Update all tests in `routes.rs` that call `app(registry)`**

Every `app(registry)` in the test module must become `app(registry, std::path::PathBuf::from("/tmp/test-auth.sqlite"))`.

Search for all occurrences:

```bash
grep -n "app(registry" ~/personal/archivr/crates/archivr-server/src/routes.rs | head -20
```

Update each one to `app(registry, tempfile::tempdir().unwrap().path().join("auth.sqlite"))`. Add `use tempfile;` if not present. (The `tempfile` crate is already a workspace dependency.)

- [ ] **Step 4: Update `main.rs`**

```rust
mod registry;
mod routes;

use anyhow::{Context, Result};
use std::{net::SocketAddr, path::PathBuf};

const DEFAULT_BIND: &str = "127.0.0.1:8080";

#[tokio::main]
async fn main() -> Result<()> {
    let config_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("archivr-server.toml"));

    let registry = registry::load_registry(&config_path)?;

    // Auth DB lives next to the config file unless overridden.
    let auth_db_path = registry.auth_db_path.clone().unwrap_or_else(|| {
        config_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("archivr-auth.sqlite")
    });

    let app = routes::app(registry.clone(), auth_db_path.clone());

    let bind_str = std::env::var("ARCHIVR_BIND")
        .ok()
        .or_else(|| registry.bind.clone())
        .unwrap_or_else(|| DEFAULT_BIND.to_string());

    let addr: SocketAddr = bind_str
        .parse()
        .with_context(|| format!("invalid bind address: {bind_str}"))?;

    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("archivr-server listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
```

- [ ] **Step 5: Run full test suite**

```bash
cd ~/personal/archivr && cargo test 2>&1 | tail -5
```

Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/archivr-server/src/routes.rs \
        crates/archivr-server/src/registry.rs \
        crates/archivr-server/src/main.rs
git commit -m "feat(auth): add auth_db_path to AppState, registry, and main.rs"
```

---

## Task 6: Create `auth.rs` — AuthUser extractor

**Files:**
- Create: `crates/archivr-server/src/auth.rs`
- Modify: `crates/archivr-server/src/routes.rs` (add `mod auth; use auth::AuthUser;`)

- [ ] **Step 1: Create `auth.rs`**

Create `crates/archivr-server/src/auth.rs` with the following content:

```rust
use anyhow::Result;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use argon2::password_hash::{SaltString, rand_core::OsRng};
use axum::{async_trait, extract::FromRequestParts, http::request::Parts};
use axum_extra::extract::CookieJar;
use rand::RngCore;

use crate::routes::{ApiError, AppState};
use archivr_core::database;

// ── Role bit constants ────────────────────────────────────────────────────────
pub const ROLE_GUEST: u32 = 1;  // bit 0
pub const ROLE_USER:  u32 = 2;  // bit 1
pub const ROLE_ADMIN: u32 = 4;  // bit 2
pub const ROLE_OWNER: u32 = 8;  // bit 3

// ── AuthUser ─────────────────────────────────────────────────────────────────
#[derive(Clone, Debug)]
pub enum AuthUser {
    Guest,
    Authenticated { user_id: i64, role_bits: u32 },
}

impl AuthUser {
    /// Returns (user_id, role_bits) or 401 if Guest.
    pub fn require_auth(&self) -> Result<(i64, u32), ApiError> {
        match self {
            AuthUser::Authenticated { user_id, role_bits } => Ok((*user_id, *role_bits)),
            AuthUser::Guest => Err(ApiError::unauthorized("login required")),
        }
    }

    /// Returns Ok(()) if the user has the given role bit set, else 401/403.
    pub fn require_role(&self, bit: u32) -> Result<(), ApiError> {
        match self {
            AuthUser::Authenticated { role_bits, .. } if role_bits & bit != 0 => Ok(()),
            AuthUser::Authenticated { .. } => Err(ApiError::forbidden("insufficient permissions")),
            AuthUser::Guest => Err(ApiError::unauthorized("login required")),
        }
    }

    pub fn has_role(&self, bit: u32) -> bool {
        matches!(self, AuthUser::Authenticated { role_bits, .. } if role_bits & bit != 0)
    }
}

#[async_trait]
impl FromRequestParts<AppState> for AuthUser {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, std::convert::Infallible> {
        let auth_db_path = state.auth_db_path.as_ref();

        // 1. Try session cookie
        let jar = CookieJar::from_headers(&parts.headers);
        if let Some(cookie) = jar.get("session") {
            let session_uid = cookie.value().to_string();
            if let Ok(conn) = database::open_auth_db(auth_db_path) {
                if let Ok(Some(session)) = database::get_session(&conn, &session_uid) {
                    // Conditional touch: only update last_seen_at if more than 60s have elapsed.
                    // The session row is already in memory, so no extra query needed.
                    let should_touch = chrono::NaiveDateTime::parse_from_str(
                        &session.last_seen_at, "%Y-%m-%dT%H-%M-%S%.3f",
                    )
                    .map(|last| {
                        let last_utc = chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(
                            last, chrono::Utc,
                        );
                        chrono::Utc::now() - last_utc > chrono::Duration::seconds(60)
                    })
                    .unwrap_or(true); // if parse fails, touch anyway
                    if should_touch {
                        let _ = database::touch_session(&conn, &session_uid);
                    }
                    return Ok(AuthUser::Authenticated {
                        user_id: session.user_id,
                        role_bits: session.role_bits,
                    });
                }
            }
        }

        // 2. Try Bearer token
        if let Some(auth_header) = parts.headers.get("Authorization") {
            if let Ok(header_str) = auth_header.to_str() {
                if let Some(raw_token) = header_str.strip_prefix("Bearer ") {
                    let token_hash = hash_token(raw_token);
                    if let Ok(conn) = database::open_auth_db(auth_db_path) {
                        if let Ok(Some(user_id)) = database::get_user_for_token(&conn, &token_hash) {
                            // Get token_uid for touch (find by hash)
                            // Compute role_bits live for tokens
                            if let Ok(role_bits) = database::compute_role_bits(&conn, user_id) {
                                return Ok(AuthUser::Authenticated { user_id, role_bits });
                            }
                        }
                    }
                }
            }
        }

        Ok(AuthUser::Guest)
    }
}

// ── Password helpers ──────────────────────────────────────────────────────────

pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("password hashing failed: {e}"))?
        .to_string();
    Ok(hash)
}

pub fn verify_password(password: &str, hash: &str) -> Result<bool> {
    let parsed = PasswordHash::new(hash)
        .map_err(|e| anyhow::anyhow!("invalid password hash: {e}"))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

// ── Token helpers ─────────────────────────────────────────────────────────────

/// Generates a cryptographically random 32-byte token, base64url-encoded.
pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, bytes)
}

/// SHA3-256 hex hash of a raw token string. Used for storage and lookup.
pub fn hash_token(raw: &str) -> String {
    archivr_core::hash::hash_bytes(raw.as_bytes())
}
```

- [ ] **Step 2: Wire into `routes.rs`**

At the top of `routes.rs`, add after `mod` declarations:

```rust
mod auth;
pub use auth::{AuthUser, ROLE_ADMIN, ROLE_OWNER, ROLE_USER};
```

Also add `unauthorized` and `forbidden` constructors to `ApiError`, and update `IntoResponse` to return JSON:

```rust
impl ApiError {
    // ... existing constructors ...

    pub fn unauthorized(message: &str) -> Self {
        Self { status: StatusCode::UNAUTHORIZED, message: message.to_string() }
    }

    pub fn forbidden(message: &str) -> Self {
        Self { status: StatusCode::FORBIDDEN, message: message.to_string() }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({ "error": self.message });
        (self.status, axum::Json(body)).into_response()
    }
}
```

- [ ] **Step 3: Compile check**

```bash
cd ~/personal/archivr && cargo check -p archivr-server 2>&1 | tail -10
```

Fix any import errors. Common fix: add `use std::path::Path;` or check `axum_extra` cookie import.

- [ ] **Step 4: Write extractor tests in `routes.rs`**

In the `#[cfg(test)]` block in `routes.rs`, add helpers and tests:

```rust
fn make_test_app() -> (Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let auth_db_path = dir.path().join("auth.sqlite");
    let registry = ServerRegistry { archives: vec![], bind: None, auth_db_path: None };
    (app(registry, auth_db_path), dir)
}

#[tokio::test]
async fn health_check_returns_ok() {
    let (app, _dir) = make_test_app();
    let response = app
        .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
```

Update the existing `archives_endpoint_lists_mounted_archives` test to use `make_test_app()`:

```rust
#[tokio::test]
async fn archives_endpoint_lists_mounted_archives() {
    let dir = tempfile::tempdir().unwrap();
    let auth_db_path = dir.path().join("auth.sqlite");
    let registry = ServerRegistry {
        archives: vec![MountedArchive {
            id: "personal".to_string(),
            label: "Personal".to_string(),
            archive_path: std::path::PathBuf::from("/tmp/personal/.archivr"),
        }],
        bind: None,
        auth_db_path: None,
    };
    let response = app(registry, auth_db_path)
        .oneshot(Request::builder().uri("/api/archives").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
```

Apply the same pattern to **every other test** in the module that calls `app(registry)`.

- [ ] **Step 5: Run all tests**

```bash
cd ~/personal/archivr && cargo test 2>&1 | tail -5
```

Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/archivr-server/src/auth.rs crates/archivr-server/src/routes.rs
git commit -m "feat(auth): AuthUser extractor, password helpers, token generation"
```

---

## Task 7: Auth endpoints — login, logout, /me, setup

**Files:**
- Modify: `crates/archivr-server/src/routes.rs`

Add the following to `routes.rs`.

- [ ] **Step 1: Write failing tests for auth endpoints**

```rust
#[tokio::test]
async fn setup_required_before_owner_created() {
    let (app, _dir) = make_test_app();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/auth/setup")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["setup_required"], true);
}

#[tokio::test]
async fn login_with_wrong_password_returns_401() {
    let dir = tempfile::tempdir().unwrap();
    let auth_db_path = dir.path().join("auth.sqlite");
    // Seed owner
    {
        let conn = archivr_core::database::open_auth_db(&auth_db_path).unwrap();
        let hash = crate::auth::hash_password("correct").unwrap();
        archivr_core::database::create_owner(&conn, "owner", &hash).unwrap();
    }
    let registry = ServerRegistry { archives: vec![], bind: None, auth_db_path: None };
    let response = app(registry, auth_db_path)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"username":"owner","password":"wrong"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn setup_post_creates_owner_and_returns_409_on_repeat() {
    let (app_once, dir) = make_test_app();
    let auth_db_path = dir.path().join("auth.sqlite");
    let registry = ServerRegistry { archives: vec![], bind: None, auth_db_path: None };

    // First POST creates the owner
    let response = app_once
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/setup")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"username":"owner","password":"hunter2"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    // Second POST must return 409
    let app2 = app(registry, auth_db_path);
    let response2 = app2
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/setup")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"username":"owner2","password":"hunter2"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response2.status(), StatusCode::CONFLICT);
}
```

- [ ] **Step 2: Run to confirm failure**

```bash
cd ~/personal/archivr && cargo test -p archivr-server setup_required login_with_wrong 2>&1 | tail -10
```

Expected: FAILED.

- [ ] **Step 3: Add auth routes to `app()` in `routes.rs`**

In the `app()` function, add these routes before `.with_state(state)`:

```rust
.route("/api/auth/setup",  get(auth_setup_status).post(auth_setup))
.route("/api/auth/login",  post(auth_login))
.route("/api/auth/logout", post(auth_logout))
.route("/api/auth/me",     get(auth_me))
// Setup guard must come AFTER routes are defined and BEFORE .with_state
.layer(axum::middleware::from_fn_with_state(state.clone(), setup_guard))
```

- [ ] **Step 3b: Implement `setup_guard` middleware**

Add this function before the `app()` definition:

```rust
/// Tower middleware: returns 503 on all non-exempt routes if setup hasn't been completed.
async fn setup_guard(
    State(state): State<AppState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let path = req.uri().path();
    let exempt = path == "/api/auth/setup"
        || path == "/api/auth/login"
        || path.starts_with("/assets")
        || path == "/"
        || path == "/health";
    if !exempt {
        if let Ok(conn) = database::open_auth_db(&state.auth_db_path) {
            if matches!(database::ensure_owner_exists(&conn), Ok(false)) {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    axum::Json(serde_json::json!({ "error": "setup_required" })),
                )
                    .into_response();
            }
        }
    }
    next.run(req).await
}
```

- [ ] **Step 4: Add request/response types**

After the existing `#[derive(serde::Deserialize)]` structs, add:

```rust
#[derive(Debug, serde::Deserialize)]
struct LoginBody {
    username: String,
    password: String,
}

#[derive(Debug, serde::Deserialize)]
struct SetupBody {
    username: String,
    password: String,
}
```

- [ ] **Step 5: Implement handler functions**

Add after the existing handlers:

```rust
async fn auth_setup_status(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let conn = database::open_auth_db(&state.auth_db_path)?;
    let required = !database::ensure_owner_exists(&conn)?;
    Ok(Json(serde_json::json!({ "setup_required": required })))
}

async fn auth_setup(
    State(state): State<AppState>,
    Json(body): Json<SetupBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let conn = database::open_auth_db(&state.auth_db_path)?;
    if database::ensure_owner_exists(&conn)? {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            message: "already_configured".to_string(),
        });
    }
    if body.username.trim().is_empty() || body.password.len() < 8 {
        return Err(ApiError::bad_request("username required and password must be at least 8 characters"));
    }
    let hash = auth::hash_password(&body.password).map_err(ApiError::from)?;
    let user_id = database::create_owner(&conn, &body.username, &hash)?;
    let user = database::get_user_by_username(&conn, &body.username)?
        .ok_or_else(|| ApiError::internal("user not found after creation"))?;
    Ok((StatusCode::CREATED, Json(serde_json::json!({
        "user_uid": user.user_uid,
        "username": user.username,
    }))))
}

async fn auth_login(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<LoginBody>,
) -> Result<(StatusCode, axum::http::HeaderMap, Json<serde_json::Value>), ApiError> {
    let conn = database::open_auth_db(&state.auth_db_path)?;
    let user = database::get_user_by_username(&conn, &body.username)?
        .filter(|u| u.status == "active")
        .ok_or_else(|| ApiError::unauthorized("invalid_credentials"))?;
    if !auth::verify_password(&body.password, &user.password_hash)
        .map_err(ApiError::from)?
    {
        return Err(ApiError::unauthorized("invalid_credentials"));
    }
    let role_bits = database::compute_role_bits(&conn, user.id)?;
    let user_agent = headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok());
    let session_uid = database::create_session(&conn, user.id, role_bits, user_agent)?;

    // Build Set-Cookie header
    let secure = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|v| v == "https")
        .unwrap_or(false);
    let cookie_value = format!(
        "session={}; HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000{}",
        session_uid,
        if secure { "; Secure" } else { "" }
    );
    let mut resp_headers = axum::http::HeaderMap::new();
    resp_headers.insert(
        axum::http::header::SET_COOKIE,
        cookie_value.parse().map_err(|_| ApiError::internal("cookie serialization failed"))?,
    );

    Ok((StatusCode::OK, resp_headers, Json(serde_json::json!({
        "user_uid": user.user_uid,
        "username": user.username,
        "role_bits": role_bits,
    }))))
}

async fn auth_logout(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<(StatusCode, axum::http::HeaderMap), ApiError> {
    if let Some(cookie) = jar.get("session") {
        let conn = database::open_auth_db(&state.auth_db_path)?;
        database::delete_session(&conn, cookie.value())?;
    }
    let mut resp_headers = axum::http::HeaderMap::new();
    resp_headers.insert(
        axum::http::header::SET_COOKIE,
        "session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"
            .parse()
            .unwrap(),
    );
    Ok((StatusCode::NO_CONTENT, resp_headers))
}

async fn auth_me(
    State(state): State<AppState>,
    auth_user: AuthUser,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (user_id, role_bits) = auth_user.require_auth()?;
    let conn = database::open_auth_db(&state.auth_db_path)?;
    // Look up username by user_id for response
    let username: String = conn
        .query_row("SELECT username FROM users WHERE id = ?1", [user_id], |r| r.get(0))
        .map_err(ApiError::from)?;
    Ok(Json(serde_json::json!({
        "role_bits": role_bits,
        "username": username,
    })))
}
```

Add `use axum_extra::extract::CookieJar;` at the top of `routes.rs` imports.

- [ ] **Step 6: Run tests**

```bash
cd ~/personal/archivr && cargo test -p archivr-server 2>&1 | tail -5
```

Expected: all tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/archivr-server/src/routes.rs
git commit -m "feat(auth): login, logout, /me, setup endpoints"
```

---

## Task 8: API token endpoints

**Files:**
- Modify: `crates/archivr-server/src/routes.rs`

- [ ] **Step 1: Write failing test**

```rust
#[tokio::test]
async fn create_token_requires_auth() {
    let (app, _dir) = make_test_app();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/tokens")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name":"my token"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
```

- [ ] **Step 2: Add token routes to `app()`**

```rust
.route("/api/auth/tokens", get(list_tokens).post(create_token))
.route("/api/auth/tokens/:token_uid", delete(delete_token))
```

- [ ] **Step 3: Add request type**

```rust
#[derive(Debug, serde::Deserialize)]
struct CreateTokenBody {
    name: String,
}
```

- [ ] **Step 4: Implement token handlers**

```rust
async fn create_token(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Json(body): Json<CreateTokenBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let (user_id, _) = auth_user.require_auth()?;
    if body.name.trim().is_empty() {
        return Err(ApiError::bad_request("token name is required"));
    }
    let raw_token = auth::generate_token();
    let token_hash = auth::hash_token(&raw_token);
    let conn = database::open_auth_db(&state.auth_db_path)?;
    let token_uid = database::create_api_token(&conn, user_id, &token_hash, &body.name)?;
    Ok((StatusCode::CREATED, Json(serde_json::json!({
        "token_uid": token_uid,
        "raw_token": raw_token,
        "name": body.name,
    }))))
}

async fn list_tokens(
    State(state): State<AppState>,
    auth_user: AuthUser,
) -> Result<Json<Vec<database::ApiTokenRecord>>, ApiError> {
    let (user_id, _) = auth_user.require_auth()?;
    let conn = database::open_auth_db(&state.auth_db_path)?;
    Ok(Json(database::list_user_tokens(&conn, user_id)?))
}

async fn delete_token(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(token_uid): Path<String>,
) -> Result<StatusCode, ApiError> {
    let (user_id, _) = auth_user.require_auth()?;
    let conn = database::open_auth_db(&state.auth_db_path)?;
    if database::delete_api_token(&conn, &token_uid, user_id)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("token not found"))
    }
}
```

- [ ] **Step 5: Derive `serde::Serialize` on `ApiTokenRecord`**

In `database.rs`, update the struct:

```rust
#[derive(Debug, Clone, serde::Serialize)]
pub struct ApiTokenRecord { ... }
```

- [ ] **Step 6: Run tests**

```bash
cd ~/personal/archivr && cargo test -p archivr-server 2>&1 | tail -5
```

Expected: all pass including `create_token_requires_auth`.

- [ ] **Step 7: Commit**

```bash
git add crates/archivr-server/src/routes.rs crates/archivr-core/src/database.rs
git commit -m "feat(auth): API token endpoints (create, list, delete)"
```

---

## Task 9: Route protection for existing routes

**Files:**
- Modify: `crates/archivr-server/src/routes.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[tokio::test]
async fn capture_returns_401_for_unauthenticated() {
    let (app, _dir) = make_test_app();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/archives/test/captures")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"locator":"https://example.com"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
```

- [ ] **Step 2: Run to confirm failure**

```bash
cd ~/personal/archivr && cargo test -p archivr-server capture_returns_401 2>&1 | tail -5
```

Expected: FAILED (currently returns 200 or 404).

- [ ] **Step 3: Add `AuthUser` to WRITE handlers**

Update `capture_handler` to require auth:

```rust
async fn capture_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(archive_id): Path<String>,
    Json(body): Json<CaptureBody>,
) -> Result<Json<capture::CaptureResult>, ApiError> {
    auth_user.require_role(ROLE_USER)?;
    // ... rest unchanged
}
```

Update `create_tag_handler`, `assign_entry_tag_handler`, `remove_entry_tag_handler` with the same `auth_user: AuthUser` parameter and `auth_user.require_role(ROLE_USER)?` guard.

- [ ] **Step 4: Update security-boundary comment in `routes.rs`**

Find the comment block at the top of `routes.rs` (lines 1–23). Replace the route classification list to reflect the new protection:

```rust
// ── Security Boundary ──────────────────────────────────────────────────────
//
// Route protection tiers:
//   STATIC    — no auth: GET /, GET /assets/*
//   PUBLIC_READ — no auth (visibility filtering deferred to Track 6):
//                 GET /api/archives, GET /api/archives/:id/entries, etc.
//   AUTH      — requires login (ROLE_USER bit):
//                 POST /api/archives/:id/captures
//                 POST/PUT/DELETE /api/archives/:id/tags
//                 POST/DELETE /api/archives/:id/entries/:uid/tags
//   ADMIN     — requires ROLE_ADMIN:
//                 GET /api/admin/archives (future)
//   OWNER     — requires ROLE_OWNER:
//                 instance settings (future)
//   AUTH_SELF — no role guard (own resources):
//                 GET/POST/DELETE /api/auth/tokens
//                 POST /api/auth/logout
//                 GET /api/auth/me
// ────────────────────────────────────────────────────────────────────────────
```

- [ ] **Step 5: Run all tests**

```bash
cd ~/personal/archivr && cargo test 2>&1 | tail -5
```

Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add crates/archivr-server/src/routes.rs
git commit -m "feat(auth): apply ROLE_USER guard to WRITE routes"
```

---

## Task 10: Session cleanup background task

**Files:**
- Modify: `crates/archivr-server/src/main.rs`

- [ ] **Step 1: Add cleanup task to `main.rs`**

After the `app` binding and before `let listener`, add:

```rust
// Spawn session cleanup background task: delete expired sessions at startup
// and every 24 hours.
let cleanup_auth_path = auth_db_path.clone();
tokio::spawn(async move {
    loop {
        if let Ok(conn) = archivr_core::database::open_auth_db(&cleanup_auth_path) {
            match archivr_core::database::delete_expired_sessions(&conn) {
                Ok(n) if n > 0 => eprintln!("info: cleaned up {n} expired sessions"),
                Err(e) => eprintln!("warn: session cleanup failed: {e:#}"),
                _ => {}
            }
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(24 * 60 * 60)).await;
    }
});
```

Add `use archivr_core;` at the top if not already present (it's brought in transitively via routes).

- [ ] **Step 2: Verify compilation**

```bash
cd ~/personal/archivr && cargo check -p archivr-server 2>&1 | tail -5
```

Expected: compiles cleanly.

- [ ] **Step 3: Commit**

```bash
git add crates/archivr-server/src/main.rs
git commit -m "feat(auth): session cleanup background task (24h interval)"
```

---

## Task 11: Frontend — auth state, api.js, LoginPage, SetupPage

**Files:**
- Modify: `frontend/src/App.jsx`
- Modify: `frontend/src/api.js`
- Create: `frontend/src/components/LoginPage.jsx`
- Create: `frontend/src/components/SetupPage.jsx`

- [ ] **Step 1: Update `api.js`**

Open `frontend/src/api.js`. Add auth helper functions and the 401 interceptor at the bottom of the file (replace or append to the existing `export` structure):

```js
// ── Auth helpers ─────────────────────────────────────────────────────────────

export async function checkSetup() {
  const r = await fetch('/api/auth/setup');
  const data = await r.json();
  return data.setup_required === true;
}

export async function doSetup(username, password) {
  const r = await fetch('/api/auth/setup', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ username, password }),
  });
  if (!r.ok) throw new Error((await r.json()).error || 'Setup failed');
  return r.json();
}

export async function login(username, password) {
  const r = await fetch('/api/auth/login', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ username, password }),
  });
  if (!r.ok) throw new Error((await r.json()).error || 'Login failed');
  return r.json(); // { user_uid, username, role_bits }
}

export async function logout() {
  await fetch('/api/auth/logout', { method: 'POST' });
}

export async function fetchMe() {
  const r = await fetch('/api/auth/me');
  if (r.status === 401) return null;
  return r.json();
}

// ── 401 interceptor ───────────────────────────────────────────────────────────
// Wrap fetch so any 401 dispatches a custom event for App.jsx to handle.
const _origFetch = window.fetch;
window.fetch = async (...args) => {
  const r = await _origFetch(...args);
  if (r.status === 401) {
    const url = typeof args[0] === 'string' ? args[0] : args[0]?.url ?? '';
    // Don't intercept auth endpoints themselves
    if (!url.includes('/api/auth/')) {
      window.dispatchEvent(new CustomEvent('auth:expired'));
    }
  }
  return r;
};
```

- [ ] **Step 2: Create `LoginPage.jsx`**

```jsx
import { useState } from 'react';
import { login } from '../api.js';

export default function LoginPage({ onLogin }) {
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [error, setError] = useState(null);
  const [loading, setLoading] = useState(false);

  async function handleSubmit(e) {
    e.preventDefault();
    setError(null);
    setLoading(true);
    try {
      const user = await login(username, password);
      onLogin(user);
    } catch (err) {
      setError(err.message);
    } finally {
      setLoading(false);
    }
  }

  return (
    <div className="login-page">
      <h1>Archivr</h1>
      <form onSubmit={handleSubmit}>
        <label>
          Username
          <input
            type="text"
            value={username}
            onChange={e => setUsername(e.target.value)}
            autoFocus
            required
          />
        </label>
        <label>
          Password
          <input
            type="password"
            value={password}
            onChange={e => setPassword(e.target.value)}
            required
          />
        </label>
        {error && <p className="error">{error}</p>}
        <button type="submit" disabled={loading}>
          {loading ? 'Logging in…' : 'Log in'}
        </button>
      </form>
    </div>
  );
}
```

- [ ] **Step 3: Create `SetupPage.jsx`**

```jsx
import { useState } from 'react';
import { doSetup } from '../api.js';

export default function SetupPage({ onComplete }) {
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [confirm, setConfirm] = useState('');
  const [error, setError] = useState(null);
  const [loading, setLoading] = useState(false);

  async function handleSubmit(e) {
    e.preventDefault();
    if (password !== confirm) {
      setError('Passwords do not match');
      return;
    }
    if (password.length < 8) {
      setError('Password must be at least 8 characters');
      return;
    }
    setError(null);
    setLoading(true);
    try {
      await doSetup(username, password);
      onComplete();
    } catch (err) {
      setError(err.message);
    } finally {
      setLoading(false);
    }
  }

  return (
    <div className="setup-page">
      <h1>Welcome to Archivr</h1>
      <p>Create your owner account to get started.</p>
      <form onSubmit={handleSubmit}>
        <label>
          Username
          <input
            type="text"
            value={username}
            onChange={e => setUsername(e.target.value)}
            autoFocus
            required
          />
        </label>
        <label>
          Password
          <input
            type="password"
            value={password}
            onChange={e => setPassword(e.target.value)}
            required
          />
        </label>
        <label>
          Confirm password
          <input
            type="password"
            value={confirm}
            onChange={e => setConfirm(e.target.value)}
            required
          />
        </label>
        {error && <p className="error">{error}</p>}
        <button type="submit" disabled={loading}>
          {loading ? 'Creating account…' : 'Create account'}
        </button>
      </form>
    </div>
  );
}
```

- [ ] **Step 4: Update `App.jsx`**

Open `frontend/src/App.jsx`. Add the following at the top (after existing imports):

```jsx
import { createContext, useContext, useState, useEffect, useCallback } from 'react';
import { checkSetup, fetchMe, logout as apiLogout } from './api.js';
import LoginPage from './components/LoginPage.jsx';
import SetupPage from './components/SetupPage.jsx';

export const AuthContext = createContext(null);
```

Inside the `App` function, add state and effects at the top:

```jsx
const [authState, setAuthState] = useState('loading'); // 'loading' | 'setup' | 'login' | 'authenticated'
const [currentUser, setCurrentUser] = useState(null);

useEffect(() => {
  (async () => {
    const needsSetup = await checkSetup();
    if (needsSetup) { setAuthState('setup'); return; }
    const user = await fetchMe();
    if (!user) { setAuthState('login'); return; }
    setCurrentUser(user);
    setAuthState('authenticated');
  })();
}, []);

// Listen for 401s from the fetch interceptor in api.js
useEffect(() => {
  const handler = () => { setCurrentUser(null); setAuthState('login'); };
  window.addEventListener('auth:expired', handler);
  return () => window.removeEventListener('auth:expired', handler);
}, []);
```

In the `App` return, add guards at the very top of the JSX:

```jsx
if (authState === 'loading') return <div>Loading…</div>;
if (authState === 'setup')   return <SetupPage onComplete={() => setAuthState('login')} />;
if (authState === 'login')   return <LoginPage onLogin={user => { setCurrentUser(user); setAuthState('authenticated'); }} />;
```

Wrap the existing return content in:

```jsx
return (
  <AuthContext.Provider value={{ currentUser, setCurrentUser }}>
    {/* existing app JSX */}
  </AuthContext.Provider>
);
```

- [ ] **Step 5: Build to check for errors**

```bash
cd ~/personal/archivr/frontend && bun run build 2>&1 | tail -10
```

Expected: builds successfully, no type errors.

- [ ] **Step 6: Commit**

```bash
cd ~/personal/archivr
git add frontend/src/App.jsx frontend/src/api.js \
        frontend/src/components/LoginPage.jsx \
        frontend/src/components/SetupPage.jsx
git commit -m "feat(auth): frontend login/setup pages + auth state in App.jsx"
```

---

## Task 12: Topbar — user menu and logout button

**Files:**
- Modify: `frontend/src/components/Topbar.jsx`

- [ ] **Step 1: Update `Topbar.jsx`**

Open `frontend/src/components/Topbar.jsx`. Import `AuthContext` and `logout`:

```jsx
import { useContext, useState } from 'react';
import { AuthContext } from '../App.jsx';
import { logout as apiLogout } from '../api.js';
```

Inside the component, add:

```jsx
const { currentUser, setCurrentUser } = useContext(AuthContext) ?? {};
const [loggingOut, setLoggingOut] = useState(false);

async function handleLogout() {
  setLoggingOut(true);
  await apiLogout();
  setCurrentUser(null);
  window.location.reload(); // simplest way to reset all state
}
```

In the JSX, add a user menu at the end of the topbar:

```jsx
{currentUser && (
  <div className="user-menu">
    <span className="username">{currentUser.username}</span>
    <button onClick={handleLogout} disabled={loggingOut} className="logout-btn">
      {loggingOut ? 'Logging out…' : 'Log out'}
    </button>
  </div>
)}
```

- [ ] **Step 2: Build to check for errors**

```bash
cd ~/personal/archivr/frontend && bun run build 2>&1 | tail -5
```

Expected: clean build.

- [ ] **Step 3: Commit**

```bash
cd ~/personal/archivr
git add frontend/src/components/Topbar.jsx
git commit -m "feat(auth): user menu and logout button in Topbar"
```

---

## Task 13: Update NEXT.md

**Files:**
- Modify: `NEXT.md`

- [ ] **Step 1: Add Track 4 as done, renumber remaining tracks**

Open `NEXT.md`. Add a new Track 4 section (done) after Track 3, following the same ~~strikethrough~~ ✅ pattern as Tracks 1 and 2. Rename the old Track 4 (Cloud backup) to Track 9 and Track 5 (Cloud storage) to Track 10. Add stub sections for new Tracks 5, 6, 7, 8 with brief descriptions and "pending" status.

Use this numbering:

| # | Track | Status |
|---|---|---|
| 1 | Generic URL capture | Done |
| 2 | Web page archiving | Done |
| 3 | Async capture jobs | Pending |
| **4** | **Auth foundation** | **Done (this track)** |
| 5 | User management | Pending |
| 6 | Permissions & visibility | Pending |
| 7 | Settings | Pending |
| 8 | Collections UI | Pending |
| 9 | Cloud backup | Pending (was 4) |
| 10 | Cloud storage | Pending (was 5) |

- [ ] **Step 2: Commit**

```bash
git add NEXT.md
git commit -m "docs: mark Track 4 auth foundation done, renumber roadmap tracks"
```

---

## Final verification

- [ ] **Run the full test suite**

```bash
cd ~/personal/archivr && cargo test 2>&1 | tail -5
```

Expected: all tests pass (count will be higher than the pre-implementation baseline due to new tests).

- [ ] **Build the frontend**

```bash
cd ~/personal/archivr/frontend && bun run build 2>&1 | tail -5
```

Expected: builds cleanly.
