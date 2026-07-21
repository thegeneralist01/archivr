use anyhow::{Context, Result, bail};
use rusqlite::OptionalExtension;
use std::{
    env, fs,
    path::{Path, PathBuf},
};

use crate::database;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivePaths {
    pub archive_path: PathBuf,
    pub store_path: PathBuf,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct EntrySummary {
    pub entry_uid: String,
    pub archived_at: String,
    pub source_kind: String,
    pub entity_kind: String,
    pub title: Option<String>,
    pub visibility: String,
    pub original_url: Option<String>,
    pub artifact_count: i64,
    pub total_artifact_bytes: i64,
    pub parent_entry_uid: Option<String>,
    /// True if a `favicon` artifact exists for this entry.
    pub has_favicon: bool,
    /// Bytes of blobs already on disk from an earlier entry (precomputed at capture time).
    pub cached_bytes: i64,
    /// Number of direct child entries; 0 for non-container entries.
    pub child_count: i64,
    /// Total non-avatar artifact bytes (query-time; used as denominator for cache-hit %).
    pub cacheable_bytes: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct EntryDetail {
    pub summary: EntrySummary,
    pub structured_root_relpath: String,
    pub source_metadata_json: String,
    pub display_metadata_json: Option<String>,
    pub artifacts: Vec<EntryArtifactSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct EntryArtifactSummary {
    pub artifact_role: String,
    pub storage_area: String,
    pub relpath: String,
    pub byte_size: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RunSummary {
    pub run_uid: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub requested_count: i64,
    pub discovered_count: i64,
    pub completed_count: i64,
    pub failed_count: i64,
    pub error_summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CaptureJobSummary {
    pub job_uid: String,
    pub archive_id: String,
    pub run_uid: Option<String>,
    pub status: String,
    pub error_text: Option<String>,
    pub notes_json: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Tag {
    pub tag_uid: String,
    pub name: String,
    pub slug: String,
    pub full_path: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TagNode {
    pub tag: Tag,
    pub entry_count: i64,
    pub subtree_count: i64,
    pub children: Vec<TagNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CollectionSummary {
    pub collection_uid: String,
    pub name: String,
    pub slug: String,
    pub default_visibility_bits: u32,
    pub created_at: String,
}

pub fn find_archive_path_from(start: &Path) -> Result<Option<PathBuf>> {
    let mut dir = start.to_path_buf();
    loop {
        let candidate = dir.join(".archivr");
        if candidate.is_dir() {
            return Ok(Some(candidate));
        }
        if !dir.pop() {
            return Ok(None);
        }
    }
}

pub fn find_archive_path() -> Result<Option<PathBuf>> {
    let cwd = env::current_dir().context("failed to read current working directory")?;
    find_archive_path_from(&cwd)
}

pub fn read_archive_paths(archive_path: &Path) -> Result<ArchivePaths> {
    if !archive_path.is_dir() {
        bail!("archive path does not exist: {}", archive_path.display());
    }

    let name = fs::read_to_string(archive_path.join("name"))
        .with_context(|| format!("failed to read archive name in {}", archive_path.display()))?
        .trim()
        .to_string();
    let store_path = fs::read_to_string(archive_path.join("store_path"))
        .with_context(|| format!("failed to read store path in {}", archive_path.display()))?;

    Ok(ArchivePaths {
        archive_path: archive_path.to_path_buf(),
        store_path: PathBuf::from(store_path.trim()),
        name,
    })
}

pub fn initialize_archive(
    archive_parent: &Path,
    store_path: &Path,
    archive_name: &str,
    force_with_info_removal: bool,
) -> Result<ArchivePaths> {
    let archive_path = archive_parent.join(".archivr");

    if archive_path.exists() {
        if !archive_path.is_dir() {
            bail!(
                "Archive path exists and is not a directory: {}",
                archive_path.display()
            );
        }

        if force_with_info_removal {
            fs::remove_dir_all(&archive_path)?;
        } else if fs::read_dir(&archive_path)?.next().is_some() {
            bail!(
                "Archive already exists at {} and is not empty. Use --force-with-info-removal to reinitialize.",
                archive_path.display()
            );
        }
    }

    if store_path.exists() && !force_with_info_removal {
        bail!("Store path already exists at {}", store_path.display());
    }

    fs::create_dir_all(&archive_path)?;
    fs::create_dir_all(store_path)?;
    fs::write(archive_path.join("name"), archive_name)?;
    let canonical_store_path = store_path
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", store_path.display()))?;
    fs::write(
        archive_path.join("store_path"),
        canonical_store_path
            .to_str()
            .context("store path is not valid UTF-8")?,
    )?;

    initialize_store_directories(&canonical_store_path)?;
    let conn = database::open_or_initialize(&archive_path)?;
    let _ = database::ensure_default_user(&conn)?;

    Ok(ArchivePaths {
        archive_path,
        store_path: canonical_store_path,
        name: archive_name.to_string(),
    })
}

pub fn initialize_store_directories(store_path: &Path) -> Result<()> {
    fs::create_dir_all(store_path.join("raw"))?;
    fs::create_dir_all(store_path.join("raw_tweets"))?;
    fs::create_dir_all(store_path.join("structured"))?;
    fs::create_dir_all(store_path.join("temp"))?;
    Ok(())
}

pub fn list_root_entries(
    conn: &rusqlite::Connection,
    caller_bits: u32,
) -> Result<Vec<EntrySummary>> {
    let mut stmt = conn.prepare(
        "SELECT
            e.entry_uid,
            e.archived_at,
            e.source_kind,
            e.entity_kind,
            e.title,
            e.visibility,
            si.canonical_url,
            COUNT(ea.id) AS artifact_count,
            COALESCE(SUM(b.byte_size), 0) + COALESCE((SELECT SUM(b2.byte_size) FROM archived_entries c2 JOIN entry_artifacts ea2 ON ea2.entry_id = c2.id JOIN blobs b2 ON b2.id = ea2.blob_id WHERE c2.parent_entry_id = e.id), 0) AS total_artifact_bytes,
            NULL AS parent_entry_uid,
            EXISTS(SELECT 1 FROM entry_artifacts fav WHERE fav.entry_id = e.id AND fav.artifact_role = 'favicon') AS has_favicon,
            e.cached_bytes,
            (SELECT COUNT(*) FROM archived_entries child WHERE child.parent_entry_id = e.id) AS child_count,
            COALESCE(SUM(CASE WHEN ea.artifact_role != 'avatar' THEN b.byte_size ELSE 0 END), 0) + COALESCE((SELECT SUM(b2.byte_size) FROM archived_entries c2 JOIN entry_artifacts ea2 ON ea2.entry_id = c2.id JOIN blobs b2 ON b2.id = ea2.blob_id WHERE c2.parent_entry_id = e.id), 0) AS cacheable_bytes
         FROM archived_entries e
         JOIN source_identities si ON si.id = e.source_identity_id
         LEFT JOIN entry_artifacts ea ON ea.entry_id = e.id
         LEFT JOIN blobs b ON b.id = ea.blob_id
         WHERE e.parent_entry_id IS NULL
         AND (
             CAST(?1 AS INTEGER) & 12 != 0
             OR EXISTS (
                 SELECT 1 FROM collection_entries ce
                 WHERE ce.entry_id = e.id
                   AND ce.visibility_bits & CAST(?1 AS INTEGER) != 0
             )
         )
         GROUP BY e.id
         ORDER BY e.archived_at DESC, e.id DESC",
    )?;

    let entries = stmt
        .query_map([caller_bits as i64], |row| {
            Ok(EntrySummary {
                entry_uid: row.get(0)?,
                archived_at: row.get(1)?,
                source_kind: row.get(2)?,
                entity_kind: row.get(3)?,
                title: row.get(4)?,
                visibility: row.get(5)?,
                original_url: row.get(6)?,
                artifact_count: row.get(7)?,
                total_artifact_bytes: row.get(8)?,
                parent_entry_uid: row.get(9)?,
                has_favicon: row.get::<_, i64>(10)? != 0,
                cached_bytes: row.get(11)?,
                child_count: row.get(12)?,
                cacheable_bytes: row.get(13)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(entries)
}

/// Fetches one `EntrySummary` for any entry (root or child) by uid.
/// Returns `None` if not found.
fn get_entry_summary(
    conn: &rusqlite::Connection,
    entry_uid: &str,
) -> Result<Option<EntrySummary>> {
    let sql = format!(
        "{} {} WHERE e.entry_uid = ?1 GROUP BY e.id",
        ENTRY_SELECT_COLS, ENTRY_FROM_JOINS,
    );
    let mut stmt = conn.prepare(&sql)?;
    let result = stmt
        .query_row([entry_uid], |row| {
            Ok(EntrySummary {
                entry_uid: row.get(0)?,
                archived_at: row.get(1)?,
                source_kind: row.get(2)?,
                entity_kind: row.get(3)?,
                title: row.get(4)?,
                visibility: row.get(5)?,
                original_url: row.get(6)?,
                artifact_count: row.get(7)?,
                total_artifact_bytes: row.get(8)?,
                parent_entry_uid: row.get(9)?,
                has_favicon: row.get::<_, i64>(10)? != 0,
                cached_bytes: row.get(11)?,
                child_count: row.get(12)?,
                cacheable_bytes: row.get(13)?,
            })
        })
        .optional()?;
    Ok(result)
}

pub fn get_entry_detail(
    conn: &rusqlite::Connection,
    entry_uid: &str,
) -> Result<Option<EntryDetail>> {
    let Some((entry_id, structured_root_relpath, source_metadata_json, display_metadata_json)) =
        conn.query_row(
            "SELECT id, structured_root_relpath, source_metadata_json, display_metadata_json
             FROM archived_entries
             WHERE entry_uid = ?1",
            [entry_uid],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()?
    else {
        return Ok(None);
    };

    let summary = get_entry_summary(conn, entry_uid)?
        .context("entry disappeared while loading detail")?;

    let mut stmt = conn.prepare(
        "SELECT ea.artifact_role, ea.storage_area, ea.relpath, b.byte_size
         FROM entry_artifacts ea
         LEFT JOIN blobs b ON b.id = ea.blob_id
         WHERE ea.entry_id = ?1
         ORDER BY ea.id ASC",
    )?;
    let artifacts = stmt
        .query_map([entry_id], |row| {
            Ok(EntryArtifactSummary {
                artifact_role: row.get(0)?,
                storage_area: row.get(1)?,
                relpath: row.get(2)?,
                byte_size: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(Some(EntryDetail {
        summary,
        structured_root_relpath,
        source_metadata_json,
        display_metadata_json,
        artifacts,
    }))
}

pub fn list_runs(conn: &rusqlite::Connection) -> Result<Vec<RunSummary>> {
    let mut stmt = conn.prepare(
        "SELECT run_uid, started_at, finished_at, status, requested_count,
                discovered_count, completed_count, failed_count, error_summary
         FROM archive_runs
         ORDER BY started_at DESC, id DESC",
    )?;
    let runs = stmt
        .query_map([], |row| {
            Ok(RunSummary {
                run_uid: row.get(0)?,
                started_at: row.get(1)?,
                finished_at: row.get(2)?,
                status: row.get(3)?,
                requested_count: row.get(4)?,
                discovered_count: row.get(5)?,
                completed_count: row.get(6)?,
                failed_count: row.get(7)?,
                error_summary: row.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(runs)
}

pub fn get_capture_job(
    conn: &rusqlite::Connection,
    job_uid: &str,
) -> Result<Option<CaptureJobSummary>> {
    Ok(
        database::get_capture_job(conn, job_uid)?.map(|r| CaptureJobSummary {
            job_uid: r.job_uid,
            archive_id: r.archive_id,
            run_uid: r.run_uid,
            status: r.status,
            error_text: r.error_text,
            notes_json: r.notes_json,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }),
    )
}

/// Lists all collections in the archive.
pub fn list_collections(conn: &rusqlite::Connection) -> Result<Vec<CollectionSummary>> {
    let records = database::list_collections(conn)?;
    Ok(records
        .into_iter()
        .map(|r| CollectionSummary {
            collection_uid: r.collection_uid,
            name: r.name,
            slug: r.slug,
            default_visibility_bits: r.default_visibility_bits,
            created_at: r.created_at,
        })
        .collect())
}

/// Represents an entry's membership in a collection with its visibility bits.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EntryCollectionMembership {
    pub collection_uid: String,
    pub visibility_bits: u32,
}

/// Returns collection memberships for the given entry_uid.
/// Returns Ok(None) if the entry_uid does not exist.
pub fn get_entry_collections(
    conn: &rusqlite::Connection,
    entry_uid: &str,
) -> Result<Option<Vec<EntryCollectionMembership>>> {
    let Some(entry_id) = conn
        .query_row(
            "SELECT id FROM archived_entries WHERE entry_uid = ?1",
            [entry_uid],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
    else {
        return Ok(None);
    };
    let memberships = database::get_entry_collection_memberships(conn, entry_id)?;
    Ok(Some(
        memberships
            .into_iter()
            .map(|(_, uid, bits)| EntryCollectionMembership {
                collection_uid: uid,
                visibility_bits: bits,
            })
            .collect(),
    ))
}

/// Returns entries belonging to a collection, filtered by caller_bits visibility.
/// Caller with admin/owner bits (4|8) sees all entries regardless of visibility_bits.
pub fn list_entries_for_collection(
    conn: &rusqlite::Connection,
    collection_id: i64,
    caller_bits: u32,
) -> Result<Vec<EntrySummary>> {
    let sql = format!(
        "{} {} \
         JOIN collection_entries ce ON ce.entry_id = e.id \
         WHERE ce.collection_id = ?1 \
         AND (CAST(?2 AS INTEGER) & 12 != 0 \
              OR (ce.visibility_bits & CAST(?2 AS INTEGER)) != 0) \
         GROUP BY e.id \
         ORDER BY e.archived_at DESC, e.id DESC",
        ENTRY_SELECT_COLS, ENTRY_FROM_JOINS,
    );
    let mut stmt = conn.prepare(&sql)?;
    let entries = stmt
        .query_map([collection_id, caller_bits as i64], |row| {
            Ok(EntrySummary {
                entry_uid: row.get(0)?,
                archived_at: row.get(1)?,
                source_kind: row.get(2)?,
                entity_kind: row.get(3)?,
                title: row.get(4)?,
                visibility: row.get(5)?,
                original_url: row.get(6)?,
                artifact_count: row.get(7)?,
                total_artifact_bytes: row.get(8)?,
                parent_entry_uid: row.get(9)?,
                has_favicon: row.get::<_, i64>(10)? != 0,
                cached_bytes: row.get(11)?,
                child_count: row.get(12)?,
                cacheable_bytes: row.get(13)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(entries)
}

/// Returns the direct children of the entry identified by `parent_entry_uid`,
/// ordered ascending by `archived_at, id` (preserves playlist ordinal feel).
/// Returns an empty vec if the parent has no children or does not exist.
pub fn list_child_entries(
    conn: &rusqlite::Connection,
    parent_entry_uid: &str,
    caller_bits: u32,
) -> Result<Vec<EntrySummary>> {
    let sql = format!(
        "{} {} \
         WHERE e.parent_entry_id = (SELECT id FROM archived_entries WHERE entry_uid = ?1) \
         AND (\
             CAST(?2 AS INTEGER) & 12 != 0 \
             OR EXISTS (\
                 SELECT 1 FROM collection_entries ce \
                 WHERE ce.entry_id = e.id \
                   AND ce.visibility_bits & CAST(?2 AS INTEGER) != 0\
             )\
             OR EXISTS (\
                 SELECT 1 FROM collection_entries ce_p \
                 WHERE ce_p.entry_id = (SELECT id FROM archived_entries WHERE entry_uid = ?1)\
                   AND ce_p.visibility_bits & CAST(?2 AS INTEGER) != 0\
             )\
         ) \
         GROUP BY e.id \
         ORDER BY e.archived_at ASC, e.id ASC",
        ENTRY_SELECT_COLS, ENTRY_FROM_JOINS,
    );
    let mut stmt = conn.prepare(&sql)?;
    let entries = stmt
        .query_map(
            rusqlite::params![parent_entry_uid, caller_bits as i64],
            |row| {
                Ok(EntrySummary {
                    entry_uid: row.get(0)?,
                    archived_at: row.get(1)?,
                    source_kind: row.get(2)?,
                    entity_kind: row.get(3)?,
                    title: row.get(4)?,
                    visibility: row.get(5)?,
                    original_url: row.get(6)?,
                    artifact_count: row.get(7)?,
                    total_artifact_bytes: row.get(8)?,
                    parent_entry_uid: row.get(9)?,
                    has_favicon: row.get::<_, i64>(10)? != 0,
                    cached_bytes: row.get(11)?,
                    child_count: row.get(12)?,
                    cacheable_bytes: row.get(13)?,
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(entries)
}

/// Returns the set of canonical URLs for all child entries archived under
/// **any** container entry whose own canonical URL matches `playlist_canonical_url`.
///
/// Used in sync mode so the playlist capture path can skip videos that were
/// already downloaded in a previous run of the same playlist.
pub fn get_archived_playlist_child_urls(
    conn: &rusqlite::Connection,
    playlist_canonical_url: &str,
) -> Result<std::collections::HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT si_child.canonical_url \
         FROM archived_entries child \
         JOIN source_identities si_child ON si_child.id = child.source_identity_id \
         WHERE child.parent_entry_id IN ( \
             SELECT e.id \
             FROM archived_entries e \
             JOIN source_identities si ON si.id = e.source_identity_id \
             WHERE si.canonical_url = ?1 \
               AND e.parent_entry_id IS NULL \
         )",
    )?;
    let urls = stmt
        .query_map([playlist_canonical_url], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<std::collections::HashSet<_>>>()?;
    Ok(urls)
}

/// Finds the most recent container entry (parent_entry_id IS NULL) whose
/// canonical URL matches `canonical_url`. Returns its row id, or None if
/// no such entry exists.
///
/// Used in sync mode to reuse an existing playlist/channel container instead
/// of creating a duplicate root entry on every sync run.
pub fn find_container_entry_id_by_canonical_url(
    conn: &rusqlite::Connection,
    canonical_url: &str,
) -> Result<Option<i64>> {
    let mut stmt = conn.prepare(
        "SELECT e.id \
         FROM archived_entries e \
         JOIN source_identities si ON si.id = e.source_identity_id \
         WHERE si.canonical_url = ?1 \
           AND e.parent_entry_id IS NULL \
         ORDER BY e.archived_at DESC \
         LIMIT 1",
    )?;
    let id = stmt
        .query_row([canonical_url], |row| row.get::<_, i64>(0))
        .optional()?;
    Ok(id)
}

/// Resolves an artifact to its absolute on-disk path under `store_path`.
///
/// `artifact.relpath` is a store-relative path (e.g. `raw/a/b/abc.pdf`).
/// The returned path is canonicalized. Returns an error if the resolved path
/// escapes `store_path` (path traversal protection) or if the file does not exist.
pub fn resolve_artifact_path(
    store_path: &Path,
    artifact: &EntryArtifactSummary,
) -> Result<PathBuf> {
    let joined = store_path.join(&artifact.relpath);
    let canonical_store = store_path.canonicalize().with_context(|| {
        format!(
            "failed to canonicalize store path: {}",
            store_path.display()
        )
    })?;
    let canonical_artifact = joined
        .canonicalize()
        .with_context(|| format!("artifact path does not exist: {}", joined.display()))?;
    if !canonical_artifact.starts_with(&canonical_store) {
        bail!(
            "artifact path escapes store: {}",
            canonical_artifact.display()
        );
    }
    Ok(canonical_artifact)
}

#[derive(Debug, Clone)]
pub struct SearchEntriesQuery {
    /// Free-text term: LIKE-matched against title, canonical_url, entry_uid, source_kind, entity_kind, visibility
    pub q: Option<String>,
    /// Exact match on e.source_kind
    pub source_kind: Option<String>,
    /// Exact match on e.entity_kind
    pub entity_kind: Option<String>,
    /// LIKE-matched against si.canonical_url
    pub url: Option<String>,
    /// LIKE-matched against e.title
    pub title: Option<String>,
    /// e.archived_at >= after (inclusive, ISO 8601)
    pub after: Option<String>,
    /// e.archived_at < before (exclusive, ISO 8601)
    pub before: Option<String>,
    /// Tag full_path filter; includes all entries (root + child) matching the tag subtree
    pub tag: Option<String>,
    /// Role bits of the caller for visibility filtering. Admins (bits 4/8) bypass all filters.
    /// Pass `u32::MAX` internally to bypass all visibility. Pass 0 for unauthenticated guests only.
    pub caller_bits: u32,
}

impl Default for SearchEntriesQuery {
    fn default() -> Self {
        Self {
            q: None,
            source_kind: None,
            entity_kind: None,
            url: None,
            title: None,
            after: None,
            before: None,
            tag: None,
            caller_bits: u32::MAX,
        }
    }
}

/// Parses a raw search string into a [`SearchEntriesQuery`].
///
/// Recognized prefixes: `source:`, `type:`, `url:`, `title:`, `after:`, `before:`, `tag:`.
/// Tokens with an unrecognized `prefix:` return `Err(prefix)`.
/// Remaining non-prefix tokens are joined as the free-text `q`.
/// Quoted values (`title:"resume templates"`) are supported for single-word values
/// after the colon; leading/trailing double quotes are stripped.
pub fn parse_search_query(raw: &str) -> Result<SearchEntriesQuery, String> {
    let mut query = SearchEntriesQuery::default();
    let mut free_text_tokens: Vec<&str> = Vec::new();

    for token in raw.split_whitespace() {
        if let Some(colon_pos) = token.find(':') {
            let prefix = &token[..colon_pos];
            let value_raw = &token[colon_pos + 1..];
            // Strip surrounding double quotes if present
            let value = value_raw.trim_matches('"').to_string();

            match prefix {
                "source" => query.source_kind = Some(value),
                "type" => query.entity_kind = Some(value),
                "url" => query.url = Some(value),
                "title" => query.title = Some(value),
                "after" => query.after = Some(value),
                "before" => query.before = Some(value),
                "tag" => query.tag = Some(value),
                other => return Err(other.to_string()),
            }
        } else {
            free_text_tokens.push(token);
        }
    }

    let q = free_text_tokens.join(" ");
    query.q = if q.is_empty() { None } else { Some(q) };

    Ok(query)
}

const ENTRY_SELECT_COLS: &str = "SELECT e.entry_uid, e.archived_at, e.source_kind, e.entity_kind, e.title, \
    e.visibility, si.canonical_url, COUNT(ea.id) AS artifact_count, \
    COALESCE(SUM(b.byte_size), 0) + COALESCE((SELECT SUM(b2.byte_size) FROM archived_entries c2 JOIN entry_artifacts ea2 ON ea2.entry_id = c2.id JOIN blobs b2 ON b2.id = ea2.blob_id WHERE c2.parent_entry_id = e.id), 0) AS total_artifact_bytes, \
    parent.entry_uid AS parent_entry_uid, \
    EXISTS(SELECT 1 FROM entry_artifacts fav WHERE fav.entry_id = e.id AND fav.artifact_role = 'favicon') AS has_favicon, \
    e.cached_bytes, \
    (SELECT COUNT(*) FROM archived_entries child WHERE child.parent_entry_id = e.id) AS child_count, \
    COALESCE(SUM(CASE WHEN ea.artifact_role != 'avatar' THEN b.byte_size ELSE 0 END), 0) + COALESCE((SELECT SUM(b2.byte_size) FROM archived_entries c2 JOIN entry_artifacts ea2 ON ea2.entry_id = c2.id JOIN blobs b2 ON b2.id = ea2.blob_id WHERE c2.parent_entry_id = e.id), 0) AS cacheable_bytes";

const ENTRY_FROM_JOINS: &str = "FROM archived_entries e \
    JOIN source_identities si ON si.id = e.source_identity_id \
    LEFT JOIN entry_artifacts ea ON ea.entry_id = e.id \
    LEFT JOIN blobs b ON b.id = ea.blob_id \
    LEFT JOIN archived_entries parent ON parent.id = e.parent_entry_id";

/// Searches archived entries matching all non-`None` fields in `query`.
///
/// Without a tag filter, returns root entries only (same scope as [`list_root_entries`]).
/// With a tag filter, returns ALL entries (root and child) assigned to that tag subtree.
pub fn search_entries(
    conn: &rusqlite::Connection,
    query: &SearchEntriesQuery,
) -> Result<Vec<EntrySummary>> {
    let mut params: Vec<String> = Vec::new();
    let mut sql;

    if let Some(tag_path) = &query.tag {
        params.push(tag_path.clone());
        sql = format!(
            "WITH RECURSIVE descendants(id) AS (\
                SELECT id FROM tags WHERE full_path = ?1 \
                UNION ALL \
                SELECT child.id FROM tags child \
                JOIN descendants d ON child.parent_tag_id = d.id\
            ) {} {} \
            JOIN entry_tag_assignments eta ON eta.entry_id = e.id \
            JOIN descendants d ON eta.tag_id = d.id \
            WHERE 1=1",
            ENTRY_SELECT_COLS, ENTRY_FROM_JOINS,
        );
    } else {
        sql = format!(
            "{} {} WHERE e.parent_entry_id IS NULL",
            ENTRY_SELECT_COLS, ENTRY_FROM_JOINS,
        );
    }

    if let Some(q) = &query.q {
        let term = format!("%{}%", q.to_lowercase());
        let n = params.len() + 1;
        sql.push_str(&format!(
            " AND (LOWER(e.title) LIKE ?{n} OR LOWER(si.canonical_url) LIKE ?{n} \
             OR LOWER(e.entry_uid) LIKE ?{n} OR LOWER(e.source_kind) LIKE ?{n} \
             OR LOWER(e.entity_kind) LIKE ?{n} OR LOWER(e.visibility) LIKE ?{n})"
        ));
        params.push(term);
    }
    if let Some(sk) = &query.source_kind {
        let n = params.len() + 1;
        sql.push_str(&format!(" AND e.source_kind = ?{n}"));
        params.push(sk.clone());
    }
    if let Some(ek) = &query.entity_kind {
        let n = params.len() + 1;
        sql.push_str(&format!(" AND e.entity_kind = ?{n}"));
        params.push(ek.clone());
    }
    if let Some(u) = &query.url {
        let n = params.len() + 1;
        sql.push_str(&format!(" AND LOWER(si.canonical_url) LIKE ?{n}"));
        params.push(format!("%{}%", u.to_lowercase()));
    }
    if let Some(t) = &query.title {
        let n = params.len() + 1;
        sql.push_str(&format!(" AND LOWER(e.title) LIKE ?{n}"));
        params.push(format!("%{}%", t.to_lowercase()));
    }
    if let Some(a) = &query.after {
        let n = params.len() + 1;
        sql.push_str(&format!(" AND e.archived_at >= ?{n}"));
        params.push(a.clone());
    }
    if let Some(b) = &query.before {
        let n = params.len() + 1;
        sql.push_str(&format!(" AND e.archived_at < ?{n}"));
        params.push(b.clone());
    }

    // Visibility filter
    let n = params.len() + 1;
    sql.push_str(&format!(
        " AND (CAST(?{n} AS INTEGER) & 12 != 0 \
         OR EXISTS (SELECT 1 FROM collection_entries ce \
         WHERE ce.entry_id = e.id AND ce.visibility_bits & CAST(?{n} AS INTEGER) != 0))"
    ));
    params.push(query.caller_bits.to_string());

    sql.push_str(" GROUP BY e.id ORDER BY e.archived_at DESC, e.id DESC");

    let mut stmt = conn.prepare(&sql)?;
    let entries = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok(EntrySummary {
                entry_uid: row.get(0)?,
                archived_at: row.get(1)?,
                source_kind: row.get(2)?,
                entity_kind: row.get(3)?,
                title: row.get(4)?,
                visibility: row.get(5)?,
                original_url: row.get(6)?,
                artifact_count: row.get(7)?,
                total_artifact_bytes: row.get(8)?,
                parent_entry_uid: row.get(9)?,
                has_favicon: row.get::<_, i64>(10)? != 0,
                cached_bytes: row.get(11)?,
                child_count: row.get(12)?,
                cacheable_bytes: row.get(13)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(entries)
}

fn tag_by_id(conn: &rusqlite::Connection, id: i64) -> Result<Tag> {
    conn.query_row(
        "SELECT tag_uid, name, slug, full_path FROM tags WHERE id = ?1",
        [id],
        |row| {
            Ok(Tag {
                tag_uid: row.get(0)?,
                name: row.get(1)?,
                slug: row.get(2)?,
                full_path: row.get(3)?,
            })
        },
    )
    .context("tag not found by id")
}

/// Creates all tag path segments (idempotent) and returns the leaf `Tag`.
pub fn create_tag(conn: &rusqlite::Connection, full_path: &str) -> Result<Tag> {
    let id = database::create_tag_path(conn, full_path)?;
    tag_by_id(conn, id)
}

/// Returns the full tag tree with root nodes at the top level and children nested.
/// Each node includes a direct entry count and a subtree count (unique entries
/// assigned to the tag itself or any descendant).
pub fn list_tag_tree(conn: &rusqlite::Connection) -> Result<Vec<TagNode>> {
    use std::collections::HashMap;

    let records = database::list_all_tags(conn)?;

    // Fetch direct entry counts for all tags in a single query.
    let mut counts: HashMap<String, i64> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT t.tag_uid, COUNT(eta.entry_id) \
             FROM tags t \
             LEFT JOIN entry_tag_assignments eta ON eta.tag_id = t.id \
             GROUP BY t.id",
        )?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
            let (uid, cnt) = row?;
            counts.insert(uid, cnt);
        }
    }

    // Fetch subtree entry counts: for each tag, count distinct entries assigned
    // to it or any descendant.  COUNT(DISTINCT) ensures an entry tagged at both
    // a parent and a child is counted only once.
    let mut subtree_counts: HashMap<String, i64> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "WITH RECURSIVE descendants(ancestor_id, descendant_id) AS ( \
                 SELECT id, id FROM tags \
                 UNION ALL \
                 SELECT d.ancestor_id, t.id \
                 FROM tags t JOIN descendants d ON t.parent_tag_id = d.descendant_id \
             ) \
             SELECT t.tag_uid, COUNT(DISTINCT eta.entry_id) \
             FROM tags t \
             JOIN descendants d ON d.ancestor_id = t.id \
             LEFT JOIN entry_tag_assignments eta ON eta.tag_id = d.descendant_id \
             GROUP BY t.id",
        )?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
            let (uid, cnt) = row?;
            subtree_counts.insert(uid, cnt);
        }
    }

    let mut by_parent: HashMap<Option<i64>, Vec<database::TagRecord>> = HashMap::new();
    for record in records {
        by_parent
            .entry(record.parent_tag_id)
            .or_default()
            .push(record);
    }

    fn build_nodes(
        parent_id: Option<i64>,
        by_parent: &HashMap<Option<i64>, Vec<database::TagRecord>>,
        counts: &HashMap<String, i64>,
        subtree_counts: &HashMap<String, i64>,
    ) -> Vec<TagNode> {
        let Some(children) = by_parent.get(&parent_id) else {
            return Vec::new();
        };
        children
            .iter()
            .map(|r| TagNode {
                tag: Tag {
                    tag_uid: r.tag_uid.clone(),
                    name: r.name.clone(),
                    slug: r.slug.clone(),
                    full_path: r.full_path.clone(),
                },
                entry_count: counts.get(&r.tag_uid).copied().unwrap_or(0),
                subtree_count: subtree_counts.get(&r.tag_uid).copied().unwrap_or(0),
                children: build_nodes(Some(r.id), by_parent, counts, subtree_counts),
            })
            .collect()
    }

    Ok(build_nodes(None, &by_parent, &counts, &subtree_counts))
}

/// Returns the tags assigned to an entry.
///
/// Returns `Ok(None)` if the entry_uid does not exist (caller maps to 404).
/// Returns `Ok(Some([]))` if the entry exists but has no tags.
pub fn get_entry_tags(conn: &rusqlite::Connection, entry_uid: &str) -> Result<Option<Vec<Tag>>> {
    let Some(entry_id) = conn
        .query_row(
            "SELECT id FROM archived_entries WHERE entry_uid = ?1",
            [entry_uid],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
    else {
        return Ok(None);
    };
    let records = database::list_tags_for_entry(conn, entry_id)?;
    Ok(Some(
        records
            .into_iter()
            .map(|r| Tag {
                tag_uid: r.tag_uid,
                name: r.name,
                slug: r.slug,
                full_path: r.full_path,
            })
            .collect(),
    ))
}

/// Assigns a tag (by full path, creating it if needed) to an entry.
///
/// Returns `Ok(None)` if the entry_uid does not exist.
/// Returns `Ok(Some(tag))` on success.
pub fn assign_entry_tag(
    conn: &rusqlite::Connection,
    entry_uid: &str,
    tag_full_path: &str,
) -> Result<Option<Tag>> {
    let Some(entry_id) = conn
        .query_row(
            "SELECT id FROM archived_entries WHERE entry_uid = ?1",
            [entry_uid],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
    else {
        return Ok(None);
    };
    let tag_id = database::create_tag_path(conn, tag_full_path)?;
    database::assign_entry_to_tag(conn, entry_id, tag_id)?;
    Ok(Some(tag_by_id(conn, tag_id)?))
}

/// Removes a tag assignment from an entry.
///
/// Returns `Ok(false)` if either the entry_uid or tag_uid is not found.
/// Returns `Ok(true)` on success (even if no row was deleted, i.e. assignment didn't exist).
pub fn remove_entry_tag(
    conn: &rusqlite::Connection,
    entry_uid: &str,
    tag_uid: &str,
) -> Result<bool> {
    let Some(entry_id) = conn
        .query_row(
            "SELECT id FROM archived_entries WHERE entry_uid = ?1",
            [entry_uid],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
    else {
        return Ok(false);
    };
    let Some(tag_record) = database::get_tag_by_uid(conn, tag_uid)? else {
        return Ok(false);
    };
    database::remove_entry_tag_assignment(conn, entry_id, tag_record.id)?;
    Ok(true)
}

/// Returns all entries (root and child) assigned to any tag in the subtree rooted at `tag_full_path`.
pub fn entries_for_tag(
    conn: &rusqlite::Connection,
    tag_full_path: &str,
) -> Result<Vec<EntrySummary>> {
    let mut stmt = conn.prepare(
        "WITH RECURSIVE descendants(id) AS (
             SELECT id FROM tags WHERE full_path = ?1
             UNION ALL
             SELECT child.id FROM tags child
             JOIN descendants d ON child.parent_tag_id = d.id
         )
         SELECT e.entry_uid, e.archived_at, e.source_kind, e.entity_kind, e.title,
                e.visibility, si.canonical_url, COUNT(ea.id) AS artifact_count,
                COALESCE(SUM(b.byte_size), 0) AS total_artifact_bytes,
                parent.entry_uid AS parent_entry_uid,
                EXISTS(SELECT 1 FROM entry_artifacts fav WHERE fav.entry_id = e.id AND fav.artifact_role = 'favicon') AS has_favicon,
                e.cached_bytes,
                (SELECT COUNT(*) FROM archived_entries child WHERE child.parent_entry_id = e.id) AS child_count,
                COALESCE(SUM(CASE WHEN ea.artifact_role != 'avatar' THEN b.byte_size ELSE 0 END), 0) AS cacheable_bytes
         FROM archived_entries e
         JOIN source_identities si ON si.id = e.source_identity_id
         LEFT JOIN entry_artifacts ea ON ea.entry_id = e.id
         LEFT JOIN blobs b ON b.id = ea.blob_id
         LEFT JOIN archived_entries parent ON parent.id = e.parent_entry_id
         JOIN entry_tag_assignments eta ON eta.entry_id = e.id
         JOIN descendants d ON eta.tag_id = d.id
         GROUP BY e.id
         ORDER BY e.archived_at DESC, e.id DESC",
    )?;
    let entries = stmt
        .query_map([tag_full_path], |row| {
            Ok(EntrySummary {
                entry_uid: row.get(0)?,
                archived_at: row.get(1)?,
                source_kind: row.get(2)?,
                entity_kind: row.get(3)?,
                title: row.get(4)?,
                visibility: row.get(5)?,
                original_url: row.get(6)?,
                artifact_count: row.get(7)?,
                total_artifact_bytes: row.get(8)?,
                parent_entry_uid: row.get(9)?,
                has_favicon: row.get::<_, i64>(10)? != 0,
                cached_bytes: row.get(11)?,
                child_count: row.get(12)?,
                cacheable_bytes: row.get(13)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_path(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!("{prefix}-{nanos}-{}", std::process::id()))
    }

    #[test]
    fn find_archive_path_walks_up_to_dot_archivr() {
        let root = unique_path("archivr-core-find");
        let nested = root.join("a").join("b");
        fs::create_dir_all(root.join(".archivr")).unwrap();
        fs::create_dir_all(&nested).unwrap();

        let found = find_archive_path_from(&nested).unwrap().unwrap();

        assert_eq!(found, root.join(".archivr"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn read_archive_paths_returns_name_and_store_path() {
        let root = unique_path("archivr-core-open");
        let archive_path = root.join(".archivr");
        let store_path = root.join("store");
        fs::create_dir_all(&archive_path).unwrap();
        fs::create_dir_all(&store_path).unwrap();
        fs::write(archive_path.join("name"), "Personal").unwrap();
        fs::write(
            archive_path.join("store_path"),
            store_path.display().to_string(),
        )
        .unwrap();

        let paths = read_archive_paths(&archive_path).unwrap();

        assert_eq!(paths.archive_path, archive_path);
        assert_eq!(paths.store_path, store_path);
        assert_eq!(paths.name, "Personal");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn initialize_archive_creates_database_store_and_metadata() {
        let root = unique_path("archivr-core-init");
        let archive_parent = root.join("archive");
        let store_path = root.join("store");

        let paths = initialize_archive(&archive_parent, &store_path, "Personal", false).unwrap();

        assert_eq!(paths.archive_path, archive_parent.join(".archivr"));
        assert!(
            paths
                .archive_path
                .join(database::DATABASE_FILE_NAME)
                .is_file()
        );
        assert!(paths.store_path.join("raw").is_dir());
        assert!(paths.store_path.join("raw_tweets").is_dir());
        assert!(paths.store_path.join("structured").is_dir());
        assert!(paths.store_path.join("temp").is_dir());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn list_root_entries_returns_entry_details_and_runs() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        database::initialize_schema(&conn).unwrap();
        let user_id = database::ensure_default_user(&conn).unwrap();
        let run = database::create_archive_run(&conn, user_id, 1).unwrap();
        let item = database::create_archive_run_item(
            &conn,
            run.id,
            None,
            0,
            "https://example.com/saved",
            Some("https://example.com/saved"),
            "web",
            "page",
        )
        .unwrap();
        let source_identity_id = database::upsert_source_identity(
            &conn,
            "web",
            "page",
            Some("saved-article"),
            Some("https://example.com/saved"),
            "https://example.com/saved",
        )
        .unwrap();
        let entry = database::create_archived_entry(
            &conn,
            &database::NewEntry {
                source_identity_id,
                archive_run_id: run.id,
                parent_entry_id: None,
                root_entry_id: None,
                created_by_user_id: user_id,
                owned_by_user_id: user_id,
                source_kind: "web".to_string(),
                entity_kind: "page".to_string(),
                title: Some("Saved Article".to_string()),
                visibility: "private".to_string(),
                representation_kind: "html".to_string(),
                source_metadata_json: r#"{"source":"test"}"#.to_string(),
                display_metadata_json: Some(r#"{"reading_time":"4m"}"#.to_string()),
            },
        )
        .unwrap();
        let blob_id = database::upsert_blob(
            &conn,
            &database::BlobRecord {
                sha256: "abc123".to_string(),
                byte_size: 123,
                mime_type: Some("text/html".to_string()),
                extension: Some("html".to_string()),
                raw_relpath: "raw/a/b/abc123.html".to_string(),
            },
        )
        .unwrap();
        database::add_entry_artifact(
            &conn,
            &database::NewArtifact {
                entry_id: entry.id,
                artifact_role: "primary_media".to_string(),
                storage_area: "raw".to_string(),
                relpath: "raw/a/b/abc123.html".to_string(),
                blob_id: Some(blob_id),
                logical_path: None,
                metadata_json: None,
            },
        )
        .unwrap();
        database::complete_archive_run_item(&conn, item.id, entry.id).unwrap();
        database::finish_archive_run(&conn, run.id).unwrap();

        let entries = list_root_entries(&conn, u32::MAX).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title.as_deref(), Some("Saved Article"));
        assert_eq!(entries[0].artifact_count, 1);

        let detail = get_entry_detail(&conn, &entries[0].entry_uid)
            .unwrap()
            .unwrap();
        assert_eq!(detail.artifacts.len(), 1);
        assert_eq!(detail.artifacts[0].artifact_role, "primary_media");

        let runs = list_runs(&conn).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "completed");
    }

    #[test]
    fn resolve_artifact_path_returns_absolute_path_within_store() {
        let root = unique_path("archivr-resolve-artifact");
        let store_path = root.join("store");
        fs::create_dir_all(store_path.join("raw/a/b")).unwrap();
        let artifact_file = store_path.join("raw/a/b/abc.pdf");
        fs::write(&artifact_file, b"data").unwrap();

        let artifact = EntryArtifactSummary {
            artifact_role: "primary".to_string(),
            storage_area: "raw".to_string(),
            relpath: "raw/a/b/abc.pdf".to_string(),
            byte_size: Some(4),
        };
        let resolved = resolve_artifact_path(&store_path, &artifact).unwrap();
        assert_eq!(resolved, artifact_file.canonicalize().unwrap());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn resolve_artifact_path_rejects_traversal() {
        let root = unique_path("archivr-resolve-traversal");
        let store_path = root.join("store");
        fs::create_dir_all(&store_path).unwrap();
        let artifact = EntryArtifactSummary {
            artifact_role: "primary".to_string(),
            storage_area: "raw".to_string(),
            relpath: "../escaped.txt".to_string(),
            byte_size: None,
        };
        assert!(resolve_artifact_path(&store_path, &artifact).is_err());
        let _ = fs::remove_dir_all(&root);
    }

    // ---- parse_search_query tests ----

    #[test]
    fn parse_empty_query_returns_default() {
        let q = parse_search_query("").unwrap();
        assert!(q.q.is_none());
        assert!(q.source_kind.is_none());
        assert!(q.entity_kind.is_none());
    }

    #[test]
    fn parse_plain_text_sets_q() {
        let q = parse_search_query("polymarket").unwrap();
        assert_eq!(q.q.as_deref(), Some("polymarket"));
    }

    #[test]
    fn parse_prefix_source_sets_source_kind() {
        let q = parse_search_query("source:x").unwrap();
        assert_eq!(q.source_kind.as_deref(), Some("x"));
        assert!(q.q.is_none());
    }

    #[test]
    fn parse_prefix_type_sets_entity_kind() {
        let q = parse_search_query("type:tweet").unwrap();
        assert_eq!(q.entity_kind.as_deref(), Some("tweet"));
    }

    #[test]
    fn parse_mixed_plain_and_prefix() {
        let q = parse_search_query("polymarket type:tweet").unwrap();
        assert_eq!(q.q.as_deref(), Some("polymarket"));
        assert_eq!(q.entity_kind.as_deref(), Some("tweet"));
    }

    #[test]
    fn parse_unknown_prefix_returns_err() {
        let result = parse_search_query("foo:bar");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "foo");
    }

    #[test]
    fn parse_after_before_dates() {
        let q = parse_search_query("after:2026-01-01 before:2026-04-01").unwrap();
        assert_eq!(q.after.as_deref(), Some("2026-01-01"));
        assert_eq!(q.before.as_deref(), Some("2026-04-01"));
    }

    #[test]
    fn parse_search_query_tag_prefix() {
        let q = parse_search_query("tag:/science/cs").unwrap();
        assert_eq!(q.tag.as_deref(), Some("/science/cs"));
        assert_eq!(q.q, None);
    }

    // ---- search_entries tests ----

    fn make_test_db_with_entries() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        database::initialize_schema(&conn).unwrap();
        let user_id = database::ensure_default_user(&conn).unwrap();
        let run = database::create_archive_run(&conn, user_id, 2).unwrap();

        // Entry 1: tweet by source x
        let si1 = database::upsert_source_identity(
            &conn,
            "x",
            "tweet",
            Some("t-1"),
            Some("https://x.com/user/status/1"),
            "https://x.com/user/status/1",
        )
        .unwrap();
        database::create_archived_entry(
            &conn,
            &database::NewEntry {
                source_identity_id: si1,
                archive_run_id: run.id,
                parent_entry_id: None,
                root_entry_id: None,
                created_by_user_id: user_id,
                owned_by_user_id: user_id,
                source_kind: "x".to_string(),
                entity_kind: "tweet".to_string(),
                title: Some("Polymarket tweet".to_string()),
                visibility: "private".to_string(),
                representation_kind: "json".to_string(),
                source_metadata_json: "{}".to_string(),
                display_metadata_json: None,
            },
        )
        .unwrap();

        // Entry 2: web page
        let si2 = database::upsert_source_identity(
            &conn,
            "web",
            "page",
            Some("page-1"),
            Some("https://medium.com/article"),
            "https://medium.com/article",
        )
        .unwrap();
        database::create_archived_entry(
            &conn,
            &database::NewEntry {
                source_identity_id: si2,
                archive_run_id: run.id,
                parent_entry_id: None,
                root_entry_id: None,
                created_by_user_id: user_id,
                owned_by_user_id: user_id,
                source_kind: "web".to_string(),
                entity_kind: "page".to_string(),
                title: Some("Resume Templates".to_string()),
                visibility: "private".to_string(),
                representation_kind: "html".to_string(),
                source_metadata_json: "{}".to_string(),
                display_metadata_json: None,
            },
        )
        .unwrap();

        conn
    }

    #[test]
    fn search_empty_query_returns_all_root_entries() {
        let conn = make_test_db_with_entries();
        let all = list_root_entries(&conn, u32::MAX).unwrap();
        let searched = search_entries(&conn, &SearchEntriesQuery::default()).unwrap();
        assert_eq!(all.len(), searched.len());
    }

    #[test]
    fn search_q_filters_on_title() {
        let conn = make_test_db_with_entries();
        let results = search_entries(
            &conn,
            &SearchEntriesQuery {
                q: Some("polymarket".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entity_kind, "tweet");
    }

    #[test]
    fn search_entity_kind_exact_match() {
        let conn = make_test_db_with_entries();
        let results = search_entries(
            &conn,
            &SearchEntriesQuery {
                entity_kind: Some("page".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source_kind, "web");
    }

    #[test]
    fn search_url_like_filter() {
        let conn = make_test_db_with_entries();
        let results = search_entries(
            &conn,
            &SearchEntriesQuery {
                url: Some("medium.com".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title.as_deref(), Some("Resume Templates"));
    }

    #[test]
    fn search_no_match_returns_empty() {
        let conn = make_test_db_with_entries();
        let results = search_entries(
            &conn,
            &SearchEntriesQuery {
                q: Some("zzznonexistent".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn search_multiple_filters_compound() {
        let conn = make_test_db_with_entries();
        let results = search_entries(
            &conn,
            &SearchEntriesQuery {
                source_kind: Some("x".to_string()),
                entity_kind: Some("tweet".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(results.len(), 1);
    }

    // ---- tag API tests ----

    fn make_tag_test_db() -> (rusqlite::Connection, i64, i64) {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        database::initialize_schema(&conn).unwrap();
        let user_id = database::ensure_default_user(&conn).unwrap();
        let run = database::create_archive_run(&conn, user_id, 2).unwrap();
        (conn, user_id, run.id)
    }

    fn make_entry_in_db(
        conn: &rusqlite::Connection,
        user_id: i64,
        run_id: i64,
        parent_entry_id: Option<i64>,
        root_entry_id: Option<i64>,
        title: &str,
        url: &str,
    ) -> database::ArchivedEntry {
        let si =
            database::upsert_source_identity(conn, "web", "page", None, Some(url), url).unwrap();
        database::create_archived_entry(
            conn,
            &database::NewEntry {
                source_identity_id: si,
                archive_run_id: run_id,
                parent_entry_id,
                root_entry_id,
                created_by_user_id: user_id,
                owned_by_user_id: user_id,
                source_kind: "web".to_string(),
                entity_kind: "page".to_string(),
                title: Some(title.to_string()),
                visibility: "private".to_string(),
                representation_kind: "html".to_string(),
                source_metadata_json: "{}".to_string(),
                display_metadata_json: None,
            },
        )
        .unwrap()
    }

    #[test]
    fn tag_tree_roots_and_children() {
        let (conn, _, _) = make_tag_test_db();
        create_tag(&conn, "/science/cs").unwrap();
        create_tag(&conn, "/art").unwrap();

        let tree = list_tag_tree(&conn).unwrap();
        assert_eq!(tree.len(), 2, "expected two root nodes");

        let science = tree
            .iter()
            .find(|n| n.tag.slug == "science")
            .expect("science root missing");
        assert_eq!(science.children.len(), 1, "science should have one child");
        assert_eq!(science.children[0].tag.slug, "cs");

        let art = tree
            .iter()
            .find(|n| n.tag.slug == "art")
            .expect("art root missing");
        assert!(art.children.is_empty(), "art should have no children");
    }

    #[test]
    fn tag_tree_entry_counts_direct_and_subtree() {
        let (conn, user_id, run_id) = make_tag_test_db();
        // Tree: /science -> /science/cs -> /science/cs/algorithms
        create_tag(&conn, "/science/cs/algorithms").unwrap();

        let e1 = make_entry_in_db(
            &conn,
            user_id,
            run_id,
            None,
            None,
            "E1",
            "https://example.com/e1",
        );
        let e2 = make_entry_in_db(
            &conn,
            user_id,
            run_id,
            None,
            None,
            "E2",
            "https://example.com/e2",
        );

        // e1 → /science/cs/algorithms (leaf), e2 → /science (root)
        assign_entry_tag(&conn, &e1.entry_uid, "/science/cs/algorithms").unwrap();
        assign_entry_tag(&conn, &e2.entry_uid, "/science").unwrap();

        let tree = list_tag_tree(&conn).unwrap();
        let science = tree.iter().find(|n| n.tag.slug == "science").unwrap();
        let cs = science
            .children
            .iter()
            .find(|n| n.tag.slug == "cs")
            .unwrap();
        let algo = cs
            .children
            .iter()
            .find(|n| n.tag.slug == "algorithms")
            .unwrap();

        assert_eq!(science.entry_count, 1, "science direct = 1");
        assert_eq!(
            science.subtree_count, 2,
            "science subtree = 2 (e1 via algo, e2 direct)"
        );
        assert_eq!(cs.entry_count, 0, "cs direct = 0");
        assert_eq!(cs.subtree_count, 1, "cs subtree = 1 (e1 via algo)");
        assert_eq!(algo.entry_count, 1, "algo direct = 1");
        assert_eq!(algo.subtree_count, 1, "algo subtree = 1");
    }

    #[test]
    fn tag_tree_subtree_count_deduplicates_shared_entry() {
        // Regression: an entry assigned to both a parent and a child must count
        // as 1 in the parent's subtree_count, not 2.
        let (conn, user_id, run_id) = make_tag_test_db();
        create_tag(&conn, "/science/cs").unwrap();

        let e = make_entry_in_db(
            &conn,
            user_id,
            run_id,
            None,
            None,
            "E",
            "https://example.com/ded",
        );
        assign_entry_tag(&conn, &e.entry_uid, "/science").unwrap();
        assign_entry_tag(&conn, &e.entry_uid, "/science/cs").unwrap();

        let tree = list_tag_tree(&conn).unwrap();
        let science = tree.iter().find(|n| n.tag.slug == "science").unwrap();

        assert_eq!(
            science.subtree_count, 1,
            "entry assigned to both parent and child must not be double-counted"
        );
        assert_eq!(science.entry_count, 1, "science direct = 1");
        assert_eq!(science.children[0].subtree_count, 1, "cs subtree = 1");
    }

    #[test]
    fn assign_entry_tag_is_idempotent() {
        let (conn, user_id, run_id) = make_tag_test_db();
        let entry = make_entry_in_db(
            &conn,
            user_id,
            run_id,
            None,
            None,
            "Test",
            "https://example.com/t1",
        );

        assign_entry_tag(&conn, &entry.entry_uid, "/science").unwrap();
        assign_entry_tag(&conn, &entry.entry_uid, "/science").unwrap();

        let tags = get_entry_tags(&conn, &entry.entry_uid).unwrap().unwrap();
        assert_eq!(
            tags.len(),
            1,
            "idempotent assign should yield exactly one tag"
        );
    }

    #[test]
    fn remove_entry_tag_clears_assignment() {
        let (conn, user_id, run_id) = make_tag_test_db();
        let entry = make_entry_in_db(
            &conn,
            user_id,
            run_id,
            None,
            None,
            "Test",
            "https://example.com/t2",
        );

        let tag = assign_entry_tag(&conn, &entry.entry_uid, "/science")
            .unwrap()
            .unwrap();
        remove_entry_tag(&conn, &entry.entry_uid, &tag.tag_uid).unwrap();

        let tags = get_entry_tags(&conn, &entry.entry_uid).unwrap().unwrap();
        assert!(tags.is_empty(), "tag should be removed");
    }

    #[test]
    fn entries_for_tag_includes_descendants() {
        let (conn, user_id, run_id) = make_tag_test_db();
        let entry = make_entry_in_db(
            &conn,
            user_id,
            run_id,
            None,
            None,
            "Compilers Paper",
            "https://example.com/c1",
        );

        assign_entry_tag(&conn, &entry.entry_uid, "/science/cs/compilers").unwrap();

        let results = entries_for_tag(&conn, "/science").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry_uid, entry.entry_uid);
    }

    #[test]
    fn entries_for_tag_includes_child_entries() {
        let (conn, user_id, run_id) = make_tag_test_db();
        let parent = make_entry_in_db(
            &conn,
            user_id,
            run_id,
            None,
            None,
            "Playlist",
            "https://example.com/pl",
        );
        let child = make_entry_in_db(
            &conn,
            user_id,
            run_id,
            Some(parent.id),
            Some(parent.id),
            "Video 1",
            "https://example.com/pl/v1",
        );

        assign_entry_tag(&conn, &child.entry_uid, "/science").unwrap();

        let results = entries_for_tag(&conn, "/science").unwrap();
        assert_eq!(results.len(), 1, "only the tagged child should appear");
        assert_eq!(results[0].entry_uid, child.entry_uid);
        assert_eq!(
            results[0].parent_entry_uid.as_deref(),
            Some(parent.entry_uid.as_str())
        );
    }

    #[test]
    fn search_with_tag_filter_works() {
        let (conn, user_id, run_id) = make_tag_test_db();
        let entry = make_entry_in_db(
            &conn,
            user_id,
            run_id,
            None,
            None,
            "Science Article",
            "https://example.com/s1",
        );

        assign_entry_tag(&conn, &entry.entry_uid, "/science").unwrap();

        let results = search_entries(
            &conn,
            &SearchEntriesQuery {
                tag: Some("/science".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].entry_uid, entry.entry_uid);

        let empty = search_entries(
            &conn,
            &SearchEntriesQuery {
                tag: Some("/art".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(empty.is_empty(), "no entries under /art");
    }

    #[test]
    fn search_without_tag_returns_roots_only() {
        let (conn, user_id, run_id) = make_tag_test_db();
        let parent = make_entry_in_db(
            &conn,
            user_id,
            run_id,
            None,
            None,
            "Parent",
            "https://example.com/par",
        );
        let _child = make_entry_in_db(
            &conn,
            user_id,
            run_id,
            Some(parent.id),
            Some(parent.id),
            "Child",
            "https://example.com/par/c",
        );

        let results = search_entries(&conn, &SearchEntriesQuery::default()).unwrap();
        assert_eq!(
            results.len(),
            1,
            "plain search should return root entries only"
        );
        assert_eq!(results[0].entry_uid, parent.entry_uid);
    }
    // ── sync helper tests ────────────────────────────────────────────────────────

    #[test]
    fn find_container_entry_id_returns_none_when_absent() {
        let (conn, _, _) = make_tag_test_db();
        let result = find_container_entry_id_by_canonical_url(
            &conn,
            "https://www.youtube.com/playlist?list=PLnobody",
        )
        .unwrap();
        assert!(result.is_none(), "should return None when no entry exists");
    }

    #[test]
    fn find_container_entry_id_returns_root_entry() {
        let (conn, user_id, run_id) = make_tag_test_db();
        let playlist_url = "https://www.youtube.com/playlist?list=PLtest";
        let container = make_entry_in_db(&conn, user_id, run_id, None, None, "My Playlist", playlist_url);
        let result = find_container_entry_id_by_canonical_url(&conn, playlist_url)
            .unwrap()
            .expect("should find the container");
        assert_eq!(result, container.id);
    }

    #[test]
    fn find_container_entry_id_ignores_child_entries() {
        let (conn, user_id, run_id) = make_tag_test_db();
        let child_url = "https://www.youtube.com/watch?v=abc123";
        // Create a parent container and a child whose canonical URL happens to be
        // what we're querying — should NOT be returned since it has parent_entry_id set.
        let parent = make_entry_in_db(&conn, user_id, run_id, None, None, "PL", "https://example.com/pl");
        let _child = make_entry_in_db(&conn, user_id, run_id, Some(parent.id), Some(parent.id), "Vid", child_url);
        let result = find_container_entry_id_by_canonical_url(&conn, child_url).unwrap();
        assert!(result.is_none(), "child entry should not be returned as a container");
    }

    #[test]
    fn get_archived_playlist_child_urls_empty_when_no_playlist() {
        let (conn, _, _) = make_tag_test_db();
        let urls = get_archived_playlist_child_urls(
            &conn,
            "https://www.youtube.com/playlist?list=PLnone",
        )
        .unwrap();
        assert!(urls.is_empty());
    }

    #[test]
    fn get_archived_playlist_child_urls_returns_children() {
        let (conn, user_id, run_id) = make_tag_test_db();
        let playlist_url = "https://www.youtube.com/playlist?list=PLchildren";
        let container = make_entry_in_db(&conn, user_id, run_id, None, None, "Playlist", playlist_url);

        let child1_url = "https://www.youtube.com/watch?v=vid1";
        let child2_url = "https://www.youtube.com/watch?v=vid2";
        make_entry_in_db(&conn, user_id, run_id, Some(container.id), Some(container.id), "Vid 1", child1_url);
        make_entry_in_db(&conn, user_id, run_id, Some(container.id), Some(container.id), "Vid 2", child2_url);

        let urls = get_archived_playlist_child_urls(&conn, playlist_url).unwrap();
        assert_eq!(urls.len(), 2);
        assert!(urls.contains(child1_url), "should contain vid1");
        assert!(urls.contains(child2_url), "should contain vid2");
    }

    #[test]
    fn get_archived_playlist_child_urls_excludes_other_playlists() {
        let (conn, user_id, run_id) = make_tag_test_db();
        let pl_a = "https://www.youtube.com/playlist?list=PLA";
        let pl_b = "https://www.youtube.com/playlist?list=PLB";
        let container_a = make_entry_in_db(&conn, user_id, run_id, None, None, "PL A", pl_a);
        let container_b = make_entry_in_db(&conn, user_id, run_id, None, None, "PL B", pl_b);

        let vid_a = "https://www.youtube.com/watch?v=forA";
        let vid_b = "https://www.youtube.com/watch?v=forB";
        make_entry_in_db(&conn, user_id, run_id, Some(container_a.id), Some(container_a.id), "A Vid", vid_a);
        make_entry_in_db(&conn, user_id, run_id, Some(container_b.id), Some(container_b.id), "B Vid", vid_b);

        let urls_a = get_archived_playlist_child_urls(&conn, pl_a).unwrap();
        assert_eq!(urls_a.len(), 1);
        assert!(urls_a.contains(vid_a));
        assert!(!urls_a.contains(vid_b), "should not include children of other playlists");
    }

    #[test]
    fn cached_bytes_excludes_avatar_blobs() {
        let (conn, user_id, run_id) = make_tag_test_db();

        // entry_a is older (created first, gets the lower id)
        let entry_a = make_entry_in_db(
            &conn,
            user_id,
            run_id,
            None,
            None,
            "Tweet A",
            "https://twitter.com/user/status/1",
        );
        // entry_b is newer (created second, higher id — tiebreak on id when archived_at is equal)
        let entry_b = make_entry_in_db(
            &conn,
            user_id,
            run_id,
            None,
            None,
            "Tweet B",
            "https://twitter.com/user/status/2",
        );

        // Shared avatar blob: 100 bytes
        let avatar_blob_id = database::upsert_blob(
            &conn,
            &database::BlobRecord {
                sha256: "avatar_sha256_test".to_string(),
                byte_size: 100,
                mime_type: Some("image/jpeg".to_string()),
                extension: Some("jpg".to_string()),
                raw_relpath: "raw/av/at/avatar.jpg".to_string(),
            },
        )
        .unwrap();

        // Shared media blob: 900 bytes
        let media_blob_id = database::upsert_blob(
            &conn,
            &database::BlobRecord {
                sha256: "media_sha256_test".to_string(),
                byte_size: 900,
                mime_type: Some("image/png".to_string()),
                extension: Some("png".to_string()),
                raw_relpath: "raw/me/di/media.png".to_string(),
            },
        )
        .unwrap();

        // Attach both artifacts to entry_a
        database::add_entry_artifact(
            &conn,
            &database::NewArtifact {
                entry_id: entry_a.id,
                artifact_role: "avatar".to_string(),
                storage_area: "raw".to_string(),
                relpath: "raw/av/at/avatar.jpg".to_string(),
                blob_id: Some(avatar_blob_id),
                logical_path: None,
                metadata_json: None,
            },
        )
        .unwrap();
        database::add_entry_artifact(
            &conn,
            &database::NewArtifact {
                entry_id: entry_a.id,
                artifact_role: "primary_media".to_string(),
                storage_area: "raw".to_string(),
                relpath: "raw/me/di/media.png".to_string(),
                blob_id: Some(media_blob_id),
                logical_path: None,
                metadata_json: None,
            },
        )
        .unwrap();

        // Attach both artifacts to entry_b (same blobs — simulates shared avatar/media)
        database::add_entry_artifact(
            &conn,
            &database::NewArtifact {
                entry_id: entry_b.id,
                artifact_role: "avatar".to_string(),
                storage_area: "raw".to_string(),
                relpath: "raw/av/at/avatar.jpg".to_string(),
                blob_id: Some(avatar_blob_id),
                logical_path: None,
                metadata_json: None,
            },
        )
        .unwrap();
        database::add_entry_artifact(
            &conn,
            &database::NewArtifact {
                entry_id: entry_b.id,
                artifact_role: "primary_media".to_string(),
                storage_area: "raw".to_string(),
                relpath: "raw/me/di/media.png".to_string(),
                blob_id: Some(media_blob_id),
                logical_path: None,
                metadata_json: None,
            },
        )
        .unwrap();

        // Recompute cached_bytes for entry_b.
        // The query excludes avatar-role artifacts and counts only non-avatar blobs
        // that are already held by an earlier entry.  entry_a (lower id) owns the
        // same media blob, so cached_bytes for entry_b should be 900, not 1000.
        database::refresh_entry_cached_bytes(&conn, entry_b.id).unwrap();

        // --- Assert raw DB value ---
        let cached_bytes_db: i64 = conn
            .query_row(
                "SELECT cached_bytes FROM archived_entries WHERE id = ?1",
                [entry_b.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            cached_bytes_db, 900,
            "cached_bytes should be 900 (media only); avatar blob must be excluded"
        );

        // --- Assert list_root_entries summary for entry_b ---
        let entries = list_root_entries(&conn, 12).unwrap(); // 12 = ADMIN bits
        let summary_b = entries
            .iter()
            .find(|e| e.entry_uid == entry_b.entry_uid)
            .expect("entry_b must appear in list_root_entries");

        assert_eq!(
            summary_b.cached_bytes, 900,
            "summary cached_bytes must equal 900 (precomputed, avatar excluded)"
        );
        assert_eq!(
            summary_b.cacheable_bytes, 900,
            "cacheable_bytes (non-avatar total) must be 900 for entry_b"
        );
        assert_eq!(
            summary_b.total_artifact_bytes, 1000,
            "total_artifact_bytes must be 1000 (avatar 100 + media 900)"
        );
    }

}
