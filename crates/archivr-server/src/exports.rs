//! `GET /api/archives/:archive_id/export` — generate and stream an entry export.

use axum::{
    Router,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
    routing::get,
};
use tokio_util::io::ReaderStream;

use archivr_core::{archive, database, export};

use crate::routes::{ApiError, AppState, AuthUser, auth_to_caller_bits, ensure_entry_visible, mounted_archive};

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/archives/:archive_id/export",
        get(export_handler),
    )
}

#[derive(Debug, serde::Deserialize)]
struct ExportQuery {
    entries: String,
    format: String,
    layout: Option<String>,
    credit: Option<bool>,
}

async fn export_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(archive_id): Path<String>,
    Query(q): Query<ExportQuery>,
) -> Result<impl IntoResponse, ApiError> {
    // 1. Require authentication.
    if matches!(auth_user, AuthUser::Guest) {
        return Err(ApiError::unauthorized("login required"));
    }

    // 2. Parse format, layout, and credit.
    let format = export::ExportFormat::parse(&q.format)
        .ok_or_else(|| ApiError::bad_request("format must be html or markdown"))?;

    let layout = match q.layout.as_deref().unwrap_or("together") {
        "together" => export::ExportLayout::Together,
        "separate" => export::ExportLayout::Separate,
        _ => return Err(ApiError::bad_request("layout must be together or separate")),
    };

    let include_credit = q.credit.unwrap_or(true);

    // 3. Parse and validate entry UIDs.
    let uids: Vec<String> = q
        .entries
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    if uids.is_empty() {
        return Err(ApiError::bad_request("entries must not be empty"));
    }
    if uids.len() > export::MAX_EXPORT_ENTRIES {
        return Err(ApiError::bad_request("select at most 100 entries"));
    }

    // 4. Resolve archive and verify entry visibility.
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    for uid in &uids {
        ensure_entry_visible(&conn, &auth_user, uid)?;
        database::entry_id_for_uid(&conn, uid)?
            .ok_or_else(|| ApiError::not_found("entry not found"))?;
    }
    drop(conn); // don't hold a connection across the blocking task

    let paths = archive::read_archive_paths(&mounted.archive_path)?;
    let caller_bits = auth_to_caller_bits(&auth_user);
    let options = export::ExportOptions {
        format,
        layout,
        include_credit,
        caller_bits,
    };

    // 5. Run the (synchronous) export in a blocking thread.
    let export_file = tokio::task::spawn_blocking(move || {
        export::export_entries(&paths, &uids, &options)
    })
    .await
    .map_err(|e| {
        eprintln!("warn: export: task panicked: {e:#}");
        ApiError::internal("export task failed")
    })?
    .map_err(|e| {
        eprintln!("warn: export: {e:#}");
        ApiError::internal(&format!("{e:#}"))
    })?;

    // 6. Stream the file.
    let (std_file, len, filename, content_type) = export_file.open().map_err(|e| {
        eprintln!("warn: export: failed to open result: {e:#}");
        ApiError::internal("failed to open export file")
    })?;
    let async_file = tokio::fs::File::from_std(std_file);
    let stream = ReaderStream::new(async_file);
    let body = Body::from_stream(stream);

    let disposition = format!("attachment; filename=\"{filename}\"");
    let response = Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, len)
        .header(header::CACHE_CONTROL, "no-store")
        .header(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_str(&disposition)
                .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
        )
        .body(body)
        .map_err(|_| ApiError::internal("failed to build response"))?;

    Ok(response)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use archivr_core::{archive, database};
    use axum::http::StatusCode;

    use crate::test_support::{fixture, make_test_session, send};

    /// Insert a minimal text entry and return its `entry_uid`.
    fn seed_entry(archive_path: &std::path::Path) -> String {
        let paths = archive::read_archive_paths(archive_path).unwrap();
        let conn = database::open_or_initialize(archive_path).unwrap();
        let user_id = database::ensure_default_user(&conn).unwrap();
        let run = database::create_archive_run(&conn, user_id, 1).unwrap();
        let si = database::upsert_source_identity(
            &conn, "text", "document", Some("te1"), None, "text:e:1",
        ).unwrap();
        let entry = database::create_archived_entry(
            &conn,
            &database::NewEntry {
                source_identity_id: si,
                archive_run_id: run.id,
                parent_entry_id: None,
                root_entry_id: None,
                created_by_user_id: user_id,
                owned_by_user_id: user_id,
                source_kind: "text".to_string(),
                entity_kind: "document".to_string(),
                title: Some("Export Test".to_string()),
                visibility: "public".to_string(),
                representation_kind: "text".to_string(),
                source_metadata_json: "{}".to_string(),
                display_metadata_json: None,
            },
        ).unwrap();
        let relpath = "raw/e/1/export_test.txt".to_string();
        let fpath = paths.store_path.join(&relpath);
        std::fs::create_dir_all(fpath.parent().unwrap()).unwrap();
        std::fs::write(&fpath, b"hello export").unwrap();
        database::add_entry_artifact(
            &conn,
            &database::NewArtifact {
                entry_id: entry.id,
                artifact_role: "primary_media".to_string(),
                storage_area: "raw".to_string(),
                relpath,
                blob_id: None,
                logical_path: None,
                metadata_json: None,
            },
        ).unwrap();
        entry.entry_uid
    }

    #[tokio::test]
    async fn no_cookie_returns_401() {
        let f = fixture();
        let uid = seed_entry(&f.archive_path);
        let (status, _) = send(
            &f.router,
            "GET",
            &format!("/api/archives/test/export?entries={uid}&format=html"),
            None,
            None,
        ).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn visible_entry_returns_zip() {
        let f = fixture();
        let uid = seed_entry(&f.archive_path);
        let cookie = make_test_session(&f.auth_path);
        let (status, bytes) = send(
            &f.router,
            "GET",
            &format!("/api/archives/test/export?entries={uid}&format=html"),
            Some(&cookie),
            None,
        ).await;
        assert_eq!(status, StatusCode::OK, "expected 200; body: {:?}", String::from_utf8_lossy(&bytes));
        let cursor = std::io::Cursor::new(bytes);
        zip::ZipArchive::new(cursor).expect("response should be a valid ZIP");
    }

    #[tokio::test]
    async fn invalid_format_returns_400() {
        let f = fixture();
        let uid = seed_entry(&f.archive_path);
        let cookie = make_test_session(&f.auth_path);
        let (status, _) = send(
            &f.router,
            "GET",
            &format!("/api/archives/test/export?entries={uid}&format=docx"),
            Some(&cookie),
            None,
        ).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn too_many_entries_returns_400() {
        let f = fixture();
        let cookie = make_test_session(&f.auth_path);
        let entries: Vec<String> = (0..101).map(|i| format!("uid-{i}")).collect();
        let (status, _) = send(
            &f.router,
            "GET",
            &format!("/api/archives/test/export?entries={}&format=html", entries.join(",")),
            Some(&cookie),
            None,
        ).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}
