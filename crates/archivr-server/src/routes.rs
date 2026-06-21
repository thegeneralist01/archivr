use std::{path::PathBuf, sync::Arc};

use archivr_core::{archive, database};
use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use tower_http::services::{ServeDir, ServeFile};
use tower::ServiceExt;

use crate::registry::{MountedArchive, ServerRegistry};

#[derive(Clone)]
pub struct AppState {
    registry: Arc<ServerRegistry>,
}

pub fn app(registry: ServerRegistry) -> Router {
    let state = AppState {
        registry: Arc::new(registry),
    };
    let static_dir = static_dir();

    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/api/archives", get(list_archives))
        .route("/api/archives/:archive_id/entries", get(list_entries))
        .route(
            "/api/archives/:archive_id/entries/:entry_uid",
            get(entry_detail),
        )
        .route(
            "/api/archives/:archive_id/entries/:entry_uid/artifacts/:artifact_index",
            get(serve_artifact),
        )
        .route("/api/archives/:archive_id/runs", get(list_runs))
        .nest_service("/assets", ServeDir::new(&static_dir))
        .fallback_service(ServeFile::new(static_dir.join("index.html")))
        .with_state(state)
}

fn static_dir() -> PathBuf {
    std::env::var_os("ARCHIVR_STATIC_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("static"))
}

async fn list_archives(State(state): State<AppState>) -> Json<Vec<MountedArchive>> {
    Json(state.registry.archives.clone())
}

async fn list_entries(
    State(state): State<AppState>,
    Path(archive_id): Path<String>,
) -> Result<Json<Vec<archive::EntrySummary>>, ApiError> {
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    Ok(Json(archive::list_root_entries(&conn)?))
}

async fn entry_detail(
    State(state): State<AppState>,
    Path((archive_id, entry_uid)): Path<(String, String)>,
) -> Result<Json<archive::EntryDetail>, ApiError> {
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    let detail = archive::get_entry_detail(&conn, &entry_uid)?
        .ok_or(ApiError::not_found("entry not found"))?;
    Ok(Json(detail))
}

async fn list_runs(
    State(state): State<AppState>,
    Path(archive_id): Path<String>,
) -> Result<Json<Vec<archive::RunSummary>>, ApiError> {
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    Ok(Json(archive::list_runs(&conn)?))
}

async fn serve_artifact(
    State(state): State<AppState>,
    Path((archive_id, entry_uid, artifact_index)): Path<(String, String, usize)>,
    req: Request,
) -> Result<Response, ApiError> {
    let mounted = mounted_archive(&state, &archive_id)?;
    let paths = archive::read_archive_paths(&mounted.archive_path)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    let detail = archive::get_entry_detail(&conn, &entry_uid)?
        .ok_or(ApiError::not_found("entry not found"))?;
    let artifact = detail
        .artifacts
        .get(artifact_index)
        .ok_or(ApiError::not_found("artifact index out of range"))?;
    let file_path = archive::resolve_artifact_path(&paths.store_path, artifact)?;
    // ServeFile streams the file, handles Range requests (video seeking),
    // sets Content-Type/ETag/Last-Modified. Error type is Infallible.
    Ok(ServeFile::new(&file_path)
        .oneshot(req)
        .await
        .unwrap()
        .into_response())
}

fn mounted_archive<'a>(
    state: &'a AppState,
    archive_id: &str,
) -> Result<&'a MountedArchive, ApiError> {
    state
        .registry
        .archives
        .iter()
        .find(|archive| archive.id == archive_id)
        .ok_or(ApiError::not_found("archive not found"))
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn not_found(message: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.to_string(),
        }
    }
}

impl<E> From<E> for ApiError
where
    E: Into<anyhow::Error>,
{
    fn from(error: E) -> Self {
        let error = error.into();
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("{error:#}"),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, self.message).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn archives_endpoint_lists_mounted_archives() {
        let registry = ServerRegistry {
            archives: vec![MountedArchive {
                id: "personal".to_string(),
                label: "Personal".to_string(),
                archive_path: std::path::PathBuf::from("/tmp/personal/.archivr"),
            }],
        };
        let response = app(registry)
            .oneshot(
                Request::builder()
                    .uri("/api/archives")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn missing_archive_returns_404() {
        let response = app(ServerRegistry::default())
            .oneshot(
                Request::builder()
                    .uri("/api/archives/missing/entries")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn artifact_missing_archive_returns_404() {
        let response = app(ServerRegistry::default())
            .oneshot(
                Request::builder()
                    .uri("/api/archives/nope/entries/entry_abc/artifacts/0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn artifact_missing_entry_returns_404() {
        let dir = tempfile::tempdir().unwrap();
        archivr_core::archive::initialize_archive(
            dir.path(),
            &dir.path().join("store"),
            "test",
            false,
        )
        .unwrap();
        let archive_path = dir.path().join(".archivr");
        let registry = ServerRegistry {
            archives: vec![MountedArchive {
                id: "test".to_string(),
                label: "Test".to_string(),
                archive_path,
            }],
        };
        let response = app(registry)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/entries/entry_doesnotexist/artifacts/0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn artifact_out_of_range_index_returns_404() {
        let dir = tempfile::tempdir().unwrap();
        archivr_core::archive::initialize_archive(
            dir.path(),
            &dir.path().join("store"),
            "test",
            false,
        )
        .unwrap();
        let archive_path = dir.path().join(".archivr");
        let registry = ServerRegistry {
            archives: vec![MountedArchive {
                id: "test".to_string(),
                label: "Test".to_string(),
                archive_path,
            }],
        };
        let response = app(registry)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/entries/entry_doesnotexist/artifacts/99")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn artifact_serves_file_with_ok_status() {
        // Initialize archive (creates .archivr dir, store dirs, and db)
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().join("store");
        let paths =
            archivr_core::archive::initialize_archive(dir.path(), &store_path, "test", false)
                .unwrap();

        // Write artifact file to the store
        let artifact_relpath = "raw/a/b/test.html";
        let artifact_dir = store_path.join("raw").join("a").join("b");
        std::fs::create_dir_all(&artifact_dir).unwrap();
        std::fs::write(artifact_dir.join("test.html"), b"<html>hello</html>").unwrap();

        // Populate the database with user, source identity, run, entry, blob, artifact
        let conn = database::open_or_initialize(&paths.archive_path).unwrap();
        let user_id = database::ensure_default_user(&conn).unwrap();
        let source_identity_id = database::upsert_source_identity(
            &conn,
            "web",
            "page",
            Some("test-page"),
            Some("https://example.com/page"),
            "https://example.com/page",
        )
        .unwrap();
        let run = database::create_archive_run(&conn, user_id, 1).unwrap();
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
                title: Some("Test Page".to_string()),
                visibility: "private".to_string(),
                representation_kind: "html".to_string(),
                source_metadata_json: r#"{"source":"test"}"#.to_string(),
                display_metadata_json: None,
            },
        )
        .unwrap();
        let blob_id = database::upsert_blob(
            &conn,
            &database::BlobRecord {
                sha256: "abc123testblob".to_string(),
                byte_size: 18,
                mime_type: Some("text/html".to_string()),
                extension: Some("html".to_string()),
                raw_relpath: artifact_relpath.to_string(),
            },
        )
        .unwrap();
        database::add_entry_artifact(
            &conn,
            &database::NewArtifact {
                entry_id: entry.id,
                artifact_role: "primary_media".to_string(),
                storage_area: "raw".to_string(),
                relpath: artifact_relpath.to_string(),
                blob_id: Some(blob_id),
                logical_path: None,
                metadata_json: None,
            },
        )
        .unwrap();
        drop(conn); // release before the HTTP handler opens the same db file

        let registry = ServerRegistry {
            archives: vec![MountedArchive {
                id: "test".to_string(),
                label: "Test".to_string(),
                archive_path: paths.archive_path.clone(),
            }],
        };
        let uri = format!(
            "/api/archives/test/entries/{}/artifacts/0",
            entry.entry_uid
        );
        let response = app(registry)
            .oneshot(Request::builder().uri(&uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
