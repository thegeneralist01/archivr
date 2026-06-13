use std::{path::PathBuf, sync::Arc};

use archivr_core::{archive, database};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use tower_http::services::{ServeDir, ServeFile};

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
}
