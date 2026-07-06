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

#[derive(Debug, Clone, serde::Serialize)]
pub struct CaptureJobRecord {
    pub job_uid: String,
    pub archive_id: String,
    pub run_uid: Option<String>,
    pub status: String,
    pub error_text: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct UserSummary {
    pub user_uid: String,
    pub username: String,
    pub email: Option<String>,
    pub status: String,
    pub created_at: String,
    pub role_slugs: Vec<String>,
    pub role_bits: u32,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RoleRecord {
    pub role_uid: String,
    pub slug: String,
    pub name: String,
    pub level: i64,
    pub bit_position: i64,
    pub is_builtin: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InstanceSettings {
    pub public_index_enabled: bool,
    pub public_entry_content_enabled: bool,
    pub open_registration_enabled: bool,  // maps to public_archive_submission_enabled column
    pub default_entry_visibility: u32,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CollectionRecord {
    pub id: i64,
    pub collection_uid: String,
    pub name: String,
    pub slug: String,
    pub default_visibility_bits: u32,
    pub created_at: String,
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
            display_metadata_json TEXT,
            cached_bytes INTEGER NOT NULL DEFAULT 0
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

        CREATE TABLE IF NOT EXISTS capture_jobs (
            id          INTEGER PRIMARY KEY,
            job_uid     TEXT NOT NULL UNIQUE,
            archive_id  TEXT NOT NULL,
            run_uid     TEXT,
            status      TEXT NOT NULL CHECK (status IN ('pending','running','completed','failed')) DEFAULT 'pending',
            error_text  TEXT,
            created_at  TEXT NOT NULL,
            updated_at  TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_capture_jobs_status ON capture_jobs(status);
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

        CREATE TABLE IF NOT EXISTS collections (
            id                      INTEGER PRIMARY KEY,
            collection_uid          TEXT NOT NULL UNIQUE,
            name                    TEXT NOT NULL,
            slug                    TEXT NOT NULL UNIQUE,
            default_visibility_bits INTEGER NOT NULL DEFAULT 2,
            created_at              TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS collection_entries (
            collection_id   INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
            entry_id        INTEGER NOT NULL REFERENCES archived_entries(id) ON DELETE CASCADE,
            visibility_bits INTEGER NOT NULL DEFAULT 2,
            added_at        TEXT NOT NULL,
            PRIMARY KEY (collection_id, entry_id)
        );

        CREATE INDEX IF NOT EXISTS idx_collection_entries_entry_id ON collection_entries(entry_id);
        CREATE INDEX IF NOT EXISTS idx_collection_entries_collection_id ON collection_entries(collection_id);

        -- Seed default collection (idempotent)
        INSERT OR IGNORE INTO collections (collection_uid, name, slug, default_visibility_bits, created_at)
        VALUES ('coll_default', 'All Entries', '_default_', 2, datetime('now'));

        -- Migrate existing entries to default collection (idempotent)
        INSERT OR IGNORE INTO collection_entries (collection_id, entry_id, visibility_bits, added_at)
        SELECT
            (SELECT id FROM collections WHERE slug = '_default_'),
            ae.id,
            CASE ae.visibility
                WHEN 'public'   THEN 3
                WHEN 'unlisted' THEN 2
                ELSE            0
            END,
            ae.archived_at
        FROM archived_entries ae;
        "#,
    )?;

    // Migration: add cached_bytes column to existing databases.
    // New databases already have it from the DDL above; the column check is
    // the idiomatic SQLite way to run a migration exactly once.
    let column_exists: bool = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('archived_entries') WHERE name = 'cached_bytes'",
        [],
        |row| row.get::<_, i64>(0),
    )? > 0;
    if !column_exists {
        conn.execute_batch(
            "ALTER TABLE archived_entries ADD COLUMN cached_bytes INTEGER NOT NULL DEFAULT 0;
             UPDATE archived_entries
             SET cached_bytes = (
                 SELECT COALESCE(SUM(b.byte_size), 0)
                 FROM entry_artifacts ea
                 JOIN blobs b ON b.id = ea.blob_id
                 WHERE ea.entry_id = archived_entries.id
                   AND ea.blob_id IS NOT NULL
                   AND EXISTS (
                       SELECT 1
                       FROM entry_artifacts ea2
                       JOIN archived_entries e2 ON e2.id = ea2.entry_id
                       WHERE ea2.blob_id = ea.blob_id
                         AND (e2.archived_at < archived_entries.archived_at
                              OR (e2.archived_at = archived_entries.archived_at
                                  AND e2.id < archived_entries.id))
                   )
             );",
        )?;
    }

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
            display_name  TEXT,
            status        TEXT NOT NULL CHECK (status IN ('active', 'disabled')),
            role          TEXT NOT NULL CHECK (role IN ('admin', 'user')),
            created_at    TEXT NOT NULL,
            last_login_at TEXT
        );
        "#,
    )?;
    // Add display_name column to users if not present (idempotent migration)
    let _ = conn.execute("ALTER TABLE users ADD COLUMN display_name TEXT", []);
    // Add humanize_slugs column to users if not present (idempotent migration)
    let _ = conn.execute(
        "ALTER TABLE users ADD COLUMN humanize_slugs INTEGER NOT NULL DEFAULT 0",
        [],
    );

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
        .to_rfc3339();
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
        .to_rfc3339();
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

pub fn get_instance_settings(conn: &Connection) -> Result<InstanceSettings> {
    conn.query_row(
        "SELECT public_index_enabled, public_entry_content_enabled,
                public_archive_submission_enabled, default_entry_visibility
         FROM instance_settings WHERE id = 1",
        [],
        |row| {
            Ok(InstanceSettings {
                public_index_enabled: row.get::<_, i64>(0)? != 0,
                public_entry_content_enabled: row.get::<_, i64>(1)? != 0,
                open_registration_enabled: row.get::<_, i64>(2)? != 0,
                default_entry_visibility: row.get::<_, i64>(3)? as u32,
            })
        },
    )
    .map_err(Into::into)
}

pub fn update_instance_settings(conn: &Connection, settings: &InstanceSettings) -> Result<()> {
    conn.execute(
        "UPDATE instance_settings
         SET public_index_enabled = ?1,
             public_entry_content_enabled = ?2,
             public_archive_submission_enabled = ?3,
             default_entry_visibility = ?4
         WHERE id = 1",
        params![
            settings.public_index_enabled as i64,
            settings.public_entry_content_enabled as i64,
            settings.open_registration_enabled as i64,
            settings.default_entry_visibility as i64,
        ],
    )?;
    Ok(())
}

pub fn get_user_password_hash(conn: &Connection, user_id: i64) -> Result<Option<String>> {
    conn.query_row(
        "SELECT password_hash FROM users WHERE id = ?1",
        [user_id],
        |r| r.get(0),
    )
    .optional()
    .map_err(Into::into)
}

pub fn update_user_password(conn: &Connection, user_id: i64, new_hash: &str) -> Result<()> {
    conn.execute(
        "UPDATE users SET password_hash = ?1 WHERE id = ?2",
        params![new_hash, user_id],
    )?;
    Ok(())
}

pub fn update_user_display_name(conn: &Connection, user_id: i64, display_name: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE users SET display_name = ?1 WHERE id = ?2",
        params![display_name, user_id],
    )?;
    Ok(())
}

pub fn update_user_humanize_slugs(conn: &Connection, user_id: i64, value: bool) -> Result<()> {
    conn.execute(
        "UPDATE users SET humanize_slugs = ?1 WHERE id = ?2",
        params![value as i64, user_id],
    )?;
    Ok(())
}

/// Updates the user-visible title of an archived entry.
/// Returns `Ok(true)` if a row was updated, `Ok(false)` if the entry_uid was not found.
pub fn update_entry_title(conn: &Connection, entry_uid: &str, title: Option<&str>) -> Result<bool> {
    let n = conn.execute(
        "UPDATE archived_entries SET title = ?1 WHERE entry_uid = ?2",
        params![title, entry_uid],
    )?;
    Ok(n > 0)
}

pub fn get_user_display_name(conn: &Connection, user_id: i64) -> Result<Option<String>> {
    conn.query_row(
        "SELECT display_name FROM users WHERE id = ?1",
        [user_id],
        |r| r.get(0),
    )
    .optional()
    .map_err(Into::into)
}

/// Deletes all sessions for a user. Called on ban or role change.
pub fn invalidate_user_sessions(conn: &Connection, user_id: i64) -> Result<usize> {
    let n = conn.execute("DELETE FROM sessions WHERE user_id = ?1", [user_id])?;
    Ok(n)
}

/// Returns the integer id for a user_uid, or None if not found.
pub fn get_user_id_by_uid(conn: &Connection, user_uid: &str) -> Result<Option<i64>> {
    conn.query_row("SELECT id FROM users WHERE user_uid = ?1", [user_uid], |r| r.get(0))
        .optional()
        .map_err(Into::into)
}

/// Lists all users with their assigned roles and computed role_bits.
pub fn list_users(conn: &Connection) -> Result<Vec<UserSummary>> {
    let mut stmt = conn.prepare(
        "SELECT id, user_uid, username, email, status, created_at FROM users ORDER BY created_at ASC",
    )?;
    let rows: Vec<(i64, String, String, Option<String>, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?
        .collect::<Result<_, _>>()?;

    rows.into_iter()
        .map(|(id, user_uid, username, email, status, created_at)| {
            let role_bits = compute_role_bits(conn, id)?;
            let mut rs = conn.prepare(
                "SELECT r.slug FROM user_roles ur JOIN roles r ON r.id = ur.role_id
                 WHERE ur.user_id = ?1 ORDER BY r.level, r.bit_position",
            )?;
            let role_slugs: Vec<String> =
                rs.query_map([id], |r| r.get(0))?.collect::<Result<_, _>>()?;
            Ok(UserSummary { user_uid, username, email, status, created_at, role_slugs, role_bits })
        })
        .collect()
}

/// Gets a single user by user_uid with roles and role_bits.
pub fn get_user_by_uid(conn: &Connection, user_uid: &str) -> Result<Option<UserSummary>> {
    let row = conn
        .query_row(
            "SELECT id, user_uid, username, email, status, created_at FROM users WHERE user_uid = ?1",
            [user_uid],
            |r| Ok((r.get::<_, i64>(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .optional()?;
    match row {
        None => Ok(None),
        Some((id, user_uid, username, email, status, created_at)) => {
            let role_bits = compute_role_bits(conn, id)?;
            let mut rs = conn.prepare(
                "SELECT r.slug FROM user_roles ur JOIN roles r ON r.id = ur.role_id
                 WHERE ur.user_id = ?1 ORDER BY r.level, r.bit_position",
            )?;
            let role_slugs: Vec<String> =
                rs.query_map([id], |r| r.get(0))?.collect::<Result<_, _>>()?;
            Ok(Some(UserSummary { user_uid, username, email, status, created_at, role_slugs, role_bits }))
        }
    }
}

/// Creates a new user (admin-created account) and assigns the 'user' role.
/// Returns the new user_uid.
pub fn create_user(
    conn: &Connection,
    username: &str,
    email: Option<&str>,
    password_hash: &str,
    created_by_user_id: i64,
) -> Result<String> {
    let user_uid = public_id("usr");
    conn.execute(
        "INSERT INTO users (user_uid, username, email, password_hash, status, role, created_at)
         VALUES (?1, ?2, ?3, ?4, 'active', 'user', ?5)",
        params![user_uid, username, email, password_hash, now_timestamp()],
    )?;
    let user_id = conn.last_insert_rowid();
    let role_id: i64 = conn.query_row(
        "SELECT id FROM roles WHERE slug = 'user'",
        [],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO user_roles (user_id, role_id, assigned_at, assigned_by_user_id)
         VALUES (?1, ?2, ?3, ?4)",
        params![user_id, role_id, now_timestamp(), created_by_user_id],
    )?;
    Ok(user_uid)
}

/// Sets a user's status ('active' | 'disabled'). Invalidates sessions when disabling.
/// Returns true if the user was found.
pub fn set_user_status(conn: &Connection, user_uid: &str, status: &str) -> Result<bool> {
    if status == "disabled" {
        let id: Option<i64> = conn
            .query_row("SELECT id FROM users WHERE user_uid = ?1", [user_uid], |r| r.get(0))
            .optional()?;
        if let Some(id) = id {
            invalidate_user_sessions(conn, id)?;
        }
    }
    let n = conn.execute(
        "UPDATE users SET status = ?1 WHERE user_uid = ?2",
        params![status, user_uid],
    )?;
    Ok(n > 0)
}

/// Assigns a role to a user (cumulative: also ensures 'user' for any non-guest role,
/// and 'admin' for 'owner'). Invalidates the user's sessions so changes take effect on re-login.
pub fn assign_role(
    conn: &Connection,
    target_user_id: i64,
    role_slug: &str,
    assigned_by_user_id: i64,
) -> Result<()> {
    let role_id: i64 = conn
        .query_row("SELECT id FROM roles WHERE slug = ?1", [role_slug], |r| r.get(0))
        .map_err(|_| anyhow::anyhow!("role '{}' not found", role_slug))?;
    conn.execute(
        "INSERT OR IGNORE INTO user_roles (user_id, role_id, assigned_at, assigned_by_user_id)
         VALUES (?1, ?2, ?3, ?4)",
        params![target_user_id, role_id, now_timestamp(), assigned_by_user_id],
    )?;
    // Cumulative: ensure 'user' whenever any non-guest role is assigned
    if role_slug != "user" && role_slug != "guest" {
        let uid: i64 = conn.query_row("SELECT id FROM roles WHERE slug = 'user'", [], |r| r.get(0))?;
        conn.execute(
            "INSERT OR IGNORE INTO user_roles (user_id, role_id, assigned_at, assigned_by_user_id)
             VALUES (?1, ?2, ?3, ?4)",
            params![target_user_id, uid, now_timestamp(), assigned_by_user_id],
        )?;
    }
    // Also ensure 'admin' when assigning 'owner'
    if role_slug == "owner" {
        let aid: i64 = conn.query_row("SELECT id FROM roles WHERE slug = 'admin'", [], |r| r.get(0))?;
        conn.execute(
            "INSERT OR IGNORE INTO user_roles (user_id, role_id, assigned_at, assigned_by_user_id)
             VALUES (?1, ?2, ?3, ?4)",
            params![target_user_id, aid, now_timestamp(), assigned_by_user_id],
        )?;
    }
    invalidate_user_sessions(conn, target_user_id)?;
    Ok(())
}

/// Removes a role from a user. Guards: can't remove the only owner's 'owner' role.
/// Invalidates the user's sessions.
pub fn remove_role(conn: &Connection, target_user_id: i64, role_slug: &str) -> Result<()> {
    if role_slug == "owner" {
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM user_roles ur JOIN roles r ON r.id = ur.role_id WHERE r.slug = 'owner'",
            [],
            |r| r.get(0),
        )?;
        if count <= 1 {
            anyhow::bail!("cannot remove the last owner");
        }
    }
    let role_id: i64 = conn
        .query_row("SELECT id FROM roles WHERE slug = ?1", [role_slug], |r| r.get(0))
        .map_err(|_| anyhow::anyhow!("role '{}' not found", role_slug))?;
    conn.execute(
        "DELETE FROM user_roles WHERE user_id = ?1 AND role_id = ?2",
        params![target_user_id, role_id],
    )?;
    invalidate_user_sessions(conn, target_user_id)?;
    Ok(())
}

/// Lists all roles ordered by level then bit_position.
pub fn list_roles(conn: &Connection) -> Result<Vec<RoleRecord>> {
    let mut stmt = conn.prepare(
        "SELECT role_uid, slug, name, level, bit_position, is_builtin FROM roles
         ORDER BY level, bit_position",
    )?;
    stmt.query_map([], |r| {
        Ok(RoleRecord {
            role_uid: r.get(0)?,
            slug: r.get(1)?,
            name: r.get(2)?,
            level: r.get(3)?,
            bit_position: r.get(4)?,
            is_builtin: r.get::<_, i64>(5)? != 0,
        })
    })?
    .collect::<Result<_, _>>()
    .map_err(Into::into)
}

/// Creates a new custom role (level=2, bit_position = max existing + 1, min 4).
/// Returns the created RoleRecord.
pub fn create_custom_role(conn: &Connection, slug: &str, name: &str) -> Result<RoleRecord> {
    if slug.is_empty() || !slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        anyhow::bail!("role slug must be non-empty and contain only ASCII letters, digits, or hyphens");
    }
    let next_bit: i64 = conn.query_row(
        "SELECT COALESCE(MAX(bit_position) + 1, 4) FROM roles WHERE bit_position >= 4",
        [],
        |r| r.get(0),
    )?;
    if next_bit >= 32 {
        anyhow::bail!("maximum number of custom roles reached");
    }
    let role_uid = public_id("role");
    conn.execute(
        "INSERT INTO roles (role_uid, slug, name, level, bit_position, is_builtin)
         VALUES (?1, ?2, ?3, 2, ?4, 0)",
        params![role_uid, slug, name, next_bit],
    )?;
    Ok(RoleRecord {
        role_uid,
        slug: slug.to_string(),
        name: name.to_string(),
        level: 2,
        bit_position: next_bit,
        is_builtin: false,
    })
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

/// Creates a pending capture job. Returns the new `job_uid`.
pub fn create_capture_job(conn: &Connection, archive_id: &str) -> Result<String> {
    let job_uid = public_id("job");
    let now = now_timestamp();
    conn.execute(
        "INSERT INTO capture_jobs (job_uid, archive_id, run_uid, status, error_text, created_at, updated_at)
         VALUES (?1, ?2, NULL, 'pending', NULL, ?3, ?3)",
        rusqlite::params![job_uid, archive_id, now],
    )?;
    Ok(job_uid)
}

/// Updates the status (and optionally run_uid / error_text) of a capture job.
pub fn update_capture_job_status(
    conn: &Connection,
    job_uid: &str,
    status: &str,
    run_uid: Option<&str>,
    error_text: Option<&str>,
) -> Result<()> {
    let now = now_timestamp();
    conn.execute(
        "UPDATE capture_jobs SET status = ?1, run_uid = COALESCE(?2, run_uid),
         error_text = ?3, updated_at = ?4 WHERE job_uid = ?5",
        rusqlite::params![status, run_uid, error_text, now, job_uid],
    )?;
    Ok(())
}

/// Returns a capture job by uid.
pub fn get_capture_job(conn: &Connection, job_uid: &str) -> Result<Option<CaptureJobRecord>> {
    conn.query_row(
        "SELECT job_uid, archive_id, run_uid, status, error_text, created_at, updated_at
         FROM capture_jobs WHERE job_uid = ?1",
        [job_uid],
        |row| {
            Ok(CaptureJobRecord {
                job_uid: row.get(0)?,
                archive_id: row.get(1)?,
                run_uid: row.get(2)?,
                status: row.get(3)?,
                error_text: row.get(4)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

/// Marks all interrupted capture jobs, runs, and run items as failed.
/// Called at server startup to recover from a hard shutdown mid-capture.
///
/// `capture_jobs.run_uid` is NULL when the server crashes before `perform_capture`
/// returns, so we cannot join; instead we fail every `archive_runs` row still
/// `in_progress` directly — any run that survived shutdown unfinished was interrupted.
///
/// Returns the number of `capture_jobs` rows updated (used for the startup log).
pub fn fail_stalled_capture_jobs(conn: &Connection) -> Result<usize> {
    let now = now_timestamp();

    // 1. Fail in-progress run items.
    conn.execute(
        "UPDATE archive_run_items
         SET status = 'failed', error_text = 'interrupted by server restart'
         WHERE status = 'in_progress'",
        [],
    )?;

    // 2. Fail in-progress archive runs; recount failed items from the updated rows.
    conn.execute(
        "UPDATE archive_runs
         SET status     = 'failed',
             finished_at = ?1,
             failed_count = (
                 SELECT COUNT(*) FROM archive_run_items
                 WHERE run_id = archive_runs.id AND status = 'failed'
             ),
             error_summary = 'interrupted by server restart'
         WHERE status = 'in_progress'",
        [now.clone()],
    )?;

    // 3. Fail running capture jobs (the polling layer).
    let n = conn.execute(
        "UPDATE capture_jobs SET status = 'failed',
         error_text = 'interrupted by server restart',
         updated_at = ?1
         WHERE status = 'running'",
        [now],
    )?;

    Ok(n)
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

/// Computes and stores `cached_bytes` for a single entry.
///
/// Must be called after all artifacts for the entry have been inserted so the
/// correlated subquery sees the complete artifact set. Ordering by `archived_at`
/// (tiebreak: `id`) matches the display ordering used in listings.
pub fn refresh_entry_cached_bytes(conn: &Connection, entry_id: i64) -> Result<()> {
    let cached: i64 = conn.query_row(
        "SELECT COALESCE(SUM(b.byte_size), 0)
         FROM entry_artifacts ea
         JOIN blobs b ON b.id = ea.blob_id
         JOIN archived_entries e ON e.id = ea.entry_id
         WHERE ea.entry_id = ?1
           AND ea.blob_id IS NOT NULL
           AND EXISTS (
               SELECT 1
               FROM entry_artifacts ea2
               JOIN archived_entries e2 ON e2.id = ea2.entry_id
               WHERE ea2.blob_id = ea.blob_id
                 AND (e2.archived_at < e.archived_at
                      OR (e2.archived_at = e.archived_at AND e2.id < ?1))
           )",
        [entry_id],
        |row| row.get(0),
    )?;
    conn.execute(
        "UPDATE archived_entries SET cached_bytes = ?1 WHERE id = ?2",
        params![cached, entry_id],
    )?;
    Ok(())
}

/// Recomputes `cached_bytes` for entries that shared blobs with `entry_id` and
/// were archived after it.
///
/// Must be called **before** the entry row is deleted so that the shared-blob
/// lookup still works. The inner EXISTS deliberately excludes `entry_id` so each
/// affected entry is recomputed as if that entry no longer exists.
///
/// Intended to be dispatched asynchronously: acknowledge the delete to the user
/// first, then call this on a background thread.
pub fn cascade_cached_bytes_after_delete(conn: &Connection, entry_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE archived_entries
         SET cached_bytes = (
             SELECT COALESCE(SUM(b.byte_size), 0)
             FROM entry_artifacts ea
             JOIN blobs b ON b.id = ea.blob_id
             WHERE ea.entry_id = archived_entries.id
               AND ea.blob_id IS NOT NULL
               AND EXISTS (
                   SELECT 1
                   FROM entry_artifacts ea3
                   JOIN archived_entries e3 ON e3.id = ea3.entry_id
                   WHERE ea3.blob_id = ea.blob_id
                     AND e3.id != ?1
                     AND (e3.archived_at < archived_entries.archived_at
                          OR (e3.archived_at = archived_entries.archived_at
                              AND e3.id < archived_entries.id))
               )
         )
         WHERE id IN (
             SELECT DISTINCT ea2.entry_id
             FROM entry_artifacts ea_del
             JOIN entry_artifacts ea2   ON ea2.blob_id = ea_del.blob_id
             JOIN archived_entries e_del ON e_del.id = ea_del.entry_id
             JOIN archived_entries e2   ON e2.id = ea2.entry_id
             WHERE ea_del.entry_id = ?1
               AND ea2.entry_id != ?1
               AND (e2.archived_at > e_del.archived_at
                    OR (e2.archived_at = e_del.archived_at AND e2.id > ?1))
         )",
        [entry_id],
    )?;
    Ok(())
}

/// Recalculates `cached_bytes` for every entry that shares a blob with any member of
/// `subtree_ids` and was archived after that member, treating the whole subtree as absent.
///
/// Unlike `cascade_cached_bytes_after_delete` (single-entry), this excludes **all** subtree IDs
/// from the EXISTS check in one SQL pass, so sibling entries don't falsely count each other as
/// "still there" during the recalculation.
///
/// Must be called before any subtree rows are deleted so the `entry_artifacts` JOIN resolves.
fn cascade_cached_bytes_after_subtree_delete(
    conn: &Connection,
    subtree_ids: &[i64],
) -> Result<()> {
    if subtree_ids.is_empty() {
        return Ok(());
    }
    // Build ?1,?2,…,?N — positional params can be re-referenced multiple times in one statement.
    let ph: String = (1..=subtree_ids.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "UPDATE archived_entries
         SET cached_bytes = (
             SELECT COALESCE(SUM(b.byte_size), 0)
             FROM entry_artifacts ea
             JOIN blobs b ON b.id = ea.blob_id
             WHERE ea.entry_id = archived_entries.id
               AND ea.blob_id IS NOT NULL
               AND EXISTS (
                   SELECT 1
                   FROM entry_artifacts ea3
                   JOIN archived_entries e3 ON e3.id = ea3.entry_id
                   WHERE ea3.blob_id = ea.blob_id
                     AND e3.id NOT IN ({ph})
                     AND (e3.archived_at < archived_entries.archived_at
                          OR (e3.archived_at = archived_entries.archived_at
                              AND e3.id < archived_entries.id))
               )
         )
         WHERE id NOT IN ({ph})
           AND id IN (
               SELECT DISTINCT ea2.entry_id
               FROM entry_artifacts ea_sub
               JOIN entry_artifacts ea2  ON ea2.blob_id  = ea_sub.blob_id
               JOIN archived_entries e_sub ON e_sub.id   = ea_sub.entry_id
               JOIN archived_entries e2    ON e2.id      = ea2.entry_id
               WHERE ea_sub.entry_id IN ({ph})
                 AND ea2.entry_id NOT IN ({ph})
                 AND (e2.archived_at > e_sub.archived_at
                      OR (e2.archived_at = e_sub.archived_at
                          AND e2.id > e_sub.id))
           )"
    );
    conn.execute(&sql, rusqlite::params_from_iter(subtree_ids.iter()))?;
    Ok(())
}

/// Deletes an entry and every descendant in its tree (identified by `root_entry_id = entry_id`).
///
/// Call order matters:
/// 1. Collect all subtree IDs while the rows still exist.
/// 2. Run `cascade_cached_bytes_after_subtree_delete` in one SQL pass that excludes the entire
///    subtree — necessary so sibling blobs don't falsely satisfy the EXISTS check for each other.
/// 3. NULL `archive_run_items.produced_entry_id` for every subtree entry (FK has no ON DELETE
///    action; would otherwise block with `PRAGMA foreign_keys = ON`).
/// 4. Delete children before root (self-referential `parent_entry_id` FK has no cascade).
/// 5. Delete the root entry; CASCADE handles `entry_artifacts`, `entry_tag_assignments`,
///    and `collection_entries` automatically.
///
/// Returns `Ok(false)` if no entry with `entry_uid` was found; `Ok(true)` otherwise.
/// Wrap in a transaction at the call site for atomicity.
pub fn delete_entry(conn: &Connection, entry_uid: &str) -> Result<bool> {
    let entry_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM archived_entries WHERE entry_uid = ?1",
            [entry_uid],
            |row| row.get(0),
        )
        .optional()?;

    let entry_id = match entry_id {
        Some(id) => id,
        None => return Ok(false),
    };

    // Collect the full subtree while rows still exist.
    let subtree_ids: Vec<i64> = {
        let mut stmt = conn.prepare(
            "SELECT id FROM archived_entries WHERE root_entry_id = ?1",
        )?;
        stmt.query_map([entry_id], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };

    // One-pass set-aware cascade: recalculate cached_bytes for all external entries that
    // shared blobs with any subtree member, excluding every subtree ID simultaneously.
    cascade_cached_bytes_after_subtree_delete(conn, &subtree_ids)?;

    // Null the FK that has no ON DELETE action (covers root and all descendants).
    conn.execute(
        "UPDATE archive_run_items SET produced_entry_id = NULL
         WHERE produced_entry_id IN (
             SELECT id FROM archived_entries WHERE root_entry_id = ?1
         )",
        [entry_id],
    )?;

    // Children first — self-referential parent_entry_id FK has no cascade.
    conn.execute(
        "DELETE FROM archived_entries WHERE root_entry_id = ?1 AND id != ?1",
        [entry_id],
    )?;

    // Root entry: CASCADE handles entry_artifacts, entry_tag_assignments, collection_entries.
    conn.execute(
        "DELETE FROM archived_entries WHERE id = ?1",
        [entry_id],
    )?;

    Ok(true)
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

/// Returns `true` if any capture job in this archive is `pending` or `running`.
/// Call before scanning or deleting orphans: the capture pipeline moves files into
/// `raw/` before writing the DB rows, so a mid-capture scan can falsely flag live files.
pub fn has_active_capture_jobs(conn: &Connection) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM capture_jobs WHERE status IN ('pending', 'running')",
        [],
        |row| row.get(0),
    )?;
    Ok(n > 0)
}

/// Returns `(id, raw_relpath, byte_size)` for every blob row not referenced by any
/// `entry_artifacts.blob_id`.  These DB rows are safe to delete regardless of whether
/// a disk file still exists at their `raw_relpath`.
pub fn list_orphaned_blob_rows(conn: &Connection) -> Result<Vec<(i64, String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT id, raw_relpath, byte_size FROM blobs \
         WHERE id NOT IN \
           (SELECT DISTINCT blob_id FROM entry_artifacts WHERE blob_id IS NOT NULL)",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Returns the set of all file relpaths (relative to `store_path`) that are currently
/// referenced by at least one live entry_artifact, either directly via
/// `entry_artifacts.relpath` or indirectly via a live blob's `raw_relpath`.
/// Any disk file whose relpath is in this set must NOT be deleted.
pub fn all_referenced_file_relpaths(conn: &Connection) -> Result<std::collections::HashSet<String>> {
    let mut set = std::collections::HashSet::new();
    {
        let mut stmt = conn.prepare("SELECT DISTINCT relpath FROM entry_artifacts")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            set.insert(row.get::<_, String>(0)?);
        }
    }
    {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT raw_relpath FROM blobs \
             WHERE id IN \
               (SELECT DISTINCT blob_id FROM entry_artifacts WHERE blob_id IS NOT NULL)",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            set.insert(row.get::<_, String>(0)?);
        }
    }
    Ok(set)
}

/// Delete every blob row not referenced by any `entry_artifacts.blob_id`.
/// Returns the number of rows deleted.
pub fn delete_orphaned_blob_rows(conn: &Connection) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM blobs WHERE id NOT IN \
           (SELECT DISTINCT blob_id FROM entry_artifacts WHERE blob_id IS NOT NULL)",
        [],
    )?)
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

    // Auto-enroll in the default collection with appropriate visibility_bits.
    let default_coll_id = ensure_default_collection(conn)?;
    let vbits = visibility_to_bits(&entry.visibility);
    add_entry_to_collection(conn, default_coll_id, id, vbits)?;

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

pub fn rename_tag(
    conn: &Connection,
    tag_uid: &str,
    new_segment: &str,
) -> Result<Option<TagRecord>> {
    // Slugify: spaces→hyphens, keep alphanumeric and hyphens (case preserved), collapse runs, strip edges.
    let trimmed = new_segment.trim();
    let hyphenated: String = trimmed.chars().map(|c| if c == ' ' { '-' } else { c }).collect();
    let filtered: String = hyphenated
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-')
        .collect();
    let mut new_slug = String::new();
    let mut prev_hyphen = false;
    for c in filtered.chars() {
        if c == '-' {
            if !prev_hyphen {
                new_slug.push(c);
            }
            prev_hyphen = true;
        } else {
            new_slug.push(c);
            prev_hyphen = false;
        }
    }
    let new_slug = new_slug.trim_matches('-').to_string();
    if new_slug.is_empty() {
        bail!("new segment slugifies to empty string");
    }

    // Fetch existing tag.
    let tag = match get_tag_by_uid(conn, tag_uid)? {
        Some(t) => t,
        None => return Ok(None),
    };

    // Build new full_path by replacing the last segment.
    let old_prefix = tag.full_path.clone();
    let parent_prefix = match old_prefix.rfind('/') {
        Some(idx) => &old_prefix[..idx],
        None => "",
    };
    let new_full_path = format!("{}/{}", parent_prefix, new_slug);

    // Collision check: bail if another tag already owns this path.
    if let Some(existing) = get_tag_by_path(conn, &new_full_path)? {
        if existing.tag_uid != tag_uid {
            bail!("tag path already exists: {new_full_path}");
        }
    }

    let new_name = humanize_slug(&new_slug);

    // Transaction: update the tag row, then cascade path change to descendants.
    let result = (|| -> Result<()> {
        conn.execute_batch("BEGIN")?;
        conn.execute(
            "UPDATE tags SET name=?1, slug=?2, full_path=?3 WHERE tag_uid=?4",
            params![new_name, new_slug, new_full_path, tag_uid],
        )?;
        let old_prefix_slash = format!("{}/", old_prefix);
        let new_prefix_slash = format!("{}/", new_full_path);
        // Use hierarchy (recursive CTE over parent_tag_id) instead of LIKE to avoid
        // treating '_'/'%' in historical slugs as wildcards.
        conn.execute(
            "WITH RECURSIVE descendants(id) AS (\
                SELECT id FROM tags WHERE parent_tag_id = ?1 \
                UNION ALL \
                SELECT t.id FROM tags t JOIN descendants d ON t.parent_tag_id = d.id \
            ) \
            UPDATE tags SET full_path = REPLACE(full_path, ?2, ?3) \
            WHERE id IN (SELECT id FROM descendants)",
            params![tag.id, old_prefix_slash, new_prefix_slash],
        )?;
        conn.execute_batch("COMMIT")?;
        Ok(())
    })();

    if let Err(e) = result {
        let _ = conn.execute_batch("ROLLBACK");
        return Err(e);
    }

    // Re-fetch the updated record.
    get_tag_by_uid(conn, tag_uid)
}

/// Deletes a tag and its entire descendant subtree.
///
/// `entry_tag_assignments` rows are removed automatically via `ON DELETE CASCADE`.
/// `parent_tag_id` has no cascade so a recursive CTE is used to collect the subtree
/// before issuing a single DELETE.
///
/// Returns `Ok(true)` if anything was deleted, `Ok(false)` if `tag_uid` was not found.
pub fn delete_tag(conn: &Connection, tag_uid: &str) -> Result<bool> {
    let deleted = conn.execute(
        "WITH RECURSIVE subtree(id) AS (
             SELECT id FROM tags WHERE tag_uid = ?1
             UNION ALL
             SELECT t.id FROM tags t JOIN subtree s ON t.parent_tag_id = s.id
         )
         DELETE FROM tags WHERE id IN (SELECT id FROM subtree)",
        [tag_uid],
    )?;
    Ok(deleted > 0)
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

/// Maps legacy visibility strings to collection_entries.visibility_bits.
/// 'public'→3 (guest|user), 'unlisted'→2 (user only), 'private'→0 (nobody).
pub fn visibility_to_bits(visibility: &str) -> u32 {
    match visibility {
        "public" => 3,
        "unlisted" => 2,
        _ => 0,
    }
}

/// Returns the id of the '_default_' collection, creating it if absent.
pub fn ensure_default_collection(conn: &Connection) -> Result<i64> {
    let now = now_timestamp();
    conn.execute(
        "INSERT OR IGNORE INTO collections (collection_uid, name, slug, default_visibility_bits, created_at) \
         VALUES ('coll_default', 'All Entries', '_default_', 2, ?1)",
        [&now],
    )?;
    let id: i64 = conn.query_row(
        "SELECT id FROM collections WHERE slug = '_default_'",
        [],
        |row| row.get(0),
    )?;
    Ok(id)
}

/// Creates a new collection. Returns the created record.
pub fn create_collection(
    conn: &Connection,
    name: &str,
    slug: &str,
    default_visibility_bits: u32,
) -> Result<CollectionRecord> {
    if slug.is_empty() || slug.starts_with('_') {
        anyhow::bail!("collection slug must be non-empty and not start with underscore");
    }
    let collection_uid = public_id("coll");
    let now = now_timestamp();
    conn.execute(
        "INSERT INTO collections (collection_uid, name, slug, default_visibility_bits, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![collection_uid, name, slug, default_visibility_bits as i64, now],
    )?;
    let id = conn.last_insert_rowid();
    Ok(CollectionRecord {
        id,
        collection_uid,
        name: name.to_string(),
        slug: slug.to_string(),
        default_visibility_bits,
        created_at: now,
    })
}

/// Lists all collections ordered by creation date.
pub fn list_collections(conn: &Connection) -> Result<Vec<CollectionRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, collection_uid, name, slug, default_visibility_bits, created_at \
         FROM collections ORDER BY created_at ASC",
    )?;
    stmt.query_map([], |row| {
        Ok(CollectionRecord {
            id: row.get(0)?,
            collection_uid: row.get(1)?,
            name: row.get(2)?,
            slug: row.get(3)?,
            default_visibility_bits: row.get::<_, i64>(4)? as u32,
            created_at: row.get(5)?,
        })
    })?
    .collect::<Result<_, _>>()
    .map_err(Into::into)
}

/// Returns a collection by its uid, or None if not found.
pub fn get_collection_by_uid(
    conn: &Connection,
    uid: &str,
) -> Result<Option<CollectionRecord>> {
    conn.query_row(
        "SELECT id, collection_uid, name, slug, default_visibility_bits, created_at \
         FROM collections WHERE collection_uid = ?1",
        [uid],
        |row| {
            Ok(CollectionRecord {
                id: row.get(0)?,
                collection_uid: row.get(1)?,
                name: row.get(2)?,
                slug: row.get(3)?,
                default_visibility_bits: row.get::<_, i64>(4)? as u32,
                created_at: row.get(5)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

/// Adds an entry to a collection with given visibility_bits. Idempotent (INSERT OR IGNORE).
pub fn add_entry_to_collection(
    conn: &Connection,
    collection_id: i64,
    entry_id: i64,
    visibility_bits: u32,
) -> Result<()> {
    let now = now_timestamp();
    conn.execute(
        "INSERT OR IGNORE INTO collection_entries (collection_id, entry_id, visibility_bits, added_at) \
         VALUES (?1, ?2, ?3, ?4)",
        params![collection_id, entry_id, visibility_bits as i64, now],
    )?;
    Ok(())
}

/// Updates the visibility_bits of an entry in a collection. Returns true if updated.
pub fn update_collection_entry_visibility(
    conn: &Connection,
    collection_id: i64,
    entry_id: i64,
    visibility_bits: u32,
) -> Result<bool> {
    let n = conn.execute(
        "UPDATE collection_entries SET visibility_bits = ?1 \
         WHERE collection_id = ?2 AND entry_id = ?3",
        params![visibility_bits as i64, collection_id, entry_id],
    )?;
    Ok(n > 0)
}

/// Removes an entry from a collection. Returns true if removed.
pub fn remove_entry_from_collection(
    conn: &Connection,
    collection_id: i64,
    entry_id: i64,
) -> Result<bool> {
    let n = conn.execute(
        "DELETE FROM collection_entries WHERE collection_id = ?1 AND entry_id = ?2",
        params![collection_id, entry_id],
    )?;
    Ok(n > 0)
}

/// Returns (collection_id, collection_uid, visibility_bits) for all collections containing an entry.
pub fn get_entry_collection_memberships(
    conn: &Connection,
    entry_id: i64,
) -> Result<Vec<(i64, String, u32)>> {
    let mut stmt = conn.prepare(
        "SELECT ce.collection_id, c.collection_uid, ce.visibility_bits \
         FROM collection_entries ce \
         JOIN collections c ON c.id = ce.collection_id \
         WHERE ce.entry_id = ?1",
    )?;
    stmt.query_map([entry_id], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get::<_, i64>(2)? as u32))
    })?
    .collect::<Result<_, _>>()
    .map_err(Into::into)
}

/// Renames a collection and/or updates its default_visibility_bits.
/// Returns true if updated, false if not found.
/// Refuses to rename the '_default_' collection.
pub fn update_collection(
    conn: &Connection,
    collection_uid: &str,
    new_name: Option<&str>,
    new_visibility_bits: Option<u32>,
) -> Result<bool> {
    let coll = get_collection_by_uid(conn, collection_uid)?;
    let Some(coll) = coll else { return Ok(false) };
    if coll.slug == "_default_" {
        anyhow::bail!("cannot modify the default collection");
    }
    let name = new_name.unwrap_or(&coll.name);
    let vbits = new_visibility_bits.unwrap_or(coll.default_visibility_bits);
    conn.execute(
        "UPDATE collections SET name = ?1, default_visibility_bits = ?2 WHERE id = ?3",
        params![name, vbits as i64, coll.id],
    )?;
    Ok(true)
}

/// Deletes a collection and cascades to collection_entries.
/// Returns true if deleted, false if not found.
/// Refuses to delete the '_default_' collection.
pub fn delete_collection(
    conn: &Connection,
    collection_uid: &str,
) -> Result<bool> {
    let coll = get_collection_by_uid(conn, collection_uid)?;
    let Some(coll) = coll else { return Ok(false) };
    if coll.slug == "_default_" {
        anyhow::bail!("cannot delete the default collection");
    }
    conn.execute("DELETE FROM collections WHERE id = ?1", [coll.id])?;
    Ok(true)
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

    #[test]
    fn capture_job_create_and_get() {
        let conn = conn();
        let job_uid = create_capture_job(&conn, "personal").unwrap();
        let job = get_capture_job(&conn, &job_uid).unwrap().unwrap();
        assert_eq!(job.status, "pending");
        assert_eq!(job.archive_id, "personal");
        assert!(job.run_uid.is_none());
    }

    #[test]
    fn capture_job_status_transitions() {
        let conn = conn();
        let job_uid = create_capture_job(&conn, "test").unwrap();
        update_capture_job_status(&conn, &job_uid, "running", None, None).unwrap();
        update_capture_job_status(&conn, &job_uid, "completed", Some("run_abc"), None).unwrap();
        let job = get_capture_job(&conn, &job_uid).unwrap().unwrap();
        assert_eq!(job.status, "completed");
        assert_eq!(job.run_uid.as_deref(), Some("run_abc"));
    }

    #[test]
    fn fail_stalled_jobs_on_restart() {
        let conn = conn();

        // Simulate an in-progress capture_job (run_uid still NULL — common crash case).
        let uid = create_capture_job(&conn, "test").unwrap();
        update_capture_job_status(&conn, &uid, "running", None, None).unwrap();

        // Simulate an in-progress archive_run and item with no associated capture_job
        // (covers the case where run_uid was never written back before the crash).
        let user_id = ensure_default_user(&conn).unwrap();
        let run = create_archive_run(&conn, user_id, 1).unwrap();
        create_archive_run_item(&conn, run.id, None, 0, "https://example.com", None, "web", "file").unwrap();

        let n = fail_stalled_capture_jobs(&conn).unwrap();
        assert_eq!(n, 1); // one capture_job updated

        // capture_job is failed
        let job = get_capture_job(&conn, &uid).unwrap().unwrap();
        assert_eq!(job.status, "failed");
        assert!(job.error_text.as_deref().unwrap().contains("interrupted"));

        // archive_run is failed
        let updated_run: String = conn.query_row(
            "SELECT status FROM archive_runs WHERE id = ?1",
            [run.id],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(updated_run, "failed");

        // archive_run_item is failed
        let item_status: String = conn.query_row(
            "SELECT status FROM archive_run_items WHERE run_id = ?1",
            [run.id],
            |r| r.get(0),
        ).unwrap();
        assert_eq!(item_status, "failed");
    }

    fn make_auth_conn_for_mgmt() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        initialize_auth_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn user_create_and_list() {
        let conn = make_auth_conn_for_mgmt();
        let owner_id = create_owner(&conn, "owner", "hash").unwrap();
        let uid = create_user(&conn, "alice", Some("alice@example.com"), "hash2", owner_id).unwrap();
        let users = list_users(&conn).unwrap();
        assert_eq!(users.len(), 2);
        let alice = users.iter().find(|u| u.username == "alice").unwrap();
        assert_eq!(alice.user_uid, uid);
        assert_eq!(alice.status, "active");
        assert!(alice.role_slugs.contains(&"user".to_string()));
    }

    #[test]
    fn set_status_disables_user_and_kills_sessions() {
        let conn = make_auth_conn_for_mgmt();
        let owner_id = create_owner(&conn, "owner", "hash").unwrap();
        let uid = create_user(&conn, "bob", None, "hash", owner_id).unwrap();
        let bob_id: i64 = conn.query_row("SELECT id FROM users WHERE user_uid = ?1", [&uid], |r| r.get(0)).unwrap();
        create_session(&conn, bob_id, 3, None).unwrap();
        set_user_status(&conn, &uid, "disabled").unwrap();
        let sess_count: i64 = conn.query_row("SELECT COUNT(*) FROM sessions WHERE user_id = ?1", [bob_id], |r| r.get(0)).unwrap();
        assert_eq!(sess_count, 0, "sessions should be cleared on disable");
        let u = get_user_by_uid(&conn, &uid).unwrap().unwrap();
        assert_eq!(u.status, "disabled");
    }

    #[test]
    fn assign_and_remove_role() {
        let conn = make_auth_conn_for_mgmt();
        let owner_id = create_owner(&conn, "owner", "hash").unwrap();
        let uid = create_user(&conn, "carol", None, "hash", owner_id).unwrap();
        let carol_id = get_user_id_by_uid(&conn, &uid).unwrap().unwrap();
        let bits_before = compute_role_bits(&conn, carol_id).unwrap();
        assign_role(&conn, carol_id, "admin", owner_id).unwrap();
        let bits_after = compute_role_bits(&conn, carol_id).unwrap();
        assert!(bits_after & 4 != 0, "admin bit should be set");
        assert!(bits_after > bits_before);
        remove_role(&conn, carol_id, "admin").unwrap();
        let bits_final = compute_role_bits(&conn, carol_id).unwrap();
        assert!(bits_final & 4 == 0, "admin bit should be cleared");
    }

    #[test]
    fn custom_role_gets_next_bit_position() {
        let conn = make_auth_conn_for_mgmt();
        let r1 = create_custom_role(&conn, "moderator", "Moderator").unwrap();
        assert_eq!(r1.bit_position, 4);
        let r2 = create_custom_role(&conn, "helper", "Helper").unwrap();
        assert_eq!(r2.bit_position, 5);
        assert_eq!(r2.level, 2);
    }
    // ── rename_tag / delete_tag ────────────────────────────────────────────

    #[test]
    fn rename_tag_unknown_uid_returns_none() {
        let conn = conn();
        let result = rename_tag(&conn, "tag_doesnotexist", "anything").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn rename_tag_updates_own_path_and_cascades_to_children() {
        let conn = conn();
        // Create /science → /science/cs → /science/cs/algorithms
        let _ = create_tag_path(&conn, "science/cs/algorithms").unwrap();

        let science = get_tag_by_path(&conn, "/science").unwrap().unwrap();
        let cs      = get_tag_by_path(&conn, "/science/cs").unwrap().unwrap();
        let algo    = get_tag_by_path(&conn, "/science/cs/algorithms").unwrap().unwrap();

        // Rename "science" → "natural-science"
        let updated = rename_tag(&conn, &science.tag_uid, "natural-science")
            .unwrap()
            .expect("should return updated tag");

        assert_eq!(updated.slug,      "natural-science");
        assert_eq!(updated.name,      "Natural Science");
        assert_eq!(updated.full_path, "/natural-science");

        // /science must no longer exist
        assert!(get_tag_by_path(&conn, "/science").unwrap().is_none());

        // /science/cs must have moved
        assert!(get_tag_by_path(&conn, "/science/cs").unwrap().is_none());
        let cs_new = get_tag_by_uid(&conn, &cs.tag_uid).unwrap().unwrap();
        assert_eq!(cs_new.full_path, "/natural-science/cs");

        // /science/cs/algorithms must have moved
        assert!(get_tag_by_path(&conn, "/science/cs/algorithms").unwrap().is_none());
        let algo_new = get_tag_by_uid(&conn, &algo.tag_uid).unwrap().unwrap();
        assert_eq!(algo_new.full_path, "/natural-science/cs/algorithms");
    }

    #[test]
    fn rename_tag_sibling_collision_returns_err() {
        let conn = conn();
        // Create /science and /natural-science as siblings
        let _ = create_tag_path(&conn, "science").unwrap();
        let _ = create_tag_path(&conn, "natural-science").unwrap();

        let science = get_tag_by_path(&conn, "/science").unwrap().unwrap();

        // Renaming /science → natural-science should collide
        let result = rename_tag(&conn, &science.tag_uid, "natural-science");
        assert!(result.is_err(), "expected collision error, got {:?}", result);
    }

    #[test]
    fn rename_tag_to_same_name_is_noop() {
        let conn = conn();
        let _ = create_tag_path(&conn, "science").unwrap();
        let science = get_tag_by_path(&conn, "/science").unwrap().unwrap();

        // "Science" humanizes to the same slug; rename should succeed (no collision since same uid)
        let updated = rename_tag(&conn, &science.tag_uid, "science")
            .unwrap()
            .expect("should return tag");
        assert_eq!(updated.full_path, "/science");
    }

    #[test]
    fn delete_tag_unknown_uid_returns_false() {
        let conn = conn();
        assert!(!delete_tag(&conn, "tag_doesnotexist").unwrap());
    }

    #[test]
    fn delete_tag_removes_subtree_and_cascades_assignments() {
        let conn = conn();
        // Build /science/cs and /science/math
        let cs_id   = create_tag_path(&conn, "science/cs").unwrap();
        let math_id = create_tag_path(&conn, "science/math").unwrap();
        let science = get_tag_by_path(&conn, "/science").unwrap().unwrap();

        // Create an entry and assign it to /science/cs
        let entry = create_entry_fixture(&conn, "private", None, None);
        assign_entry_to_tag(&conn, entry.id, cs_id).unwrap();

        // Verify assignment exists
        let assigned_before: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM entry_tag_assignments WHERE entry_id = ?1",
                [entry.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(assigned_before, 1);

        // Delete the /science subtree
        assert!(delete_tag(&conn, &science.tag_uid).unwrap());

        // All three tag rows must be gone
        let tag_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM tags", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tag_count, 0, "all tags in subtree should be deleted");

        // Assignment must have been cascade-deleted
        let assigned_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM entry_tag_assignments WHERE entry_id = ?1",
                [entry.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(assigned_after, 0, "assignment should be removed by cascade");

        // Verify by uid too (subtree ids: science, cs, math)
        assert!(get_tag_by_uid(&conn, &science.tag_uid).unwrap().is_none());
        let cs_tag = conn
            .query_row("SELECT tag_uid FROM tags WHERE id = ?1", [cs_id], |r| r.get::<_, String>(0))
            .optional()
            .unwrap();
        assert!(cs_tag.is_none(), "/science/cs should be deleted");
        let math_tag = conn
            .query_row("SELECT tag_uid FROM tags WHERE id = ?1", [math_id], |r| r.get::<_, String>(0))
            .optional()
            .unwrap();
        assert!(math_tag.is_none(), "/science/math should be deleted");
    }

    #[test]
    fn rename_tag_slug_with_special_chars_is_stripped() {
        let conn = conn();
        let _ = create_tag_path(&conn, "science").unwrap();
        let science = get_tag_by_path(&conn, "/science").unwrap().unwrap();

        // Input with spaces and underscores — underscores stripped, spaces become hyphens, case preserved
        let updated = rename_tag(&conn, &science.tag_uid, "Natural Science")
            .unwrap()
            .expect("should rename");
        assert_eq!(updated.slug, "Natural-Science");
        assert_eq!(updated.full_path, "/Natural-Science");
    }

    // ── delete_entry tests ────────────────────────────────────────────────────

    /// Helper: attach a shared blob to an entry and return the blob id.
    fn attach_blob(conn: &Connection, entry_id: i64, sha256: &str, byte_size: i64) -> i64 {
        let blob = BlobRecord {
            sha256: sha256.to_string(),
            byte_size,
            mime_type: None,
            extension: None,
            raw_relpath: format!("raw/{sha256}"),
        };
        let blob_id = upsert_blob(conn, &blob).unwrap();
        add_entry_artifact(conn, &NewArtifact {
            entry_id,
            artifact_role: "main".to_string(),
            storage_area: "raw".to_string(),
            relpath: format!("raw/{sha256}"),
            blob_id: Some(blob_id),
            logical_path: None,
            metadata_json: None,
        }).unwrap();
        blob_id
    }

    #[test]
    fn delete_entry_returns_false_for_unknown_uid() {
        let conn = conn();
        assert!(!delete_entry(&conn, "entry_doesnotexist").unwrap());
    }

    #[test]
    fn delete_entry_removes_root_and_child_rows() {
        let conn = conn();
        let root = create_entry_fixture(&conn, "private", None, None);
        let child = create_entry_fixture(&conn, "private", Some(root.id), Some(root.id));

        delete_entry(&conn, &root.entry_uid).unwrap();

        let root_gone: Option<i64> = conn
            .query_row("SELECT id FROM archived_entries WHERE id = ?1", [root.id], |r| r.get(0))
            .optional().unwrap();
        let child_gone: Option<i64> = conn
            .query_row("SELECT id FROM archived_entries WHERE id = ?1", [child.id], |r| r.get(0))
            .optional().unwrap();
        assert!(root_gone.is_none(), "root should be gone");
        assert!(child_gone.is_none(), "child should be gone");
    }

    #[test]
    fn delete_entry_nulls_run_item_produced_entry_id() {
        let conn = conn();
        let user_id = ensure_default_user(&conn).unwrap();
        let root = create_entry_fixture(&conn, "private", None, None);
        let run = create_archive_run(&conn, user_id, 1).unwrap();
        let item = create_archive_run_item(
            &conn, run.id, None, 0, "https://example.com", None, "web", "page",
        ).unwrap();
        complete_archive_run_item(&conn, item.id, root.id).unwrap();

        delete_entry(&conn, &root.entry_uid).unwrap();

        let produced: Option<i64> = conn
            .query_row(
                "SELECT produced_entry_id FROM archive_run_items WHERE id = ?1",
                [item.id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(produced.is_none(), "produced_entry_id should be NULL after delete");
    }

    #[test]
    fn delete_entry_recalculates_cached_bytes_for_external_entries() {
        // Scenario: root (id=N) and child (id=N+1) both own blob X (100 bytes).
        // External (id=N+2, higher → newer by tiebreaker) also uses blob X.
        // Before delete: external.cached_bytes = 100 (blob owned by root).
        // After delete_entry(root): external.cached_bytes = 0 (no older entry remains).
        let conn = conn();

        let root = create_entry_fixture(&conn, "private", None, None);
        let child = create_entry_fixture(&conn, "private", Some(root.id), Some(root.id));
        let external = create_entry_fixture(&conn, "private", None, None);

        // Attach the same blob to all three.
        attach_blob(&conn, root.id, "blobx", 100);
        attach_blob(&conn, child.id, "blobx", 100);
        attach_blob(&conn, external.id, "blobx", 100);

        // Compute external.cached_bytes before delete — root and child are older by id.
        refresh_entry_cached_bytes(&conn, external.id).unwrap();
        let before: i64 = conn
            .query_row("SELECT cached_bytes FROM archived_entries WHERE id = ?1", [external.id], |r| r.get(0))
            .unwrap();
        assert_eq!(before, 100, "external should see blob as cached before delete");

        delete_entry(&conn, &root.entry_uid).unwrap();

        // external must still exist but with cached_bytes = 0.
        let after: i64 = conn
            .query_row("SELECT cached_bytes FROM archived_entries WHERE id = ?1", [external.id], |r| r.get(0))
            .unwrap();
        assert_eq!(after, 0, "cached_bytes must be 0 after whole subtree is deleted");
    }

    // ── Orphan blob cleanup ───────────────────────────────────────────────────────

    #[test]
    fn has_active_capture_jobs_false_when_none() {
        let conn = conn();
        assert!(!has_active_capture_jobs(&conn).unwrap());
    }

    #[test]
    fn has_active_capture_jobs_true_for_pending() {
        let conn = conn();
        create_capture_job(&conn, "test").unwrap();
        assert!(has_active_capture_jobs(&conn).unwrap());
    }

    #[test]
    fn has_active_capture_jobs_true_for_running() {
        let conn = conn();
        let uid = create_capture_job(&conn, "test").unwrap();
        update_capture_job_status(&conn, &uid, "running", None, None).unwrap();
        assert!(has_active_capture_jobs(&conn).unwrap());
    }

    #[test]
    fn has_active_capture_jobs_false_for_completed() {
        let conn = conn();
        let uid = create_capture_job(&conn, "test").unwrap();
        update_capture_job_status(&conn, &uid, "completed", Some("run_x"), None).unwrap();
        assert!(!has_active_capture_jobs(&conn).unwrap());
    }

    #[test]
    fn list_orphaned_blob_rows_empty_when_blob_is_referenced() {
        let conn = conn();
        let entry = create_entry_fixture(&conn, "private", None, None);
        let blob = BlobRecord {
            sha256: "aaa111".to_string(),
            byte_size: 100,
            mime_type: None,
            extension: Some("mp4".to_string()),
            raw_relpath: "raw/a/a/aaa111.mp4".to_string(),
        };
        let blob_id = upsert_blob(&conn, &blob).unwrap();
        add_entry_artifact(&conn, &NewArtifact {
            entry_id: entry.id,
            artifact_role: "main".to_string(),
            storage_area: "raw".to_string(),
            relpath: blob.raw_relpath.clone(),
            blob_id: Some(blob_id),
            logical_path: None,
            metadata_json: None,
        }).unwrap();
        assert!(list_orphaned_blob_rows(&conn).unwrap().is_empty(),
            "referenced blob must not appear as orphan");
    }

    #[test]
    fn list_orphaned_blob_rows_finds_unreferenced_blob() {
        let conn = conn();
        upsert_blob(&conn, &BlobRecord {
            sha256: "bbb222".to_string(),
            byte_size: 200,
            mime_type: None,
            extension: Some("jpg".to_string()),
            raw_relpath: "raw/b/b/bbb222.jpg".to_string(),
        }).unwrap();
        let orphans = list_orphaned_blob_rows(&conn).unwrap();
        assert_eq!(orphans.len(), 1, "unreferenced blob must appear as orphan");
        assert_eq!(orphans[0].1, "raw/b/b/bbb222.jpg");
    }

    #[test]
    fn all_referenced_file_relpaths_covers_blob_and_direct_artifact_relpaths() {
        let conn = conn();
        let entry = create_entry_fixture(&conn, "private", None, None);
        // Live blob: linked via blob_id
        let blob = BlobRecord {
            sha256: "live1".to_string(), byte_size: 50,
            mime_type: None, extension: None,
            raw_relpath: "raw/l/i/live1".to_string(),
        };
        let blob_id = upsert_blob(&conn, &blob).unwrap();
        add_entry_artifact(&conn, &NewArtifact {
            entry_id: entry.id,
            artifact_role: "main".to_string(),
            storage_area: "raw".to_string(),
            relpath: blob.raw_relpath.clone(),
            blob_id: Some(blob_id),
            logical_path: None, metadata_json: None,
        }).unwrap();
        // Artifact referencing a file directly (no blob_id)
        add_entry_artifact(&conn, &NewArtifact {
            entry_id: entry.id,
            artifact_role: "sidecar".to_string(),
            storage_area: "raw".to_string(),
            relpath: "raw/s/i/sidecar.vtt".to_string(),
            blob_id: None,
            logical_path: None, metadata_json: None,
        }).unwrap();
        let refs = all_referenced_file_relpaths(&conn).unwrap();
        assert!(refs.contains("raw/l/i/live1"), "live blob relpath must be protected");
        assert!(refs.contains("raw/s/i/sidecar.vtt"), "direct artifact relpath must be protected");
    }

    #[test]
    fn delete_orphaned_blob_rows_removes_only_unreferenced() {
        let conn = conn();
        let entry = create_entry_fixture(&conn, "private", None, None);
        // Referenced blob
        let live = BlobRecord {
            sha256: "live9999".to_string(), byte_size: 10,
            mime_type: None, extension: None,
            raw_relpath: "raw/l/v/live9999".to_string(),
        };
        let live_id = upsert_blob(&conn, &live).unwrap();
        add_entry_artifact(&conn, &NewArtifact {
            entry_id: entry.id,
            artifact_role: "main".to_string(),
            storage_area: "raw".to_string(),
            relpath: live.raw_relpath.clone(),
            blob_id: Some(live_id),
            logical_path: None, metadata_json: None,
        }).unwrap();
        // Orphaned blob
        upsert_blob(&conn, &BlobRecord {
            sha256: "dead0000".to_string(), byte_size: 20,
            mime_type: None, extension: None,
            raw_relpath: "raw/d/e/dead0000".to_string(),
        }).unwrap();
        let deleted = delete_orphaned_blob_rows(&conn).unwrap();
        assert_eq!(deleted, 1, "only the unreferenced blob row should be deleted");
        assert!(get_blob_by_sha256(&conn, "live9999").unwrap().is_some(),
            "referenced blob row must be preserved");
    }

    #[test]
    fn orphan_blob_row_whose_relpath_is_artifact_relpath_stays_in_referenced_set() {
        // Blob row has no blob_id reference (would be deleted from DB),
        // but an artifact points to the same file via relpath — the file must
        // appear in all_referenced_file_relpaths so it won't be deleted from disk.
        let conn = conn();
        let entry = create_entry_fixture(&conn, "private", None, None);
        let blob = BlobRecord {
            sha256: "edgecase".to_string(), byte_size: 30,
            mime_type: None, extension: None,
            raw_relpath: "raw/e/d/edgecase".to_string(),
        };
        upsert_blob(&conn, &blob).unwrap();
        // artifact uses same relpath but no blob_id
        add_entry_artifact(&conn, &NewArtifact {
            entry_id: entry.id,
            artifact_role: "sidecar".to_string(),
            storage_area: "raw".to_string(),
            relpath: blob.raw_relpath.clone(),
            blob_id: None,
            logical_path: None, metadata_json: None,
        }).unwrap();
        // blob row is orphaned (no blob_id reference)
        assert_eq!(list_orphaned_blob_rows(&conn).unwrap().len(), 1);
        // but the file relpath is still protected
        let refs = all_referenced_file_relpaths(&conn).unwrap();
        assert!(refs.contains(&blob.raw_relpath),
            "file must be protected because artifact.relpath references it directly");
    }

}
