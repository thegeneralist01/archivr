use anyhow::{Context, Result, bail};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub const DATABASE_FILE_NAME: &str = "archivr.sqlite";
pub const DEFAULT_USERNAME: &str = "local-admin";

#[derive(Debug, Clone)]
pub struct ArchiveRun {
    pub id: i64,
    pub run_uid: String,
}

#[derive(Debug, Clone)]
pub struct ArchiveRunItem {
    pub id: i64,
    pub item_uid: String,
}

#[derive(Debug, Clone)]
pub struct ArchivedEntry {
    pub id: i64,
    pub entry_uid: String,
    pub structured_root_relpath: String,
}

#[derive(Debug, Clone)]
pub struct BlobRecord {
    pub sha256: String,
    pub byte_size: i64,
    pub mime_type: Option<String>,
    pub extension: Option<String>,
    pub raw_relpath: String,
}

#[derive(Debug, Clone)]
pub struct NewEntry {
    pub source_identity_id: i64,
    pub archive_run_id: i64,
    pub parent_entry_id: Option<i64>,
    pub root_entry_id: Option<i64>,
    pub created_by_user_id: i64,
    pub owned_by_user_id: i64,
    pub source_kind: String,
    pub entity_kind: String,
    pub title: Option<String>,
    pub visibility: String,
    pub representation_kind: String,
    pub source_metadata_json: String,
    pub display_metadata_json: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewArtifact {
    pub entry_id: i64,
    pub artifact_role: String,
    pub storage_area: String,
    pub relpath: String,
    pub blob_id: Option<i64>,
    pub logical_path: Option<String>,
    pub metadata_json: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TagRecord {
    pub id: i64,
    pub tag_uid: String,
    pub parent_tag_id: Option<i64>,
    pub name: String,
    pub slug: String,
    pub full_path: String,
}

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

#[derive(Debug, Clone, serde::Serialize)]
pub struct ApiTokenRecord {
    pub token_uid: String,
    pub name: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

pub fn database_path(archive_path: &Path) -> PathBuf {
    archive_path.join(DATABASE_FILE_NAME)
}

pub fn open_or_initialize(archive_path: &Path) -> Result<Connection> {
    let conn = Connection::open(database_path(archive_path)).with_context(|| {
        format!(
            "failed to open archive database in {}",
            archive_path.display()
        )
    })?;
    initialize_schema(&conn)?;
    Ok(conn)
}

pub fn initialize_schema(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS users (
            id INTEGER PRIMARY KEY,
            user_uid TEXT NOT NULL UNIQUE,
            username TEXT NOT NULL UNIQUE,
            email TEXT UNIQUE,
            password_hash TEXT NOT NULL,
            status TEXT NOT NULL CHECK (status IN ('active', 'disabled')),
            role TEXT NOT NULL CHECK (role IN ('admin', 'user')),
            created_at TEXT NOT NULL,
            last_login_at TEXT
        );

        CREATE TABLE IF NOT EXISTS instance_settings (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            public_index_enabled INTEGER NOT NULL DEFAULT 0 CHECK (public_index_enabled IN (0, 1)),
            public_entry_content_enabled INTEGER NOT NULL DEFAULT 0 CHECK (public_entry_content_enabled IN (0, 1)),
            public_archive_submission_enabled INTEGER NOT NULL DEFAULT 0 CHECK (public_archive_submission_enabled IN (0, 1))
        );

        INSERT OR IGNORE INTO instance_settings (
            id,
            public_index_enabled,
            public_entry_content_enabled,
            public_archive_submission_enabled
        ) VALUES (1, 0, 0, 0);

        CREATE TABLE IF NOT EXISTS archive_runs (
            id INTEGER PRIMARY KEY,
            run_uid TEXT NOT NULL UNIQUE,
            created_by_user_id INTEGER NOT NULL REFERENCES users(id),
            started_at TEXT NOT NULL,
            finished_at TEXT,
            status TEXT NOT NULL CHECK (status IN ('in_progress', 'completed', 'failed')),
            requested_count INTEGER NOT NULL DEFAULT 0,
            discovered_count INTEGER NOT NULL DEFAULT 0,
            completed_count INTEGER NOT NULL DEFAULT 0,
            failed_count INTEGER NOT NULL DEFAULT 0,
            error_summary TEXT
        );

        CREATE TABLE IF NOT EXISTS archive_run_items (
            id INTEGER PRIMARY KEY,
            run_id INTEGER NOT NULL REFERENCES archive_runs(id) ON DELETE CASCADE,
            item_uid TEXT NOT NULL UNIQUE,
            parent_item_id INTEGER REFERENCES archive_run_items(id),
            ordinal INTEGER NOT NULL,
            requested_locator TEXT NOT NULL,
            canonical_locator TEXT,
            source_kind TEXT NOT NULL,
            entity_kind TEXT NOT NULL,
            status TEXT NOT NULL CHECK (status IN ('pending', 'in_progress', 'completed', 'failed')),
            error_text TEXT,
            produced_entry_id INTEGER REFERENCES archived_entries(id)
        );

        CREATE TABLE IF NOT EXISTS source_identities (
            id INTEGER PRIMARY KEY,
            source_kind TEXT NOT NULL,
            entity_kind TEXT NOT NULL,
            external_id TEXT,
            canonical_url TEXT,
            normalized_locator TEXT NOT NULL,
            identity_key TEXT NOT NULL UNIQUE
        );

        CREATE TABLE IF NOT EXISTS archived_entries (
            id INTEGER PRIMARY KEY,
            entry_uid TEXT NOT NULL UNIQUE,
            source_identity_id INTEGER NOT NULL REFERENCES source_identities(id),
            archive_run_id INTEGER NOT NULL REFERENCES archive_runs(id),
            parent_entry_id INTEGER REFERENCES archived_entries(id),
            root_entry_id INTEGER REFERENCES archived_entries(id),
            created_by_user_id INTEGER NOT NULL REFERENCES users(id),
            owned_by_user_id INTEGER NOT NULL REFERENCES users(id),
            source_kind TEXT NOT NULL,
            entity_kind TEXT NOT NULL,
            title TEXT,
            visibility TEXT NOT NULL CHECK (visibility IN ('private', 'unlisted', 'public')),
            archived_at TEXT NOT NULL,
            original_published_at TEXT,
            structured_root_relpath TEXT NOT NULL,
            representation_kind TEXT NOT NULL,
            source_metadata_json TEXT NOT NULL DEFAULT '{}',
            display_metadata_json TEXT
        );

        CREATE TABLE IF NOT EXISTS blobs (
            id INTEGER PRIMARY KEY,
            sha256 TEXT NOT NULL UNIQUE,
            byte_size INTEGER NOT NULL,
            mime_type TEXT,
            extension TEXT,
            raw_relpath TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS entry_artifacts (
            id INTEGER PRIMARY KEY,
            entry_id INTEGER NOT NULL REFERENCES archived_entries(id) ON DELETE CASCADE,
            artifact_role TEXT NOT NULL,
            storage_area TEXT NOT NULL CHECK (storage_area IN ('raw', 'raw_tweets', 'structured')),
            relpath TEXT NOT NULL,
            blob_id INTEGER REFERENCES blobs(id),
            logical_path TEXT,
            metadata_json TEXT
        );

        CREATE TABLE IF NOT EXISTS tags (
            id INTEGER PRIMARY KEY,
            tag_uid TEXT NOT NULL UNIQUE,
            parent_tag_id INTEGER REFERENCES tags(id),
            name TEXT NOT NULL,
            slug TEXT NOT NULL,
            full_path TEXT NOT NULL UNIQUE
        );

        CREATE TABLE IF NOT EXISTS entry_tag_assignments (
            entry_id INTEGER NOT NULL REFERENCES archived_entries(id) ON DELETE CASCADE,
            tag_id INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
            PRIMARY KEY (entry_id, tag_id)
        );

        CREATE INDEX IF NOT EXISTS idx_archive_run_items_run_id ON archive_run_items(run_id);
        CREATE INDEX IF NOT EXISTS idx_archived_entries_source_identity_id ON archived_entries(source_identity_id);
        CREATE INDEX IF NOT EXISTS idx_archived_entries_created_by_user_id ON archived_entries(created_by_user_id);
        CREATE INDEX IF NOT EXISTS idx_archived_entries_parent_entry_id ON archived_entries(parent_entry_id);
        CREATE INDEX IF NOT EXISTS idx_archived_entries_root_entry_id ON archived_entries(root_entry_id);
        CREATE INDEX IF NOT EXISTS idx_archived_entries_visibility ON archived_entries(visibility);
        CREATE INDEX IF NOT EXISTS idx_entry_artifacts_entry_id ON entry_artifacts(entry_id);
        CREATE INDEX IF NOT EXISTS idx_entry_artifacts_blob_id ON entry_artifacts(blob_id);
        CREATE INDEX IF NOT EXISTS idx_tags_parent_tag_id ON tags(parent_tag_id);
        CREATE INDEX IF NOT EXISTS idx_entry_tag_assignments_tag_id ON entry_tag_assignments(tag_id);
        "#,
    )?;
    Ok(())
}

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

/// Computes role_bits = ROLE_GUEST (1) | OR(assigned role bit values).
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

/// Returns a new session_uid (UUID).
pub fn create_session(
    conn: &Connection,
    user_id: i64,
    role_bits: u32,
    user_agent: Option<&str>,
) -> Result<String> {
    let session_uid = public_id("sess");
    let now = now_timestamp();
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

/// Creates an API token. `token_hash` is SHA3-256 hex of the raw token.
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

pub fn ensure_default_user(conn: &Connection) -> Result<i64> {
    if let Some(id) = conn
        .query_row(
            "SELECT id FROM users WHERE username = ?1",
            [DEFAULT_USERNAME],
            |row| row.get(0),
        )
        .optional()?
    {
        return Ok(id);
    }

    conn.execute(
        "INSERT INTO users (
            user_uid, username, email, password_hash, status, role, created_at, last_login_at
        ) VALUES (?1, ?2, NULL, ?3, 'active', 'admin', ?4, NULL)",
        params![
            public_id("usr"),
            DEFAULT_USERNAME,
            "disabled-local-password",
            now_timestamp()
        ],
    )?;

    Ok(conn.last_insert_rowid())
}

pub fn create_archive_run(
    conn: &Connection,
    created_by_user_id: i64,
    requested_count: i64,
) -> Result<ArchiveRun> {
    let run_uid = public_id("run");
    conn.execute(
        "INSERT INTO archive_runs (
            run_uid, created_by_user_id, started_at, status, requested_count
        ) VALUES (?1, ?2, ?3, 'in_progress', ?4)",
        params![
            run_uid,
            created_by_user_id,
            now_timestamp(),
            requested_count
        ],
    )?;

    Ok(ArchiveRun {
        id: conn.last_insert_rowid(),
        run_uid,
    })
}

pub fn create_archive_run_item(
    conn: &Connection,
    run_id: i64,
    parent_item_id: Option<i64>,
    ordinal: i64,
    requested_locator: &str,
    canonical_locator: Option<&str>,
    source_kind: &str,
    entity_kind: &str,
) -> Result<ArchiveRunItem> {
    let item_uid = public_id("item");
    conn.execute(
        "INSERT INTO archive_run_items (
            run_id, item_uid, parent_item_id, ordinal, requested_locator, canonical_locator,
            source_kind, entity_kind, status
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'in_progress')",
        params![
            run_id,
            item_uid,
            parent_item_id,
            ordinal,
            requested_locator,
            canonical_locator,
            source_kind,
            entity_kind
        ],
    )?;

    Ok(ArchiveRunItem {
        id: conn.last_insert_rowid(),
        item_uid,
    })
}

pub fn complete_archive_run_item(
    conn: &Connection,
    item_id: i64,
    produced_entry_id: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE archive_run_items
         SET status = 'completed', produced_entry_id = ?1, error_text = NULL
         WHERE id = ?2",
        params![produced_entry_id, item_id],
    )?;
    refresh_run_counters(conn, run_id_for_item(conn, item_id)?)?;
    Ok(())
}

pub fn fail_archive_run_item(conn: &Connection, item_id: i64, error_text: &str) -> Result<()> {
    conn.execute(
        "UPDATE archive_run_items
         SET status = 'failed', error_text = ?1
         WHERE id = ?2",
        params![error_text, item_id],
    )?;
    refresh_run_counters(conn, run_id_for_item(conn, item_id)?)?;
    Ok(())
}

pub fn finish_archive_run(conn: &Connection, run_id: i64) -> Result<()> {
    refresh_run_counters(conn, run_id)?;
    let failed_count: i64 = conn.query_row(
        "SELECT failed_count FROM archive_runs WHERE id = ?1",
        [run_id],
        |row| row.get(0),
    )?;
    let status = if failed_count > 0 {
        "failed"
    } else {
        "completed"
    };
    conn.execute(
        "UPDATE archive_runs SET status = ?1, finished_at = ?2 WHERE id = ?3",
        params![status, now_timestamp(), run_id],
    )?;
    Ok(())
}

pub fn fail_archive_run(conn: &Connection, run_id: i64, error_summary: &str) -> Result<()> {
    refresh_run_counters(conn, run_id)?;
    conn.execute(
        "UPDATE archive_runs
         SET status = 'failed', finished_at = ?1, error_summary = ?2
         WHERE id = ?3",
        params![now_timestamp(), error_summary, run_id],
    )?;
    Ok(())
}

pub fn upsert_source_identity(
    conn: &Connection,
    source_kind: &str,
    entity_kind: &str,
    external_id: Option<&str>,
    canonical_url: Option<&str>,
    normalized_locator: &str,
) -> Result<i64> {
    let identity_key = identity_key(
        source_kind,
        entity_kind,
        external_id,
        canonical_url,
        normalized_locator,
    );
    conn.execute(
        "INSERT OR IGNORE INTO source_identities (
            source_kind, entity_kind, external_id, canonical_url, normalized_locator, identity_key
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            source_kind,
            entity_kind,
            external_id,
            canonical_url,
            normalized_locator,
            identity_key
        ],
    )?;

    let id = conn.query_row(
        "SELECT id FROM source_identities WHERE identity_key = ?1",
        [identity_key],
        |row| row.get(0),
    )?;
    Ok(id)
}

pub fn upsert_blob(conn: &Connection, blob: &BlobRecord) -> Result<i64> {
    conn.execute(
        "INSERT OR IGNORE INTO blobs (
            sha256, byte_size, mime_type, extension, raw_relpath, created_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            blob.sha256,
            blob.byte_size,
            blob.mime_type,
            blob.extension,
            blob.raw_relpath,
            now_timestamp()
        ],
    )?;

    let id = conn.query_row(
        "SELECT id FROM blobs WHERE sha256 = ?1",
        [blob.sha256.as_str()],
        |row| row.get(0),
    )?;
    Ok(id)
}

/// Returns the `BlobRecord` for the given SHA-256 hex digest, or `None` if not found.
pub fn get_blob_by_sha256(conn: &Connection, sha256: &str) -> Result<Option<BlobRecord>> {
    conn.query_row(
        "SELECT sha256, byte_size, mime_type, extension, raw_relpath
         FROM blobs WHERE sha256 = ?1",
        [sha256],
        |row| {
            Ok(BlobRecord {
                sha256: row.get(0)?,
                byte_size: row.get(1)?,
                mime_type: row.get(2)?,
                extension: row.get(3)?,
                raw_relpath: row.get(4)?,
            })
        },
    )
    .optional()
    .map_err(anyhow::Error::from)
}

pub fn create_archived_entry(conn: &Connection, entry: &NewEntry) -> Result<ArchivedEntry> {
    validate_visibility(&entry.visibility)?;
    let entry_uid = public_id("entry");
    let structured_root_relpath = format!("structured/{entry_uid}");

    conn.execute(
        "INSERT INTO archived_entries (
            entry_uid, source_identity_id, archive_run_id, parent_entry_id, root_entry_id,
            created_by_user_id, owned_by_user_id, source_kind, entity_kind, title, visibility,
            archived_at, original_published_at, structured_root_relpath, representation_kind,
            source_metadata_json, display_metadata_json
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
            ?12, NULL, ?13, ?14, ?15, ?16
        )",
        params![
            entry_uid,
            entry.source_identity_id,
            entry.archive_run_id,
            entry.parent_entry_id,
            entry.root_entry_id,
            entry.created_by_user_id,
            entry.owned_by_user_id,
            entry.source_kind,
            entry.entity_kind,
            entry.title,
            entry.visibility,
            now_timestamp(),
            structured_root_relpath,
            entry.representation_kind,
            entry.source_metadata_json,
            entry.display_metadata_json
        ],
    )?;
    let id = conn.last_insert_rowid();

    if entry.root_entry_id.is_none() {
        conn.execute(
            "UPDATE archived_entries SET root_entry_id = ?1 WHERE id = ?1",
            [id],
        )?;
    }

    Ok(ArchivedEntry {
        id,
        entry_uid,
        structured_root_relpath,
    })
}

pub fn add_entry_artifact(conn: &Connection, artifact: &NewArtifact) -> Result<i64> {
    conn.execute(
        "INSERT INTO entry_artifacts (
            entry_id, artifact_role, storage_area, relpath, blob_id, logical_path, metadata_json
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            artifact.entry_id,
            artifact.artifact_role,
            artifact.storage_area,
            artifact.relpath,
            artifact.blob_id,
            artifact.logical_path,
            artifact.metadata_json
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn remove_entry_tag_assignment(
    conn: &Connection,
    entry_id: i64,
    tag_id: i64,
) -> Result<()> {
    conn.execute(
        "DELETE FROM entry_tag_assignments WHERE entry_id = ?1 AND tag_id = ?2",
        params![entry_id, tag_id],
    )?;
    Ok(())
}

pub fn list_all_tags(conn: &Connection) -> Result<Vec<TagRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, tag_uid, parent_tag_id, name, slug, full_path
         FROM tags
         ORDER BY full_path",
    )?;
    let records = stmt
        .query_map([], |row| {
            Ok(TagRecord {
                id: row.get(0)?,
                tag_uid: row.get(1)?,
                parent_tag_id: row.get(2)?,
                name: row.get(3)?,
                slug: row.get(4)?,
                full_path: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .context("failed to list tags")?;
    Ok(records)
}

pub fn list_tags_for_entry(conn: &Connection, entry_id: i64) -> Result<Vec<TagRecord>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.tag_uid, t.parent_tag_id, t.name, t.slug, t.full_path
         FROM tags t
         JOIN entry_tag_assignments eta ON eta.tag_id = t.id
         WHERE eta.entry_id = ?1
         ORDER BY t.full_path",
    )?;
    let records = stmt
        .query_map([entry_id], |row| {
            Ok(TagRecord {
                id: row.get(0)?,
                tag_uid: row.get(1)?,
                parent_tag_id: row.get(2)?,
                name: row.get(3)?,
                slug: row.get(4)?,
                full_path: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
        .context("failed to list tags for entry")?;
    Ok(records)
}

pub fn get_tag_by_uid(conn: &Connection, tag_uid: &str) -> Result<Option<TagRecord>> {
    conn.query_row(
        "SELECT id, tag_uid, parent_tag_id, name, slug, full_path
         FROM tags WHERE tag_uid = ?1",
        [tag_uid],
        |row| {
            Ok(TagRecord {
                id: row.get(0)?,
                tag_uid: row.get(1)?,
                parent_tag_id: row.get(2)?,
                name: row.get(3)?,
                slug: row.get(4)?,
                full_path: row.get(5)?,
            })
        },
    )
    .optional()
    .context("failed to get tag by uid")
}

pub fn get_tag_by_path(conn: &Connection, full_path: &str) -> Result<Option<TagRecord>> {
    conn.query_row(
        "SELECT id, tag_uid, parent_tag_id, name, slug, full_path
         FROM tags WHERE full_path = ?1",
        [full_path],
        |row| {
            Ok(TagRecord {
                id: row.get(0)?,
                tag_uid: row.get(1)?,
                parent_tag_id: row.get(2)?,
                name: row.get(3)?,
                slug: row.get(4)?,
                full_path: row.get(5)?,
            })
        },
    )
    .optional()
    .context("failed to get tag by path")
}

#[cfg(test)]
pub fn set_public_settings(
    conn: &Connection,
    public_index_enabled: bool,
    public_entry_content_enabled: bool,
    public_archive_submission_enabled: bool,
) -> Result<()> {
    conn.execute(
        "UPDATE instance_settings
         SET public_index_enabled = ?1,
             public_entry_content_enabled = ?2,
             public_archive_submission_enabled = ?3
         WHERE id = 1",
        params![
            public_index_enabled as i64,
            public_entry_content_enabled as i64,
            public_archive_submission_enabled as i64
        ],
    )?;
    Ok(())
}

#[cfg(test)]
pub fn public_index_entry_count(conn: &Connection) -> Result<i64> {
    let count = conn.query_row(
        "SELECT COUNT(*)
         FROM archived_entries
         WHERE parent_entry_id IS NULL
           AND visibility = 'public'
           AND (SELECT public_index_enabled FROM instance_settings WHERE id = 1) = 1
           AND (SELECT public_entry_content_enabled FROM instance_settings WHERE id = 1) = 1",
        [],
        |row| row.get(0),
    )?;
    Ok(count)
}

#[cfg(test)]
pub fn main_archive_entry_count(conn: &Connection) -> Result<i64> {
    let count = conn.query_row(
        "SELECT COUNT(*) FROM archived_entries WHERE parent_entry_id IS NULL",
        [],
        |row| row.get(0),
    )?;
    Ok(count)
}

pub fn create_tag_path(conn: &Connection, full_path: &str) -> Result<i64> {
    let segments = normalized_tag_segments(full_path)?;
    let mut parent_tag_id = None;
    let mut current_path = String::new();
    let mut current_id = 0;

    for segment in segments {
        current_path.push('/');
        current_path.push_str(segment);

        if let Some(id) = conn
            .query_row(
                "SELECT id FROM tags WHERE full_path = ?1",
                [current_path.as_str()],
                |row| row.get(0),
            )
            .optional()?
        {
            current_id = id;
            parent_tag_id = Some(id);
            continue;
        }

        conn.execute(
            "INSERT INTO tags (tag_uid, parent_tag_id, name, slug, full_path)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                public_id("tag"),
                parent_tag_id,
                humanize_slug(segment),
                segment,
                current_path
            ],
        )?;
        current_id = conn.last_insert_rowid();
        parent_tag_id = Some(current_id);
    }

    Ok(current_id)
}

pub fn assign_entry_to_tag(conn: &Connection, entry_id: i64, tag_id: i64) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO entry_tag_assignments (entry_id, tag_id)
         VALUES (?1, ?2)",
        params![entry_id, tag_id],
    )?;
    Ok(())
}

pub fn entry_count_for_tag_path(conn: &Connection, full_path: &str) -> Result<i64> {
    let count = conn.query_row(
        "WITH RECURSIVE descendants(id) AS (
            SELECT id FROM tags WHERE full_path = ?1
            UNION ALL
            SELECT child.id
            FROM tags child
            JOIN descendants parent ON child.parent_tag_id = parent.id
         )
         SELECT COUNT(DISTINCT eta.entry_id)
         FROM entry_tag_assignments eta
         JOIN descendants d ON eta.tag_id = d.id",
        [full_path],
        |row| row.get(0),
    )?;
    Ok(count)
}

fn refresh_run_counters(conn: &Connection, run_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE archive_runs
         SET discovered_count = (SELECT COUNT(*) FROM archive_run_items WHERE run_id = ?1),
             completed_count = (SELECT COUNT(*) FROM archive_run_items WHERE run_id = ?1 AND status = 'completed'),
             failed_count = (SELECT COUNT(*) FROM archive_run_items WHERE run_id = ?1 AND status = 'failed')
         WHERE id = ?1",
        [run_id],
    )?;
    Ok(())
}

fn run_id_for_item(conn: &Connection, item_id: i64) -> Result<i64> {
    let run_id = conn.query_row(
        "SELECT run_id FROM archive_run_items WHERE id = ?1",
        [item_id],
        |row| row.get(0),
    )?;
    Ok(run_id)
}

fn public_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4().simple())
}

fn now_timestamp() -> String {
    Utc::now().to_rfc3339()
}

fn identity_key(
    source_kind: &str,
    entity_kind: &str,
    external_id: Option<&str>,
    canonical_url: Option<&str>,
    normalized_locator: &str,
) -> String {
    let stable_locator = external_id.or(canonical_url).unwrap_or(normalized_locator);
    format!("{source_kind}:{entity_kind}:{stable_locator}")
}

fn validate_visibility(visibility: &str) -> Result<()> {
    match visibility {
        "private" | "unlisted" | "public" => Ok(()),
        _ => bail!("invalid archived entry visibility: {visibility}"),
    }
}

fn normalized_tag_segments(full_path: &str) -> Result<Vec<&str>> {
    let segments = full_path
        .trim()
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();

    if segments.is_empty() {
        bail!("tag path must contain at least one segment");
    }

    Ok(segments)
}

fn humanize_slug(slug: &str) -> String {
    slug.split('-')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        env, fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        initialize_schema(&conn).unwrap();
        conn
    }

    fn unique_db_path(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!("{prefix}-{nanos}-{}.sqlite", std::process::id()))
    }

    fn create_entry_fixture(
        conn: &Connection,
        visibility: &str,
        parent_entry_id: Option<i64>,
        root_entry_id: Option<i64>,
    ) -> ArchivedEntry {
        let user_id = ensure_default_user(conn).unwrap();
        let run = create_archive_run(conn, user_id, 1).unwrap();
        let source_id = upsert_source_identity(
            conn,
            "youtube",
            "video",
            Some("video-1"),
            Some("https://youtube.com/watch?v=video-1"),
            "https://youtube.com/watch?v=video-1",
        )
        .unwrap();

        create_archived_entry(
            conn,
            &NewEntry {
                source_identity_id: source_id,
                archive_run_id: run.id,
                parent_entry_id,
                root_entry_id,
                created_by_user_id: user_id,
                owned_by_user_id: user_id,
                source_kind: "youtube".to_string(),
                entity_kind: "video".to_string(),
                title: None,
                visibility: visibility.to_string(),
                representation_kind: "video".to_string(),
                source_metadata_json: "{}".to_string(),
                display_metadata_json: None,
            },
        )
        .unwrap()
    }

    #[test]
    fn schema_defaults_public_settings_to_private() {
        let conn = conn();
        let defaults: (i64, i64, i64) = conn
            .query_row(
                "SELECT public_index_enabled, public_entry_content_enabled, public_archive_submission_enabled
                 FROM instance_settings WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();

        assert_eq!(defaults, (0, 0, 0));
    }

    #[test]
    fn file_database_uses_wal_journal_mode() {
        let path = unique_db_path("archivr-wal-test");
        let conn = Connection::open(&path).unwrap();
        initialize_schema(&conn).unwrap();

        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();

        assert_eq!(journal_mode, "wal");

        drop(conn);
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(path.with_extension("sqlite-wal"));
        let _ = fs::remove_file(path.with_extension("sqlite-shm"));
    }

    #[test]
    fn root_entry_sets_root_id_after_insert() {
        let conn = conn();
        let entry = create_entry_fixture(&conn, "private", None, None);
        let root_entry_id: i64 = conn
            .query_row(
                "SELECT root_entry_id FROM archived_entries WHERE id = ?1",
                [entry.id],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(root_entry_id, entry.id);
    }

    #[test]
    fn rearchiving_reuses_source_identity_and_blob_but_creates_entries() {
        let conn = conn();
        let user_id = ensure_default_user(&conn).unwrap();
        let blob = BlobRecord {
            sha256: "abc123".to_string(),
            byte_size: 123,
            mime_type: Some("video/mp4".to_string()),
            extension: Some("mp4".to_string()),
            raw_relpath: "raw/a/b/abc123.mp4".to_string(),
        };
        let blob_id = upsert_blob(&conn, &blob).unwrap();
        let duplicate_blob_id = upsert_blob(&conn, &blob).unwrap();
        assert_eq!(blob_id, duplicate_blob_id);

        let first_source_id = upsert_source_identity(
            &conn,
            "youtube",
            "video",
            Some("video-1"),
            Some("https://youtube.com/watch?v=video-1"),
            "https://youtube.com/watch?v=video-1",
        )
        .unwrap();
        let second_source_id = upsert_source_identity(
            &conn,
            "youtube",
            "video",
            Some("video-1"),
            Some("https://youtube.com/watch?v=video-1"),
            "https://youtube.com/watch?v=video-1",
        )
        .unwrap();
        assert_eq!(first_source_id, second_source_id);

        for _ in 0..2 {
            let run = create_archive_run(&conn, user_id, 1).unwrap();
            let entry = create_archived_entry(
                &conn,
                &NewEntry {
                    source_identity_id: first_source_id,
                    archive_run_id: run.id,
                    parent_entry_id: None,
                    root_entry_id: None,
                    created_by_user_id: user_id,
                    owned_by_user_id: user_id,
                    source_kind: "youtube".to_string(),
                    entity_kind: "video".to_string(),
                    title: None,
                    visibility: "private".to_string(),
                    representation_kind: "video".to_string(),
                    source_metadata_json: "{}".to_string(),
                    display_metadata_json: None,
                },
            )
            .unwrap();
            add_entry_artifact(
                &conn,
                &NewArtifact {
                    entry_id: entry.id,
                    artifact_role: "primary_media".to_string(),
                    storage_area: "raw".to_string(),
                    relpath: blob.raw_relpath.clone(),
                    blob_id: Some(blob_id),
                    logical_path: None,
                    metadata_json: None,
                },
            )
            .unwrap();
        }

        let entry_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM archived_entries", [], |row| {
                row.get(0)
            })
            .unwrap();
        let source_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM source_identities", [], |row| {
                row.get(0)
            })
            .unwrap();
        let blob_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get(0))
            .unwrap();

        assert_eq!(entry_count, 2);
        assert_eq!(source_count, 1);
        assert_eq!(blob_count, 1);
    }

    #[test]
    fn source_identity_key_prefers_external_id_over_shared_canonical_url() {
        let conn = conn();
        let first_source_id = upsert_source_identity(
            &conn,
            "x",
            "tweet",
            Some("tweet-1"),
            Some("https://x.com/some-profile"),
            "https://x.com/some-profile/status/tweet-1",
        )
        .unwrap();
        let second_source_id = upsert_source_identity(
            &conn,
            "x",
            "tweet",
            Some("tweet-2"),
            Some("https://x.com/some-profile"),
            "https://x.com/some-profile/status/tweet-2",
        )
        .unwrap();

        assert_ne!(first_source_id, second_source_id);
    }

    #[test]
    fn run_items_refresh_progress_counters() {
        let conn = conn();
        let user_id = ensure_default_user(&conn).unwrap();
        let run = create_archive_run(&conn, user_id, 2).unwrap();
        let source_id =
            upsert_source_identity(&conn, "local", "file", None, None, "file:///a").unwrap();
        let entry = create_archived_entry(
            &conn,
            &NewEntry {
                source_identity_id: source_id,
                archive_run_id: run.id,
                parent_entry_id: None,
                root_entry_id: None,
                created_by_user_id: user_id,
                owned_by_user_id: user_id,
                source_kind: "local".to_string(),
                entity_kind: "file".to_string(),
                title: None,
                visibility: "private".to_string(),
                representation_kind: "file".to_string(),
                source_metadata_json: "{}".to_string(),
                display_metadata_json: None,
            },
        )
        .unwrap();
        let first =
            create_archive_run_item(&conn, run.id, None, 0, "file:///a", None, "local", "file")
                .unwrap();
        let second =
            create_archive_run_item(&conn, run.id, None, 1, "file:///b", None, "local", "file")
                .unwrap();

        complete_archive_run_item(&conn, first.id, entry.id).unwrap();
        fail_archive_run_item(&conn, second.id, "copy failed").unwrap();
        finish_archive_run(&conn, run.id).unwrap();

        let counters: (i64, i64, i64, String) = conn
            .query_row(
                "SELECT discovered_count, completed_count, failed_count, status
                 FROM archive_runs WHERE id = ?1",
                [run.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();

        assert_eq!(counters, (2, 1, 1, "failed".to_string()));
    }

    #[test]
    fn main_archive_query_only_counts_roots() {
        let conn = conn();
        let parent = create_entry_fixture(&conn, "private", None, None);
        let _child = create_entry_fixture(&conn, "private", Some(parent.id), Some(parent.id));

        assert_eq!(main_archive_entry_count(&conn).unwrap(), 1);
    }

    #[test]
    fn public_entries_require_instance_flags_and_public_visibility() {
        let conn = conn();
        let _public = create_entry_fixture(&conn, "public", None, None);
        let _private = create_entry_fixture(&conn, "private", None, None);

        assert_eq!(public_index_entry_count(&conn).unwrap(), 0);

        set_public_settings(&conn, true, false, false).unwrap();
        assert_eq!(public_index_entry_count(&conn).unwrap(), 0);

        set_public_settings(&conn, true, true, false).unwrap();
        assert_eq!(public_index_entry_count(&conn).unwrap(), 1);
    }

    #[test]
    fn hierarchical_tag_assignments_are_discoverable_through_ancestors() {
        let conn = conn();
        let entry = create_entry_fixture(&conn, "private", None, None);
        let tag_id = create_tag_path(&conn, "/sciences/computer-science/compilers").unwrap();
        assign_entry_to_tag(&conn, entry.id, tag_id).unwrap();

        assert_eq!(
            entry_count_for_tag_path(&conn, "/sciences/computer-science/compilers").unwrap(),
            1
        );
        assert_eq!(
            entry_count_for_tag_path(&conn, "/sciences/computer-science").unwrap(),
            1
        );
        assert_eq!(entry_count_for_tag_path(&conn, "/sciences").unwrap(), 1);
    }

    #[test]
    fn get_blob_by_sha256_round_trips() {
        let conn = conn();
        let blob = BlobRecord {
            sha256: "deadbeef01234567".repeat(4), // 64-char hex string
            byte_size: 1234,
            mime_type: Some("font/woff2".to_string()),
            extension: Some("woff2".to_string()),
            raw_relpath: "raw/d/e/deadbeef.woff2".to_string(),
        };
        upsert_blob(&conn, &blob).unwrap();

        let found = get_blob_by_sha256(&conn, &blob.sha256).unwrap();
        assert!(found.is_some(), "should find the blob we just upserted");
        let found = found.unwrap();
        assert_eq!(found.sha256, blob.sha256);
        assert_eq!(found.byte_size, 1234);
        assert_eq!(found.mime_type, Some("font/woff2".to_string()));
        assert_eq!(found.raw_relpath, blob.raw_relpath);
    }

    #[test]
    fn get_blob_by_sha256_returns_none_for_unknown() {
        let conn = conn();
        let result = get_blob_by_sha256(&conn, "0000000000000000000000000000000000000000000000000000000000000000").unwrap();
        assert!(result.is_none());
    }

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
        initialize_auth_schema(&conn).unwrap();
    }

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
        assert_eq!(bits, 15u32);
    }

    #[test]
    fn get_user_by_username_returns_none_for_unknown() {
        let conn = make_auth_conn();
        assert!(get_user_by_username(&conn, "nobody").unwrap().is_none());
    }

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
}
