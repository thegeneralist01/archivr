// ── Security Boundary ────────────────────────────────────────────────────────
// All routes are currently trusted-local: no authentication or authorization
// middleware is applied. The server is designed to bind on 127.0.0.1 only.
//
// Route classification (for when middleware is added later):
//
//   STATIC  — safe to expose publicly: GET / and static /assets/*
//   READ    — safe to expose read-only: GET /health
//                                       GET /api/archives
//                                       GET /api/archives/:id/entries
//                                       GET /api/archives/:id/entries/search
//                                       GET /api/archives/:id/entries/:uid
//                                       GET /api/archives/:id/entries/:uid/artifacts/:idx
//                                       GET /api/archives/:id/runs
//                                       GET /api/archives/:id/tags
//   ADMIN   — requires auth if ever public: GET /api/admin/archives
//   WRITE   — requires auth if ever public: POST /api/archives/:id/captures
//                                           POST /api/archives/:id/tags
//                                           PUT  /api/archives/:id/tags/:tag_id
//                                           DELETE /api/archives/:id/tags/:tag_id
//
// Do not add middleware here until the auth model is chosen. See docs/README.md.
// ─────────────────────────────────────────────────────────────────────────────

use std::{path::PathBuf, sync::Arc};

use archivr_core::{archive, capture, database};
use axum::{
    Json, Router,
    extract::{Path, Query, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use tower_http::services::{ServeDir, ServeFile};
use tower::ServiceExt;

use crate::registry::{MountedArchive, ServerRegistry};

#[derive(Clone)]
pub struct AppState {
    registry: Arc<ServerRegistry>,
}

#[derive(Debug, serde::Deserialize, Default)]
pub struct EntrySearchParams {
    pub q: Option<String>,
    pub tag: Option<String>,
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
        .route("/api/archives/:archive_id/entries/search", get(search_entries_handler))
        .route(
            "/api/archives/:archive_id/entries/:entry_uid",
            get(entry_detail),
        )
        .route(
            "/api/archives/:archive_id/entries/:entry_uid/artifacts/:artifact_index",
            get(serve_artifact),
        )
        .route(
            "/api/archives/:archive_id/entries/:entry_uid/favicon",
            get(serve_entry_favicon),
        )
        .route(
            "/api/archives/:archive_id/blobs/:sha256",
            get(serve_blob),
        )
        .route("/api/archives/:archive_id/runs", get(list_runs))
        .route("/api/archives/:archive_id/captures", post(capture_handler))
        .route("/api/archives/:archive_id/tags", get(list_tags).post(create_tag_handler))
        .route(
            "/api/archives/:archive_id/entries/:entry_uid/tags",
            get(list_entry_tags).post(assign_entry_tag_handler),
        )
        .route(
            "/api/archives/:archive_id/entries/:entry_uid/tags/:tag_uid",
            delete(remove_entry_tag_handler),
        )
        .nest_service("/assets", ServeDir::new(static_dir.join("assets")))
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

async fn search_entries_handler(
    State(state): State<AppState>,
    Path(archive_id): Path<String>,
    Query(params): Query<EntrySearchParams>,
) -> Result<Json<Vec<archive::EntrySummary>>, ApiError> {
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    let raw = params.q.as_deref().unwrap_or("");
    let mut search_query = archive::parse_search_query(raw)
        .map_err(|prefix| ApiError::bad_request(&format!("unknown search prefix: {prefix}")))?;
    if let Some(tag) = params.tag {
        search_query.tag = Some(tag);
    }
    Ok(Json(archive::search_entries(&conn, &search_query)?))
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

async fn serve_entry_favicon(
    State(state): State<AppState>,
    Path((archive_id, entry_uid)): Path<(String, String)>,
    req: Request,
) -> Result<Response, ApiError> {
    let mounted = mounted_archive(&state, &archive_id)?;
    let paths = archive::read_archive_paths(&mounted.archive_path)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    let detail = archive::get_entry_detail(&conn, &entry_uid)?
        .ok_or(ApiError::not_found("entry not found"))?;
    let artifact = detail
        .artifacts
        .iter()
        .find(|a| a.artifact_role == "favicon")
        .ok_or(ApiError::not_found("no favicon for this entry"))?;
    let file_path = archive::resolve_artifact_path(&paths.store_path, artifact)?;
    Ok(ServeFile::new(&file_path)
        .oneshot(req)
        .await
        .unwrap()
        .into_response())
}
async fn serve_blob(
    State(state): State<AppState>,
    Path((archive_id, sha256)): Path<(String, String)>,
    req: Request,
) -> Result<Response, ApiError> {
    let mounted = mounted_archive(&state, &archive_id)?;
    let paths = archive::read_archive_paths(&mounted.archive_path)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;

    let blob = database::get_blob_by_sha256(&conn, &sha256)?
        .ok_or(ApiError::not_found("blob not found"))?;

    let file_path = paths.store_path.join(&blob.raw_relpath);

    // Path-traversal guard: resolved path must stay inside store_path.
    let canonical_file = file_path
        .canonicalize()
        .map_err(|_| ApiError::not_found("blob file not found"))?;
    let canonical_store = paths
        .store_path
        .canonicalize()
        .map_err(|_| ApiError::internal("invalid store path"))?;
    if !canonical_file.starts_with(&canonical_store) {
        return Err(ApiError::not_found("blob not found"));
    }

    Ok(ServeFile::new(&canonical_file)
        .oneshot(req)
        .await
        .unwrap()
        .into_response())
}

#[derive(Debug, serde::Deserialize)]
struct CreateTagBody {
    path: String,
}

#[derive(Debug, serde::Deserialize)]
struct AssignTagBody {
    tag_path: String,
}

async fn list_tags(
    State(state): State<AppState>,
    Path(archive_id): Path<String>,
) -> Result<Json<Vec<archive::TagNode>>, ApiError> {
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    Ok(Json(archive::list_tag_tree(&conn)?))
}

async fn create_tag_handler(
    State(state): State<AppState>,
    Path(archive_id): Path<String>,
    Json(body): Json<CreateTagBody>,
) -> Result<(StatusCode, Json<archive::Tag>), ApiError> {
    if body.path.trim().is_empty() {
        return Err(ApiError::bad_request("tag path must not be empty"));
    }
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    let tag = archive::create_tag(&conn, &body.path)?;
    Ok((StatusCode::CREATED, Json(tag)))
}

async fn list_entry_tags(
    State(state): State<AppState>,
    Path((archive_id, entry_uid)): Path<(String, String)>,
) -> Result<Json<Vec<archive::Tag>>, ApiError> {
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    match archive::get_entry_tags(&conn, &entry_uid)? {
        Some(tags) => Ok(Json(tags)),
        None => Err(ApiError::not_found("entry not found")),
    }
}

async fn assign_entry_tag_handler(
    State(state): State<AppState>,
    Path((archive_id, entry_uid)): Path<(String, String)>,
    Json(body): Json<AssignTagBody>,
) -> Result<(StatusCode, Json<archive::Tag>), ApiError> {
    if body.tag_path.trim().is_empty() {
        return Err(ApiError::bad_request("tag_path must not be empty"));
    }
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    match archive::assign_entry_tag(&conn, &entry_uid, &body.tag_path)? {
        Some(tag) => Ok((StatusCode::CREATED, Json(tag))),
        None => Err(ApiError::not_found("entry not found")),
    }
}

async fn remove_entry_tag_handler(
    State(state): State<AppState>,
    Path((archive_id, entry_uid, tag_uid)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    if archive::remove_entry_tag(&conn, &entry_uid, &tag_uid)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("entry or tag not found"))
    }
}

#[derive(Debug, serde::Deserialize)]
struct CaptureBody {
    locator: String,
}

async fn capture_handler(
    State(state): State<AppState>,
    Path(archive_id): Path<String>,
    Json(body): Json<CaptureBody>,
) -> Result<Json<capture::CaptureResult>, ApiError> {
    if body.locator.trim().is_empty() {
        return Err(ApiError::bad_request("locator must not be empty"));
    }
    let mounted = mounted_archive(&state, &archive_id)?;
    let archive_paths = archive::read_archive_paths(&mounted.archive_path)
        .map_err(ApiError::from)?;
    let result = capture::perform_capture(&archive_paths, &body.locator, Some(&archive_id))
        .map_err(ApiError::from)?;
    Ok(Json(result))
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

    fn bad_request(message: &str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.to_string(),
        }
    }

    fn internal(message: &str) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
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
            bind: None,
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
            bind: None,
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
            bind: None,
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
            bind: None,
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

    #[tokio::test]
    async fn search_missing_archive_returns_404() {
        let response = app(ServerRegistry::default())
            .oneshot(
                Request::builder()
                    .uri("/api/archives/nope/entries/search?q=anything")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn search_empty_q_returns_ok() {
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
            bind: None,
        };
        let response = app(registry)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/entries/search")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn search_unknown_prefix_returns_400() {
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
            bind: None,
        };
        let response = app(registry)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/entries/search?q=unknownprefix%3Aval")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    // ---- helpers ----

    fn make_test_registry(dir: &tempfile::TempDir) -> (ServerRegistry, std::path::PathBuf) {
        let paths = archivr_core::archive::initialize_archive(
            dir.path(),
            &dir.path().join("store"),
            "test",
            false,
        )
        .unwrap();
        let registry = ServerRegistry {
            archives: vec![MountedArchive {
                id: "test".to_string(),
                label: "Test".to_string(),
                archive_path: paths.archive_path.clone(),
            }],
            bind: None,
        };
        (registry, paths.archive_path)
    }

    fn make_test_entry(archive_path: &std::path::Path) -> archivr_core::database::ArchivedEntry {
        let conn = database::open_or_initialize(archive_path).unwrap();
        let user_id = database::ensure_default_user(&conn).unwrap();
        let run = database::create_archive_run(&conn, user_id, 1).unwrap();
        let si = database::upsert_source_identity(
            &conn, "web", "page", None,
            Some("https://example.com/test"),
            "https://example.com/test",
        )
        .unwrap();
        database::create_archived_entry(
            &conn,
            &database::NewEntry {
                source_identity_id: si,
                archive_run_id: run.id,
                parent_entry_id: None,
                root_entry_id: None,
                created_by_user_id: user_id,
                owned_by_user_id: user_id,
                source_kind: "web".to_string(),
                entity_kind: "page".to_string(),
                title: Some("Test Entry".to_string()),
                visibility: "private".to_string(),
                representation_kind: "html".to_string(),
                source_metadata_json: "{}".to_string(),
                display_metadata_json: None,
            },
        )
        .unwrap()
    }

    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn json_body(payload: &serde_json::Value) -> Body {
        Body::from(serde_json::to_vec(payload).unwrap())
    }

    // ---- tag route tests ----

    #[tokio::test]
    async fn test_list_tags_unknown_archive() {
        let response = app(ServerRegistry::default())
            .oneshot(
                Request::builder()
                    .uri("/api/archives/ghost/tags")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_create_tag_unknown_archive() {
        let response = app(ServerRegistry::default())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/archives/ghost/tags")
                    .header("content-type", "application/json")
                    .body(json_body(&serde_json::json!({"path": "/science"})))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_create_tag_empty_path() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _) = make_test_registry(&dir);
        let response = app(registry)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/archives/test/tags")
                    .header("content-type", "application/json")
                    .body(json_body(&serde_json::json!({"path": ""})))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_tag_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _) = make_test_registry(&dir);

        let create_response = app(registry.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/archives/test/tags")
                    .header("content-type", "application/json")
                    .body(json_body(&serde_json::json!({"path": "/science"})))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create_response.status(), StatusCode::CREATED);

        let list_response = app(registry.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/tags")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list_response.status(), StatusCode::OK);
        let tree = body_json(list_response).await;
        let slugs: Vec<&str> = tree
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["tag"]["slug"].as_str().unwrap())
            .collect();
        assert!(slugs.contains(&"science"), "expected 'science' in tag tree, got {slugs:?}");
    }

    #[tokio::test]
    async fn test_entry_tag_assign_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path) = make_test_registry(&dir);
        let entry = make_test_entry(&archive_path);
        let entry_uid = entry.entry_uid.clone();
        let entry_tags_uri = format!("/api/archives/test/entries/{entry_uid}/tags");

        // Assign tag
        let assign_response = app(registry.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(&entry_tags_uri)
                    .header("content-type", "application/json")
                    .body(json_body(&serde_json::json!({"tag_path": "/science"})))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(assign_response.status(), StatusCode::CREATED);
        let assigned_tag = body_json(assign_response).await;
        let tag_uid = assigned_tag["tag_uid"].as_str().unwrap().to_string();

        // List entry tags — should contain the assigned tag
        let list_response = app(registry.clone())
            .oneshot(
                Request::builder()
                    .uri(&entry_tags_uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list_response.status(), StatusCode::OK);
        let tags = body_json(list_response).await;
        assert_eq!(tags.as_array().unwrap().len(), 1);

        // Remove tag
        let delete_uri = format!("{entry_tags_uri}/{tag_uid}");
        let delete_response = app(registry.clone())
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(&delete_uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(delete_response.status(), StatusCode::NO_CONTENT);

        // List entry tags again — should be empty
        let list2_response = app(registry.clone())
            .oneshot(
                Request::builder()
                    .uri(&entry_tags_uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list2_response.status(), StatusCode::OK);
        let tags2 = body_json(list2_response).await;
        assert!(tags2.as_array().unwrap().is_empty(), "tags should be empty after removal");
    }

    #[tokio::test]
    async fn test_search_with_tag_param() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path) = make_test_registry(&dir);
        let entry = make_test_entry(&archive_path);
        let entry_uid = entry.entry_uid.clone();

        // Assign /science tag to entry
        let assign_resp = app(registry.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/archives/test/entries/{entry_uid}/tags"))
                    .header("content-type", "application/json")
                    .body(json_body(&serde_json::json!({"tag_path": "/science"})))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(assign_resp.status(), StatusCode::CREATED, "assign tag should return 201");

        // Search with ?tag=/science — entry should appear
        let response = app(registry.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/entries/search?tag=%2Fscience")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let results = body_json(response).await;
        assert_eq!(
            results.as_array().unwrap().len(),
            1,
            "expected 1 result for /science tag, got {}",
            results.as_array().unwrap().len()
        );

        // Search with ?tag=/art — should return empty
        let response2 = app(registry.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/entries/search?tag=%2Fart")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response2.status(), StatusCode::OK);
        let results2 = body_json(response2).await;
        assert!(
            results2.as_array().unwrap().is_empty(),
            "expected 0 results for /art tag"
        );
    }

    #[tokio::test]
    async fn test_list_entry_tags_unknown_entry() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _) = make_test_registry(&dir);
        let response = app(registry)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/entries/ghost_uid/tags")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_assign_entry_tag_unknown_entry() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _) = make_test_registry(&dir);
        let response = app(registry)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/archives/test/entries/ghost_uid/tags")
                    .header("content-type", "application/json")
                    .body(json_body(&serde_json::json!({"tag_path": "/science"})))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_assign_entry_tag_empty_tag_path() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path) = make_test_registry(&dir);
        let entry = make_test_entry(&archive_path);
        let entry_uid = entry.entry_uid.clone();
        let response = app(registry)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/archives/test/entries/{entry_uid}/tags"))
                    .header("content-type", "application/json")
                    .body(json_body(&serde_json::json!({"tag_path": ""})))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_remove_entry_tag_unknown_entry() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _) = make_test_registry(&dir);
        let response = app(registry)
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/archives/test/entries/ghost_uid/tags/ghost_tag_uid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn capture_rejects_empty_locator() {
        let response = app(ServerRegistry::default())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/archives/test/captures")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"locator":""}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn capture_rejects_unknown_archive() {
        let response = app(ServerRegistry::default())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/archives/nonexistent/captures")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"locator":"tweet:1234567890"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_blob_returns_404_for_unknown_sha256() {
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
            bind: None,
        };
        let response = app(registry)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/blobs/0000000000000000000000000000000000000000000000000000000000000000")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

}
