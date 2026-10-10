//! Capture-job listing and scoping queries (`created_by` filters, per-job items).
use anyhow::Result;
use rusqlite::Connection;

use crate::archive::CaptureJobSummary;

/// Valid `capture_jobs.status` values (the table's CHECK constraint).
pub const JOB_STATUSES: [&str; 4] = ["pending", "running", "completed", "failed"];
/// Default and maximum page size for `list_capture_jobs`.
pub const JOB_LIST_DEFAULT_LIMIT: i64 = 50;
pub const JOB_LIST_MAX_LIMIT: i64 = 200;
/// Maximum number of per-item rows returned by `list_job_items`.
pub const JOB_ITEMS_MAX: usize = 200;

/// Lists capture jobs newest first (`created_at DESC, id DESC`).
///
/// `created_by = Some(uid)` restricts to jobs submitted by that user (this is how a
/// non-admin is scoped; it also excludes NULL-owner rows); `None` returns every job.
/// `limit` is clamped to 1..=200, `offset` to >= 0.
pub fn list_capture_jobs(
    conn: &Connection,
    status: Option<&str>,
    created_by: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Vec<CaptureJobSummary>> {
    let limit = limit.clamp(1, JOB_LIST_MAX_LIMIT);
    let offset = offset.max(0);
    let mut stmt = conn.prepare(
        "SELECT job_uid, archive_id, run_uid, status, error_text, notes_json,
                created_at, updated_at, created_by
         FROM capture_jobs
         WHERE (?1 IS NULL OR status = ?1)
           AND (?2 IS NULL OR created_by = ?2)
         ORDER BY created_at DESC, id DESC
         LIMIT ?3 OFFSET ?4",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![status, created_by, limit, offset], |row| {
            Ok(CaptureJobSummary {
                job_uid: row.get(0)?,
                archive_id: row.get(1)?,
                run_uid: row.get(2)?,
                status: row.get(3)?,
                error_text: row.get(4)?,
                notes_json: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
                created_by: row.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// One `archive_run_items` row of a job's run.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct JobItem {
    pub item_uid: String,
    pub ordinal: i64,
    pub requested_locator: String,
    pub canonical_locator: Option<String>,
    pub source_kind: String,
    pub entity_kind: String,
    pub status: String,
    pub error_text: Option<String>,
    /// Produced entry (`produced_entry_id` resolved to its `entry_uid`).
    pub entry_uid: Option<String>,
}

/// Items and produced entries of a job's run.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct JobItems {
    /// De-duplicated non-null `entry_uid`s over *all* items, in item order.
    pub entry_uids: Vec<String>,
    /// At most `JOB_ITEMS_MAX` items ordered by `ordinal`.
    pub items: Vec<JobItem>,
    pub items_truncated: bool,
}

/// Items of the run `run_uid` (empty when the run is unknown). Mirrors the spec's J2.
pub fn list_job_items(conn: &Connection, run_uid: &str) -> Result<JobItems> {
    let mut stmt = conn.prepare(
        "SELECT i.item_uid, i.ordinal, i.requested_locator, i.canonical_locator,
                i.source_kind, i.entity_kind, i.status, i.error_text, e.entry_uid
         FROM archive_run_items i
         JOIN archive_runs r ON r.id = i.run_id
         LEFT JOIN archived_entries e ON e.id = i.produced_entry_id
         WHERE r.run_uid = ?1
         ORDER BY i.ordinal ASC, i.id ASC",
    )?;
    let all = stmt
        .query_map([run_uid], |row| {
            Ok(JobItem {
                item_uid: row.get(0)?,
                ordinal: row.get(1)?,
                requested_locator: row.get(2)?,
                canonical_locator: row.get(3)?,
                source_kind: row.get(4)?,
                entity_kind: row.get(5)?,
                status: row.get(6)?,
                error_text: row.get(7)?,
                entry_uid: row.get(8)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut entry_uids: Vec<String> = Vec::new();
    for uid in all.iter().filter_map(|i| i.entry_uid.as_ref()) {
        if !entry_uids.contains(uid) {
            entry_uids.push(uid.clone());
        }
    }
    let items_truncated = all.len() > JOB_ITEMS_MAX;
    let items = all.into_iter().take(JOB_ITEMS_MAX).collect();
    Ok(JobItems {
        entry_uids,
        items,
        items_truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database;

    fn conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        database::initialize_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn list_filters_by_owner_status_and_pages() {
        let conn = conn();
        let a1 = database::create_capture_job_as(&conn, "t", Some("usr_a")).unwrap();
        let a2 = database::create_capture_job_as(&conn, "t", Some("usr_a")).unwrap();
        let _b = database::create_capture_job_as(&conn, "t", Some("usr_b")).unwrap();
        let _legacy = database::create_capture_job(&conn, "t").unwrap();
        database::update_capture_job_status(&conn, &a1, "failed", None, Some("x"), None).unwrap();

        assert_eq!(list_capture_jobs(&conn, None, None, 50, 0).unwrap().len(), 4);
        let mine = list_capture_jobs(&conn, None, Some("usr_a"), 50, 0).unwrap();
        assert_eq!(mine.len(), 2);
        assert!(mine.iter().all(|j| j.created_by.as_deref() == Some("usr_a")));
        let failed = list_capture_jobs(&conn, Some("failed"), Some("usr_a"), 50, 0).unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].job_uid, a1);
        // newest first, offset pages through
        let page = list_capture_jobs(&conn, None, Some("usr_a"), 1, 0).unwrap();
        assert_eq!(page[0].job_uid, a2);
        let page2 = list_capture_jobs(&conn, None, Some("usr_a"), 1, 1).unwrap();
        assert_eq!(page2[0].job_uid, a1);
        // limit is clamped to >= 1
        assert_eq!(list_capture_jobs(&conn, None, None, 0, 0).unwrap().len(), 1);
    }

    #[test]
    fn job_items_resolve_entry_uids_and_dedupe() {
        let conn = conn();
        let user_id = database::ensure_default_user(&conn).unwrap();
        let run = database::create_archive_run(&conn, user_id, 1).unwrap();
        for ordinal in 0..2 {
            database::create_archive_run_item(
                &conn,
                run.id,
                None,
                ordinal,
                "text:x",
                None,
                "text",
                "text",
            )
            .unwrap();
        }
        let items = list_job_items(&conn, &run.run_uid).unwrap();
        assert_eq!(items.items.len(), 2);
        assert!(items.entry_uids.is_empty());
        assert!(!items.items_truncated);
        assert!(list_job_items(&conn, "run_missing").unwrap().items.is_empty());
    }
}
