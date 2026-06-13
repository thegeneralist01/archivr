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

pub fn list_root_entries(conn: &rusqlite::Connection) -> Result<Vec<EntrySummary>> {
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
            COALESCE(SUM(b.byte_size), 0) AS total_artifact_bytes
         FROM archived_entries e
         JOIN source_identities si ON si.id = e.source_identity_id
         LEFT JOIN entry_artifacts ea ON ea.entry_id = e.id
         LEFT JOIN blobs b ON b.id = ea.blob_id
         WHERE e.parent_entry_id IS NULL
         GROUP BY e.id
         ORDER BY e.archived_at DESC, e.id DESC",
    )?;

    let entries = stmt
        .query_map([], |row| {
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
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(entries)
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

    let summary = list_root_entries(conn)?
        .into_iter()
        .find(|entry| entry.entry_uid == entry_uid)
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

        let entries = list_root_entries(&conn).unwrap();
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
}
