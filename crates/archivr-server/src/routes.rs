// ── Security Boundary ──────────────────────────────────────────────────────────────────
// setup_guard middleware returns 503 for all non-auth routes until POST /api/auth/setup
// creates the owner account.
//
// Route protection tiers:
//   STATIC      — no auth: GET /, GET /assets/*
//   PUBLIC_READ — no auth (visibility filtering deferred to Track 6):
//                   GET /api/archives, GET /api/archives/:id/entries, etc.
//   AUTH        — requires login (ROLE_USER bit):
//                   POST /api/archives/:id/captures
//                   POST /api/archives/:id/tags
//                   POST/DELETE /api/archives/:id/entries/:uid/tags
//                   PATCH /api/archives/:id/entries/:uid
//   ADMIN       — requires ROLE_ADMIN: (future)
//   OWNER       — requires ROLE_OWNER: (future)
//   AUTH_SELF   — own resources, require_auth() only:
//                   GET/POST/DELETE /api/auth/tokens
//                   POST /api/auth/logout, GET/PATCH /api/auth/me
//   SETTINGS    — instance settings, require ROLE_ADMIN:
//                   GET/PATCH /api/admin/instance-settings
// ────────────────────────────────────────────────────────────────────────────

use parking_lot::Mutex;
use std::{
    collections::{HashMap, VecDeque},
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use archivr_core::{archive, capture, database, downloader};
use axum::{
    Json, Router,
    extract::{ConnectInfo, Path, Query, Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post},
};
use tower::ServiceExt;
use tower_http::services::{ServeDir, ServeFile};

use crate::auth;
pub use crate::auth::{AuthUser, ROLE_ADMIN, ROLE_GUEST, ROLE_OWNER, ROLE_USER};
use crate::registry::{MountedArchive, ServerRegistry};
use axum_extra::extract::CookieJar;
use rusqlite::OptionalExtension;

const LOGIN_WINDOW: Duration = Duration::from_secs(15 * 60);
const LOGIN_MAX_ATTEMPTS: usize = 5;

#[derive(Clone)]
pub struct AppState {
    registry: Arc<ServerRegistry>,
    pub auth_db_path: Arc<std::path::PathBuf>,
    pub login_attempts: Arc<Mutex<HashMap<IpAddr, VecDeque<Instant>>>>,
}

#[derive(Debug, serde::Deserialize, Default)]
pub struct EntrySearchParams {
    pub q: Option<String>,
    pub tag: Option<String>,
}

/// Tower middleware: returns 503 on all non-exempt routes if setup hasn't been completed.
async fn setup_guard(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_owned();
    let exempt = path.starts_with("/api/auth/")
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

/// Tower middleware: injects HTTP security response headers on every response.
/// HSTS is intentionally omitted — that belongs at the reverse-proxy layer.
async fn security_headers(req: Request, next: Next) -> Response {
    // Capture path before consuming req for next.run()
    let is_artifact = req.uri().path().contains("/artifacts/");
    let mut response = next.run(req).await;
    let headers = response.headers_mut();
    headers.insert(
        axum::http::header::HeaderName::from_static("x-content-type-options"),
        axum::http::HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        axum::http::header::HeaderName::from_static("referrer-policy"),
        axum::http::HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    headers.insert(
        axum::http::header::HeaderName::from_static("permissions-policy"),
        axum::http::HeaderValue::from_static(
            "camera=(), microphone=(), geolocation=(), autoplay=()",
        ),
    );
    if is_artifact {
        // Artifact responses are iframed by the preview modal (sandboxed, no allow-scripts).
        // When opened directly in a new tab scripts must still be blocked so archived
        // pages cannot make same-origin API calls with the user's session.
        // Only styles, images, fonts and media need to be relaxed for rendering.
        headers.insert(
            axum::http::header::HeaderName::from_static("content-security-policy"),
            axum::http::HeaderValue::from_static(
                "default-src 'none'; \
                 script-src 'none'; \
                 style-src 'self' 'unsafe-inline' https:; \
                 img-src 'self' data: blob: https:; \
                 font-src 'self' https:; \
                 media-src 'self' blob:; \
                 connect-src 'none'; \
                 frame-ancestors 'self'",
            ),
        );
    } else {
        headers.insert(
            axum::http::header::HeaderName::from_static("x-frame-options"),
            axum::http::HeaderValue::from_static("DENY"),
        );
        // Main app CSP — allow Google Fonts and external images for tweet previews
        headers.insert(
            axum::http::header::HeaderName::from_static("content-security-policy"),
            axum::http::HeaderValue::from_static(
                "default-src 'self'; \
                 script-src 'self'; \
                 style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; \
                 img-src 'self' data: blob: https:; \
                 font-src 'self' https://fonts.gstatic.com; \
                 media-src 'self' blob: https:; \
                 connect-src 'self'; \
                 frame-src 'self'; \
                 frame-ancestors 'none'",
            ),
        );
    }
    response
}

async fn login_rate_limit(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if req.method() != axum::http::Method::POST || req.uri().path() != "/api/auth/login" {
        return next.run(req).await;
    }
    let ip = extract_client_ip(&req);
    let retry_after = {
        let mut map = state.login_attempts.lock();
        let attempts = map.entry(ip).or_default();
        let now = Instant::now();
        attempts.retain(|t| now.duration_since(*t) < LOGIN_WINDOW);
        if attempts.len() >= LOGIN_MAX_ATTEMPTS {
            let oldest = *attempts.front().unwrap();
            let elapsed = now.duration_since(oldest).as_secs() as i64;
            let secs = (LOGIN_WINDOW.as_secs() as i64 - elapsed).max(1);
            Some(secs)
        } else {
            attempts.push_back(now);
            None
        }
    };
    if let Some(secs) = retry_after {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(
                axum::http::header::RETRY_AFTER,
                axum::http::HeaderValue::from_str(&secs.to_string()).unwrap(),
            )],
            axum::Json(serde_json::json!({
                "error": "rate_limited",
                "retry_after_secs": secs,
            })),
        )
            .into_response();
    }
    next.run(req).await
}

fn extract_client_ip(req: &Request) -> IpAddr {
    // Attempt to read the real peer address injected by
    // `into_make_service_with_connect_info` in main.rs.
    let peer_ip: Option<IpAddr> = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip());

    match peer_ip {
        // Peer is a loopback address → the connection came from a local
        // reverse proxy (nginx/caddy on the same host). Trust the last
        // address in X-Forwarded-For as the real client IP — the last entry
        // is always appended by the trusted proxy, even if the client sent a
        // spoofed value earlier in the chain.
        Some(peer) if peer.is_loopback() => req
            .headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split(',').last())
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(peer),

        // Peer is a real address → use it directly; ignoring X-Forwarded-For
        // prevents header-spoofing attacks.
        Some(peer) => peer,

        // No ConnectInfo present (unit tests using .oneshot() without a real
        // socket). Fall back to XFF for test compatibility.
        None => req
            .headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split(',').next())
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(IpAddr::from([127, 0, 0, 1])),
    }
}

/// Build the Axum router from a pre-constructed `AppState`.
/// Use this in tests that need to share state across multiple `oneshot` calls.
pub fn app_with_state(state: AppState) -> Router {
    let static_dir = static_dir();

    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/api/archives", get(list_archives))
        .route("/api/archives/:archive_id/entries", get(list_entries))
        .route(
            "/api/archives/:archive_id/entries/search",
            get(search_entries_handler),
        )
        .route(
            "/api/archives/:archive_id/entries/:entry_uid",
            get(entry_detail)
                .patch(patch_entry_handler)
                .delete(delete_entry_handler),
        )
        .route(
            "/api/archives/:archive_id/entries/:entry_uid/children",
            get(list_entry_children),
        )
        .route(
            "/api/archives/:archive_id/entries/:entry_uid/artifacts/:artifact_index",
            get(serve_artifact),
        )
        .route(
            "/api/archives/:archive_id/entries/:entry_uid/rearchive",
            post(rearchive_handler),
        )
        .route(
            "/api/archives/:archive_id/entries/:entry_uid/favicon",
            get(serve_entry_favicon),
        )
        .route("/api/archives/:archive_id/blobs/:sha256", get(serve_blob))
        .route("/api/archives/:archive_id/runs", get(list_runs))
        .route("/api/archives/:archive_id/captures", post(capture_handler))
        .route(
            "/api/archives/:archive_id/captures/probe",
            get(probe_handler),
        )
        .route(
            "/api/archives/:archive_id/captures/probe-playlist",
            post(probe_playlist_handler),
        )
        .route(
            "/api/archives/:archive_id/capture_jobs/:job_uid",
            get(get_capture_job_handler),
        )
        .route(
            "/api/archives/:archive_id/tags",
            get(list_tags).post(create_tag_handler),
        )
        .route(
            "/api/archives/:archive_id/tags/:tag_uid",
            patch(patch_tag_handler).delete(delete_tag_handler),
        )
        .route(
            "/api/archives/:archive_id/tags/:tag_uid/move",
            post(move_tag_handler),
        )
        .route(
            "/api/archives/:archive_id/entries/:entry_uid/tags",
            get(list_entry_tags).post(assign_entry_tag_handler),
        )
        .route(
            "/api/archives/:archive_id/entries/:entry_uid/tags/:tag_uid",
            delete(remove_entry_tag_handler),
        )
        .route(
            "/api/auth/setup",
            axum::routing::get(auth_setup_status).post(auth_setup),
        )
        .route("/api/auth/login", axum::routing::post(auth_login))
        .route("/api/auth/logout", axum::routing::post(auth_logout))
        .route("/api/auth/me", axum::routing::get(auth_me).patch(patch_me))
        .route(
            "/api/auth/tokens",
            axum::routing::get(list_tokens).post(create_token),
        )
        .route(
            "/api/auth/tokens/:token_uid",
            axum::routing::delete(delete_token),
        )
        .route(
            "/api/admin/users",
            get(admin_list_users).post(admin_create_user),
        )
        .route(
            "/api/admin/users/:uid/status",
            axum::routing::patch(admin_set_user_status),
        )
        .route(
            "/api/admin/users/:uid/roles",
            axum::routing::post(admin_assign_role),
        )
        .route(
            "/api/admin/users/:uid/roles/:role_slug",
            axum::routing::delete(admin_remove_role),
        )
        .route(
            "/api/admin/roles",
            get(admin_list_roles).post(admin_create_role),
        )
        .route(
            "/api/admin/instance-settings",
            get(get_instance_settings_handler).patch(update_instance_settings_handler),
        )
        .route(
            "/api/admin/cookie-rules",
            get(list_cookie_rules_handler).post(create_cookie_rule_handler),
        )
        .route(
            "/api/admin/cookie-rules/:rule_uid",
            patch(update_cookie_rule_handler).delete(delete_cookie_rule_handler),
        )
        .route(
            "/api/archives/:archive_id/collections",
            get(list_collections_handler).post(create_collection_handler),
        )
        .route(
            "/api/archives/:archive_id/collections/:coll_uid",
            get(get_collection_handler)
                .patch(patch_collection_handler)
                .delete(delete_collection_handler),
        )
        .route(
            "/api/archives/:archive_id/collections/:coll_uid/entries",
            post(add_entry_to_collection_handler),
        )
        .route(
            "/api/archives/:archive_id/collections/:coll_uid/entries/:entry_uid",
            delete(remove_entry_from_collection_handler).patch(update_entry_visibility_handler),
        )
        .route(
            "/api/archives/:archive_id/entries/:entry_uid/collections",
            get(list_entry_collections_handler),
        )
        .route(
            "/api/archives/:archive_id/blob-cleanup",
            get(blob_cleanup_scan_handler).delete(blob_cleanup_delete_handler),
        )
        .route("/api/util/resolve-tco", post(resolve_tco_handler))
        .nest_service("/assets", ServeDir::new(static_dir.join("assets")))
        .fallback_service(ServeFile::new(static_dir.join("index.html")))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            setup_guard,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            login_rate_limit,
        ))
        .layer(axum::middleware::from_fn(security_headers))
        .with_state(state)
}

/// Build the Axum router, constructing `AppState` from the given registry and auth DB path.
pub fn app(registry: ServerRegistry, auth_db_path: std::path::PathBuf) -> Router {
    let state = AppState {
        registry: Arc::new(registry),
        auth_db_path: Arc::new(auth_db_path),
        login_attempts: Arc::new(Mutex::new(HashMap::new())),
    };
    app_with_state(state)
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
    auth: AuthUser,
    Path(archive_id): Path<String>,
) -> Result<Json<Vec<archive::EntrySummary>>, ApiError> {
    auth.require_auth()?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    let caller_bits = auth_to_caller_bits(&auth);
    Ok(Json(archive::list_root_entries(&conn, caller_bits)?))
}

async fn list_entry_children(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((archive_id, entry_uid)): Path<(String, String)>,
) -> Result<Json<Vec<archive::EntrySummary>>, ApiError> {
    auth.require_auth()?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    let caller_bits = auth_to_caller_bits(&auth);
    Ok(Json(archive::list_child_entries(&conn, &entry_uid, caller_bits)?))
}

async fn search_entries_handler(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(archive_id): Path<String>,
    Query(params): Query<EntrySearchParams>,
) -> Result<Json<Vec<archive::EntrySummary>>, ApiError> {
    auth.require_auth()?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    let raw = params.q.as_deref().unwrap_or("");
    let mut search_query = archive::parse_search_query(raw)
        .map_err(|prefix| ApiError::bad_request(&format!("unknown search prefix: {prefix}")))?;
    if let Some(tag) = params.tag {
        search_query.tag = Some(tag);
    }
    search_query.caller_bits = auth_to_caller_bits(&auth);
    Ok(Json(archive::search_entries(&conn, &search_query)?))
}

async fn entry_detail(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path((archive_id, entry_uid)): Path<(String, String)>,
) -> Result<Json<archive::EntryDetail>, ApiError> {
    auth_user.require_auth()?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    let detail = archive::get_entry_detail(&conn, &entry_uid)?
        .ok_or(ApiError::not_found("entry not found"))?;
    Ok(Json(detail))
}

async fn list_runs(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(archive_id): Path<String>,
) -> Result<Json<Vec<archive::RunSummary>>, ApiError> {
    auth_user.require_auth()?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    Ok(Json(archive::list_runs(&conn)?))
}

async fn serve_artifact(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path((archive_id, entry_uid, artifact_index)): Path<(String, String, usize)>,
    req: Request,
) -> Result<Response, ApiError> {
    auth_user.require_auth()?;
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
    Ok(ServeFile::new(&file_path)
        .oneshot(req)
        .await
        .unwrap()
        .into_response())
}

async fn serve_entry_favicon(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path((archive_id, entry_uid)): Path<(String, String)>,
    req: Request,
) -> Result<Response, ApiError> {
    auth_user.require_auth()?;
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
    auth_user: AuthUser,
    Path((archive_id, sha256)): Path<(String, String)>,
    req: Request,
) -> Result<Response, ApiError> {
    auth_user.require_auth()?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let paths = archive::read_archive_paths(&mounted.archive_path)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    let blob = database::get_blob_by_sha256(&conn, &sha256)?
        .ok_or(ApiError::not_found("blob not found"))?;
    let file_path = paths.store_path.join(&blob.raw_relpath);
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

#[derive(Debug, serde::Deserialize)]
struct CreateCollectionBody {
    name: String,
    slug: String,
    #[serde(default = "default_user_visibility")]
    default_visibility_bits: u32,
}

fn default_user_visibility() -> u32 {
    2
}

#[derive(Debug, serde::Deserialize)]
struct AddEntryBody {
    entry_uid: String,
    #[serde(default = "default_user_visibility")]
    visibility_bits: u32,
}

#[derive(Debug, serde::Deserialize)]
struct UpdateVisibilityBody {
    visibility_bits: u32,
}

#[derive(Debug, serde::Deserialize)]
struct PatchCollectionBody {
    name: Option<String>,
    default_visibility_bits: Option<u32>,
}

async fn list_tags(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(archive_id): Path<String>,
) -> Result<Json<Vec<archive::TagNode>>, ApiError> {
    auth_user.require_auth()?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    Ok(Json(archive::list_tag_tree(&conn)?))
}

async fn create_tag_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(archive_id): Path<String>,
    Json(body): Json<CreateTagBody>,
) -> Result<(StatusCode, Json<archive::Tag>), ApiError> {
    auth_user.require_role(ROLE_USER)?;
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
    auth_user: AuthUser,
    Path((archive_id, entry_uid)): Path<(String, String)>,
) -> Result<Json<Vec<archive::Tag>>, ApiError> {
    auth_user.require_auth()?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    match archive::get_entry_tags(&conn, &entry_uid)? {
        Some(tags) => Ok(Json(tags)),
        None => Err(ApiError::not_found("entry not found")),
    }
}

async fn assign_entry_tag_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path((archive_id, entry_uid)): Path<(String, String)>,
    Json(body): Json<AssignTagBody>,
) -> Result<(StatusCode, Json<archive::Tag>), ApiError> {
    auth_user.require_role(ROLE_USER)?;
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
    auth_user: AuthUser,
    Path((archive_id, entry_uid, tag_uid)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    auth_user.require_role(ROLE_USER)?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    if archive::remove_entry_tag(&conn, &entry_uid, &tag_uid)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("entry or tag not found"))
    }
}

async fn patch_tag_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path((archive_id, tag_uid)): Path<(String, String)>,
    Json(body): Json<PatchTagBody>,
) -> Result<Json<archive::Tag>, ApiError> {
    auth_user.require_role(ROLE_USER)?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    match database::rename_tag(&conn, &tag_uid, &body.name)? {
        Some(record) => Ok(Json(archive::Tag {
            tag_uid: record.tag_uid,
            name: record.name,
            slug: record.slug,
            full_path: record.full_path,
        })),
        None => Err(ApiError::not_found("tag not found")),
    }
}

async fn delete_tag_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path((archive_id, tag_uid)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    auth_user.require_role(ROLE_USER)?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    if database::delete_tag(&conn, &tag_uid)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("tag not found"))
    }
}

async fn move_tag_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path((archive_id, tag_uid)): Path<(String, String)>,
    Json(body): Json<MoveTagBody>,
) -> Result<Json<archive::Tag>, ApiError> {
    auth_user.require_role(ROLE_USER)?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    match database::move_tag(&conn, &tag_uid, body.parent_uid.as_deref())? {
        Some(record) => Ok(Json(archive::Tag {
            tag_uid: record.tag_uid,
            name: record.name,
            slug: record.slug,
            full_path: record.full_path,
        })),
        None => Err(ApiError::not_found("tag not found")),
    }
}

async fn patch_entry_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path((archive_id, entry_uid)): Path<(String, String)>,
    Json(body): Json<PatchEntryBody>,
) -> Result<StatusCode, ApiError> {
    auth_user.require_role(ROLE_USER)?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    let title = body
        .title
        .as_deref()
        .map(|s| {
            let t = s.trim();
            if t.is_empty() { None } else { Some(t) }
        })
        .flatten();
    if database::update_entry_title(&conn, &entry_uid, title)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("entry not found"))
    }
}

async fn delete_entry_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path((archive_id, entry_uid)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    auth_user.require_role(ROLE_USER)?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let mut conn = database::open_or_initialize(&mounted.archive_path)?;
    // Transaction: if any step fails (cascade update, FK null, or delete), nothing is committed.
    let tx = conn.transaction()?;
    let found = database::delete_entry(&tx, &entry_uid)?;
    tx.commit()?;
    if found {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("entry not found"))
    }
}

#[derive(Debug, serde::Deserialize)]
struct CaptureBody {
    locator: String,
    quality: Option<String>,
    ublock_enabled: Option<bool>,
    /// Distil to article content via Readability before archiving.  Absent = false.
    reader_mode: Option<bool>,
    cookie_ext_enabled: Option<bool>,
    modal_closer_enabled: Option<bool>,
    /// Route through Freedium mirror for WebPage captures. Absent = true (on by default).
    via_freedium: Option<bool>,
    /// Per-video quality overrides for playlist captures.
    /// Keys are yt-dlp video IDs; values are quality strings ("best", "1080p", "audio", etc.).
    #[serde(default)]
    per_item_quality: std::collections::HashMap<String, String>,
    /// When true, skip playlist items already archived under an existing container.
    #[serde(default)]
    sync: bool,
}

#[derive(Debug, serde::Deserialize)]
struct ProbeQuery {
    locator: String,
}

#[derive(Debug, serde::Deserialize)]
struct ProbePlaylistBody {
    locator: String,
}

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

#[derive(Debug, serde::Deserialize)]
struct CreateTokenBody {
    name: String,
}

#[derive(Debug, serde::Deserialize)]
struct PatchEntryBody {
    title: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct PatchTagBody {
    name: String,
}

#[derive(Debug, serde::Deserialize)]
struct MoveTagBody {
    /// `None` promotes the tag to root; `Some(uid)` sets a new parent.
    parent_uid: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct CreateCookieRuleBody {
    url_pattern: Option<String>,
    pattern_kind: String,
    cookies_json: String,
}

#[derive(Debug, serde::Deserialize)]
struct UpdateCookieRuleBody {
    url_pattern: Option<serde_json::Value>, // null → clear, string → set, absent → keep
    pattern_kind: Option<String>,
    cookies_json: Option<String>,
    ordinal: Option<i64>,
}

async fn capture_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(archive_id): Path<String>,
    Json(body): Json<CaptureBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    auth_user.require_role(ROLE_USER)?;
    if body.locator.trim().is_empty() {
        return Err(ApiError::bad_request("locator must not be empty"));
    }
    if let Some(q) = &body.quality {
        let valid = q == "best"
            || q == "audio"
            || q.strip_suffix('p')
                .and_then(|n| n.parse::<u32>().ok())
                .is_some();
        if !valid {
            return Err(ApiError::bad_request(
                "invalid quality: must be \"best\", \"audio\", or a height string like \"1080p\"",
            ));
        }
    }
    {
        let is_valid_quality = |q: &str| {
            q == "best"
                || q == "audio"
                || q.strip_suffix('p')
                    .and_then(|n| n.parse::<u32>().ok())
                    .is_some()
        };
        if let Some(bad) = body.per_item_quality.values().find(|q| !is_valid_quality(q)) {
            return Err(ApiError::bad_request(&format!(
                "invalid per_item_quality value {bad:?}: must be \"best\", \"audio\", or a height string like \"1080p\""
            )));
        }
    }
    // per_item_quality semantics (enforced in capture.rs):
    // - Absent or empty map: all playlist items are downloaded; quality is the
    //   global `quality` field applied as a yt-dlp cap with graceful fallback.
    // - Non-empty map: ONLY items whose yt-dlp ID appears as a key are downloaded;
    //   absent IDs are skipped. This is how the frontend's delete-item button works.
    // The "must choose quality for unsupported videos" invariant is enforced by the
    // frontend before submission; a direct API caller bypassing the UI accepts
    // yt-dlp's standard cap-and-fallback behavior for items it includes.
    let mounted = mounted_archive(&state, &archive_id)?;
    let archive_paths =
        archive::read_archive_paths(&mounted.archive_path).map_err(ApiError::from)?;

    // Create job record in the archive DB.
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    let job_uid = database::create_capture_job(&conn, &archive_id)?;
    drop(conn);
    // Load cookie rules and global uBlock / cookie-ext settings from the auth DB.
    let (cookie_rules, global_ublock, global_cookie_ext, global_modal_closer) = {
        match database::open_auth_db(&state.auth_db_path) {
            Ok(conn) => {
                let rules = database::list_cookie_rules(&conn).unwrap_or_default();
                let settings = database::get_instance_settings(&conn);
                let ublock = settings.as_ref().map(|s| s.ublock_enabled).unwrap_or(true);
                let cookie_ext = settings
                    .as_ref()
                    .map(|s| s.cookie_ext_enabled)
                    .unwrap_or(true);
                let modal_closer = settings.map(|s| s.modal_closer_enabled).unwrap_or(true);
                (rules, ublock, cookie_ext, modal_closer)
            }
            Err(_) => (vec![], true, true, true),
        }
    };
    // Per-capture body overrides global; if body doesn't specify, use the global setting.
    // The resolved bool is then passed as Some(_) to singlefile, overriding the env var.
    let effective_ublock = body.ublock_enabled.unwrap_or(global_ublock);
    let effective_cookie_ext = body.cookie_ext_enabled.unwrap_or(global_cookie_ext);
    let effective_modal_closer = body.modal_closer_enabled.unwrap_or(global_modal_closer);
    let capture_config = capture::CaptureConfig {
        cookie_rules,
        ublock_enabled: Some(effective_ublock),
        cookie_ext_enabled: Some(effective_cookie_ext),
        modal_closer_enabled: Some(effective_modal_closer),
        reader_mode: body.reader_mode.unwrap_or(false),
        via_freedium: body.via_freedium.unwrap_or(true),
        per_item_quality: body.per_item_quality.clone(),
        sync: body.sync,
    };

    // Spawn background capture.
    let locator = body.locator.trim().to_string();
    let quality = body.quality.clone();
    let archive_path = mounted.archive_path.clone();
    let job_uid_bg = job_uid.clone();
    let archive_id_bg = archive_id.clone();
    tokio::task::spawn_blocking(move || {
        let conn = match database::open_or_initialize(&archive_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("warn: capture job {job_uid_bg}: db open failed: {e:#}");
                return;
            }
        };
        database::update_capture_job_status(&conn, &job_uid_bg, "running", None, None, None).ok();
        match capture::perform_capture(
            &archive_paths,
            &locator,
            Some(&archive_id_bg),
            quality.as_deref(),
            &capture_config,
        ) {
            Ok(result) => {
                let mut notes_map = serde_json::Map::new();
                if result.ublock_skipped {
                    notes_map.insert("ublock_skipped".into(), serde_json::Value::Bool(true));
                }
                if result.cookie_ext_skipped {
                    notes_map.insert("cookie_ext_skipped".into(), serde_json::Value::Bool(true));
                }
                let notes_str;
                let notes: Option<&str> = if notes_map.is_empty() {
                    None
                } else {
                    notes_str = serde_json::Value::Object(notes_map).to_string();
                    Some(&notes_str)
                };
                // A partial playlist (some items succeeded, some failed) has status="failed"
                // but completed_child_count > 0. Treat it as a completed job so onCaptured
                // fires and the archived entries appear. Only mark failed when no child
                // succeeded (completed_child_count == 0).
                let job_status = if result.status == "failed" && result.completed_child_count == 0 {
                    "failed"
                } else {
                    "completed"
                };
                database::update_capture_job_status(
                    &conn,
                    &job_uid_bg,
                    job_status,
                    Some(&result.run_uid),
                    None,
                    notes,
                )
                .ok();
            }
            Err(e) => {
                database::update_capture_job_status(
                    &conn,
                    &job_uid_bg,
                    "failed",
                    None,
                    Some(&format!("{e:#}")),
                    None,
                )
                .ok();
            }
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "job_uid": job_uid, "status": "pending" })),
    ))
}

async fn get_capture_job_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path((archive_id, job_uid)): Path<(String, String)>,
) -> Result<Json<archive::CaptureJobSummary>, ApiError> {
    auth_user.require_role(ROLE_USER)?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    archive::get_capture_job(&conn, &job_uid)?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("capture job not found"))
}

/// POST /api/archives/:archive_id/entries/:entry_uid/rearchive
///
/// Re-archives an existing tweet or tweet_thread entry in-place:
/// - Stages scraper output in a temp dir (existing data safe if scraper fails)
/// - On success: atomically replaces entry_artifacts and refreshes cached_bytes
/// - On failure (tweet deleted/private): job is marked failed; existing data preserved
///
/// Returns 202 immediately with a job_uid the client should poll via
/// GET /api/archives/:archive_id/capture_jobs/:job_uid.
async fn rearchive_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path((archive_id, entry_uid)): Path<(String, String)>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    auth_user.require_role(ROLE_USER)?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let archive_paths =
        archive::read_archive_paths(&mounted.archive_path).map_err(ApiError::from)?;

    // Create a capture job record so the client can poll for completion.
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    let job_uid = database::create_capture_job(&conn, &archive_id)?;
    drop(conn);

    // Load cookie rules from the auth DB (needed for Twitter credentials resolution).
    let cookie_rules = match database::open_auth_db(&state.auth_db_path) {
        Ok(conn) => database::list_cookie_rules(&conn).unwrap_or_default(),
        Err(_) => vec![],
    };
    let capture_config = capture::CaptureConfig {
        cookie_rules,
        ublock_enabled: None,
        cookie_ext_enabled: None,
        modal_closer_enabled: None,
        reader_mode: false,
        via_freedium: false,
        per_item_quality: std::collections::HashMap::new(),
        sync: false,
    };

    let job_uid_bg = job_uid.clone();
    let archive_path = mounted.archive_path.clone();
    tokio::task::spawn_blocking(move || {
        let conn = match database::open_or_initialize(&archive_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("warn: rearchive job {job_uid_bg}: db open failed: {e:#}");
                return;
            }
        };
        database::update_capture_job_status(&conn, &job_uid_bg, "running", None, None, None).ok();
        match capture::perform_rearchive(&archive_paths, &entry_uid, &capture_config) {
            Ok(result) => {
                if result.status == "completed" {
                    database::update_capture_job_status(
                        &conn,
                        &job_uid_bg,
                        "completed",
                        None,
                        None,
                        None,
                    )
                    .ok();
                } else {
                    // "not_a_tweet" or "scraper_failed" — surface as a job failure
                    // so the client sees a meaningful error message.
                    database::update_capture_job_status(
                        &conn,
                        &job_uid_bg,
                        "failed",
                        None,
                        Some(&result.message),
                        None,
                    )
                    .ok();
                }
            }
            Err(e) => {
                database::update_capture_job_status(
                    &conn,
                    &job_uid_bg,
                    "failed",
                    None,
                    Some(&format!("{e:#}")),
                    None,
                )
                .ok();
            }
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "job_uid": job_uid, "status": "pending" })),
    ))
}

/// `GET /api/archives/:id/captures/probe?locator=<url>`
///
/// Runs `yt-dlp --dump-json` (behind `spawn_blocking`) and returns the video
/// heights actually available at the given locator.
///
/// Response shapes:
/// - Locator is not a yt-dlp source (tweet, webpage, local, …):
///   `{ "has_video": false, "qualities": [] }` — 200
/// - yt-dlp ran and found no video tracks (e.g. tweet URL with no media):
///   `{ "has_video": false, "qualities": [] }` — 200
/// - yt-dlp ran and found video tracks:
///   `{ "has_video": true, "qualities": ["1080p", "720p", …] }` — 200
/// - yt-dlp itself failed (non-zero exit, network error, rate-limit, …):
///   502 — caller should treat this as "probe inconclusive", not "no video"
async fn probe_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(archive_id): Path<String>,
    Query(params): Query<ProbeQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    auth_user.require_role(ROLE_USER)?;
    let locator = params.locator.trim().to_string();
    if locator.is_empty() {
        return Err(ApiError::bad_request("locator must not be empty"));
    }
    // Verify the archive exists but don't need the paths for probing.
    let _ = mounted_archive(&state, &archive_id)?;

    // Resolve to a yt-dlp URL; return empty result immediately for non-video sources.
    let Some(ytdlp_url) = capture::locator_to_ytdlp_url(&locator) else {
        return Ok(Json(
            serde_json::json!({ "has_video": false, "has_audio": false, "qualities": [] }),
        ));
    };

    // fetch_metadata shells out and can take several seconds — keep the async runtime free.
    // Returns None when yt-dlp exits non-zero (transient error, rate-limit, unsupported
    // extractor, etc.). That is distinct from "yt-dlp ran fine but found no video": we
    // return 502 so the frontend treats it as inconclusive rather than showing
    // "No video detected" for a URL that may well be downloadable.
    let cookie_rules = match database::open_auth_db(&state.auth_db_path) {
        Ok(conn) => database::list_cookie_rules(&conn).unwrap_or_default(),
        Err(_) => vec![],
    };
    let cookies = capture::resolve_cookies_for_url(&cookie_rules, &ytdlp_url);
    let maybe_result = tokio::task::spawn_blocking(move || {
        downloader::ytdlp::fetch_metadata(&ytdlp_url, &cookies)
            .map(|json| downloader::ytdlp::probe_result(&json))
    })
    .await
    .map_err(|_| ApiError::internal("probe task panicked"))?;

    let result = maybe_result.ok_or_else(|| ApiError {
        status: StatusCode::BAD_GATEWAY,
        message: "yt-dlp metadata fetch failed".to_string(),
    })?;

    let qualities: Vec<String> = result
        .video_heights
        .iter()
        .map(|h| format!("{h}p"))
        .collect();
    Ok(Json(serde_json::json!({
        "has_video": !qualities.is_empty(),
        "qualities": qualities,
        "has_audio": result.has_audio,
    })))
}

async fn probe_playlist_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(archive_id): Path<String>,
    Json(body): Json<ProbePlaylistBody>,
) -> Result<Json<downloader::ytdlp::PlaylistProbeResult>, ApiError> {
    auth_user.require_role(ROLE_USER)?;
    let locator = body.locator.trim().to_string();
    if locator.is_empty() {
        return Err(ApiError::bad_request("locator must not be empty"));
    }
    // Validate it's a playlist/channel source and expand shorthands.
    let canonical_url = capture::locator_to_playlist_url(&locator)
        .ok_or_else(|| ApiError::bad_request("locator is not a YouTube playlist, channel, YTM playlist, or Spotify album/playlist"))?;
    // Verify archive exists.
    let _ = mounted_archive(&state, &archive_id)?;
    // Resolve cookies.
    let cookie_rules = match database::open_auth_db(&state.auth_db_path) {
        Ok(conn) => database::list_cookie_rules(&conn).unwrap_or_default(),
        Err(_) => vec![],
    };
    let cookies = capture::resolve_cookies_for_url(&cookie_rules, &canonical_url);
    // Shell out to yt-dlp in a blocking task.
    let result = tokio::task::spawn_blocking(move || {
        downloader::ytdlp::probe_playlist_qualities(&canonical_url, &cookies)
    })
    .await
    .map_err(|_| ApiError::internal("probe-playlist task panicked"))?
    .map_err(|e| ApiError {
        status: StatusCode::BAD_GATEWAY,
        message: format!("playlist probe failed: {e:#}"),
    })?;
    Ok(Json(result))
}

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
        return Err(ApiError::bad_request(
            "username required and password must be at least 8 characters",
        ));
    }
    let hash = auth::hash_password(&body.password).map_err(ApiError::from)?;
    database::create_owner(&conn, &body.username, &hash)?;
    let user = database::get_user_by_username(&conn, &body.username)?
        .ok_or_else(|| ApiError::internal("user not found after creation"))?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "user_uid": user.user_uid,
            "username": user.username,
        })),
    ))
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
    if !auth::verify_password(&body.password, &user.password_hash).map_err(ApiError::from)? {
        return Err(ApiError::unauthorized("invalid_credentials"));
    }
    let role_bits = database::compute_role_bits(&conn, user.id)?;
    let user_agent = headers.get("user-agent").and_then(|v| v.to_str().ok());
    let session_uid = database::create_session(&conn, user.id, role_bits, user_agent)?;

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
        cookie_value
            .parse()
            .map_err(|_| ApiError::internal("cookie error"))?,
    );

    Ok((
        StatusCode::OK,
        resp_headers,
        Json(serde_json::json!({
            "user_uid": user.user_uid,
            "username": user.username,
            "role_bits": role_bits,
        })),
    ))
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
    let (username, display_name, humanize_slugs_int): (String, Option<String>, i64) = conn
        .query_row(
            "SELECT username, display_name, COALESCE(humanize_slugs, 0) FROM users WHERE id = ?1",
            [user_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|e| ApiError::from(anyhow::anyhow!("db error: {e}")))?;
    let humanize_slugs = humanize_slugs_int != 0;
    Ok(Json(serde_json::json!({
        "role_bits": role_bits,
        "username": username,
        "display_name": display_name,
        "humanize_slugs": humanize_slugs,
    })))
}

async fn patch_me(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Json(body): Json<UpdateProfileBody>,
) -> Result<StatusCode, ApiError> {
    let (user_id, _) = auth_user.require_auth()?;
    let conn = database::open_auth_db(&state.auth_db_path)?;

    if let Some(ref new_pw) = body.new_password {
        if new_pw.trim().is_empty() {
            return Err(ApiError::bad_request("new_password must not be blank"));
        }
        let current_pw = body.current_password.as_deref().unwrap_or("");
        let hash = database::get_user_password_hash(&conn, user_id)?
            .ok_or_else(|| ApiError::not_found("user not found"))?;
        if !auth::verify_password(current_pw, &hash).map_err(ApiError::from)? {
            return Err(ApiError::unauthorized("current password is incorrect"));
        }
        let new_hash = auth::hash_password(new_pw).map_err(ApiError::from)?;
        database::update_user_password(&conn, user_id, &new_hash)?;
    }

    if let Some(ref dn) = body.display_name {
        let v: Option<&str> = if dn.trim().is_empty() {
            None
        } else {
            Some(dn.as_str())
        };
        database::update_user_display_name(&conn, user_id, v)?;
    }

    if let Some(hs) = body.humanize_slugs {
        database::update_user_humanize_slugs(&conn, user_id, hs)?;
    }

    Ok(StatusCode::NO_CONTENT)
}

async fn get_instance_settings_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
) -> Result<Json<serde_json::Value>, ApiError> {
    auth_user.require_role(ROLE_ADMIN)?;
    let conn = database::open_auth_db(&state.auth_db_path)?;
    let settings = database::get_instance_settings(&conn)?;
    let ublock_ext_available = std::env::var("ARCHIVR_UBLOCK_EXT")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|p| std::path::Path::new(&p).is_dir())
        .unwrap_or(false);
    let cookie_ext_available = std::env::var("ARCHIVR_COOKIE_EXT")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|p| std::path::Path::new(&p).is_dir())
        .unwrap_or(false);
    let mut val = serde_json::to_value(&settings).unwrap_or_default();
    if let Some(obj) = val.as_object_mut() {
        obj.insert(
            "ublock_ext_available".into(),
            serde_json::Value::Bool(ublock_ext_available),
        );
        obj.insert(
            "cookie_ext_available".into(),
            serde_json::Value::Bool(cookie_ext_available),
        );
    }
    Ok(Json(val))
}

async fn update_instance_settings_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Json(body): Json<UpdateInstanceSettingsBody>,
) -> Result<StatusCode, ApiError> {
    auth_user.require_role(ROLE_ADMIN)?;
    let conn = database::open_auth_db(&state.auth_db_path)?;
    let mut settings = database::get_instance_settings(&conn)?;
    if let Some(v) = body.public_index_enabled {
        settings.public_index_enabled = v;
    }
    if let Some(v) = body.public_entry_content_enabled {
        settings.public_entry_content_enabled = v;
    }
    if let Some(v) = body.open_registration_enabled {
        settings.open_registration_enabled = v;
    }
    if let Some(v) = body.default_entry_visibility {
        settings.default_entry_visibility = v;
    }
    if let Some(v) = body.ublock_enabled {
        settings.ublock_enabled = v;
    }
    if let Some(v) = body.cookie_ext_enabled {
        settings.cookie_ext_enabled = v;
    }
    if let Some(v) = body.modal_closer_enabled {
        settings.modal_closer_enabled = v;
    }
    database::update_instance_settings(&conn, &settings)?;
    Ok(StatusCode::NO_CONTENT)
}

// ── Cookie rules ──────────────────────────────────────────────────────────────

async fn list_cookie_rules_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
) -> Result<Json<Vec<database::CookieRule>>, ApiError> {
    auth_user.require_role(ROLE_ADMIN)?;
    let conn = database::open_auth_db(&state.auth_db_path)?;
    Ok(Json(database::list_cookie_rules(&conn)?))
}

async fn create_cookie_rule_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Json(body): Json<CreateCookieRuleBody>,
) -> Result<(StatusCode, Json<database::CookieRule>), ApiError> {
    auth_user.require_role(ROLE_ADMIN)?;
    if !["global", "wildcard", "regex"].contains(&body.pattern_kind.as_str()) {
        return Err(ApiError::bad_request(
            "pattern_kind must be 'global', 'wildcard', or 'regex'",
        ));
    }
    if serde_json::from_str::<std::collections::HashMap<String, String>>(&body.cookies_json)
        .is_err()
    {
        return Err(ApiError::bad_request(
            "cookies_json must be a JSON object whose values are all strings, e.g. {\"name\": \"value\"}",
        ));
    }
    if body.pattern_kind != "global" && body.url_pattern.as_deref().unwrap_or("").trim().is_empty()
    {
        return Err(ApiError::bad_request(
            "url_pattern is required for non-global rules",
        ));
    }
    if body.pattern_kind == "regex" {
        if let Some(pat) = &body.url_pattern {
            regex::Regex::new(pat)
                .map_err(|e| ApiError::bad_request(&format!("invalid regex: {e}")))?;
        }
    }
    let conn = database::open_auth_db(&state.auth_db_path)?;
    let url_pattern = body.url_pattern.as_deref().filter(|s| !s.trim().is_empty());
    let rule =
        database::create_cookie_rule(&conn, url_pattern, &body.pattern_kind, &body.cookies_json)?;
    Ok((StatusCode::CREATED, Json(rule)))
}

async fn update_cookie_rule_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(rule_uid): Path<String>,
    Json(body): Json<UpdateCookieRuleBody>,
) -> Result<StatusCode, ApiError> {
    auth_user.require_role(ROLE_ADMIN)?;
    let conn = database::open_auth_db(&state.auth_db_path)?;
    let rules = database::list_cookie_rules(&conn)?;
    let existing = rules
        .into_iter()
        .find(|r| r.rule_uid == rule_uid)
        .ok_or_else(|| ApiError::not_found("cookie rule not found"))?;
    let pattern_kind = body.pattern_kind.unwrap_or(existing.pattern_kind);
    let cookies_json = body.cookies_json.unwrap_or(existing.cookies_json);
    let ordinal = body.ordinal.unwrap_or(existing.ordinal);
    // url_pattern: null JSON value → clear, string → set, absent (None) → keep existing
    let url_pattern: Option<String> = match body.url_pattern {
        Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(s)) if s.trim().is_empty() => None,
        Some(serde_json::Value::String(s)) => Some(s),
        None => existing.url_pattern,
        _ => existing.url_pattern,
    };
    if !["global", "wildcard", "regex"].contains(&pattern_kind.as_str()) {
        return Err(ApiError::bad_request(
            "pattern_kind must be 'global', 'wildcard', or 'regex'",
        ));
    }
    if serde_json::from_str::<std::collections::HashMap<String, String>>(&cookies_json).is_err() {
        return Err(ApiError::bad_request(
            "cookies_json must be a JSON object whose values are all strings, e.g. {\"name\": \"value\"}",
        ));
    }
    if pattern_kind == "regex" {
        if let Some(pat) = &url_pattern {
            regex::Regex::new(pat)
                .map_err(|e| ApiError::bad_request(&format!("invalid regex: {e}")))?;
        }
    }
    database::update_cookie_rule(
        &conn,
        &rule_uid,
        url_pattern.as_deref(),
        &pattern_kind,
        &cookies_json,
        ordinal,
    )?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_cookie_rule_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(rule_uid): Path<String>,
) -> Result<StatusCode, ApiError> {
    auth_user.require_role(ROLE_ADMIN)?;
    let conn = database::open_auth_db(&state.auth_db_path)?;
    database::delete_cookie_rule(&conn, &rule_uid)
        .map_err(|_| ApiError::not_found("cookie rule not found"))?;
    Ok(StatusCode::NO_CONTENT)
}

// ── Blob / orphan cleanup ─────────────────────────────────────────────────────

#[derive(serde::Serialize)]
struct BlobCleanupScanResponse {
    orphaned_blob_rows: usize,
    deletable_files: usize,
    total_bytes: u64,
}

/// GET /api/archives/:archive_id/blob-cleanup
/// Returns stats on orphaned blob DB rows and unreferenced raw files.
/// Returns 409 if any capture job is pending or running.
async fn blob_cleanup_scan_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(archive_id): Path<String>,
) -> Result<Json<BlobCleanupScanResponse>, ApiError> {
    auth_user.require_role(ROLE_ADMIN)?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let paths = archive::read_archive_paths(&mounted.archive_path)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;

    if database::has_active_capture_jobs(&conn)? {
        return Err(ApiError::conflict(
            "captures are in progress; wait for them to finish before scanning",
        ));
    }

    let referenced = database::all_referenced_file_relpaths(&conn)?;
    let orphaned_blob_rows = database::list_orphaned_blob_rows(&conn)?.len();
    let orphaned_files = collect_orphaned_disk_files(&paths.store_path, &referenced)
        .map_err(|e| ApiError::internal(&format!("disk scan failed: {e:#}")))?;
    let total_bytes: u64 = orphaned_files.iter().map(|(_, sz)| sz).sum();

    Ok(Json(BlobCleanupScanResponse {
        orphaned_blob_rows,
        deletable_files: orphaned_files.len(),
        total_bytes,
    }))
}

/// DELETE /api/archives/:archive_id/blob-cleanup
/// Deletes orphaned blob DB rows and unreferenced raw files.
/// Re-checks for active captures immediately before executing to close the TOCTOU window.
async fn blob_cleanup_delete_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(archive_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    auth_user.require_role(ROLE_ADMIN)?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let paths = archive::read_archive_paths(&mounted.archive_path)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;

    // Re-check immediately before acting (closes the TOCTOU gap between scan and delete).
    if database::has_active_capture_jobs(&conn)? {
        return Err(ApiError::conflict(
            "captures are in progress; wait for them to finish before cleaning up",
        ));
    }

    // Collect the set of protected relpaths and the files to delete BEFORE mutating the DB.
    // This ensures the referenced set is consistent with the rows we're about to remove.
    let referenced = database::all_referenced_file_relpaths(&conn)?;
    let files_to_delete = collect_orphaned_disk_files(&paths.store_path, &referenced)
        .map_err(|e| ApiError::internal(&format!("disk scan failed: {e:#}")))?;

    // Second guard: re-check after the disk walk, which can be slow.
    // A capture that started during the walk may have moved files into raw/ before
    // writing its DB rows; those files would appear orphaned but must not be deleted.
    if database::has_active_capture_jobs(&conn)? {
        return Err(ApiError::conflict(
            "a capture started during the scan; retry after all captures finish",
        ));
    }

    // Delete orphaned blob rows from the database.
    let deleted_blob_rows = database::delete_orphaned_blob_rows(&conn)?;

    // Delete the unreferenced disk files.
    let mut freed_bytes: u64 = 0;
    let mut deleted_files: usize = 0;
    let mut errors: Vec<String> = Vec::new();
    for (path, size) in &files_to_delete {
        match std::fs::remove_file(path) {
            Ok(()) => {
                freed_bytes += *size;
                deleted_files += 1;
            }
            Err(e) => errors.push(format!("{}: {e}", path.display())),
        }
    }

    eprintln!(
        "info: blob cleanup for '{}': {} blob rows, {} files deleted, {} bytes freed, {} errors",
        archive_id,
        deleted_blob_rows,
        deleted_files,
        freed_bytes,
        errors.len()
    );

    Ok(Json(serde_json::json!({
        "deleted_blob_rows": deleted_blob_rows,
        "deleted_files": deleted_files,
        "freed_bytes": freed_bytes,
        "errors": errors,
    })))
}

/// Walk `raw/` and `raw_tweets/` under `store_path` and return every file whose
/// relpath (relative to `store_path`, forward-slash separated) is absent from
/// `referenced`.  Each entry is `(absolute_path, byte_size)`.
fn collect_orphaned_disk_files(
    store_path: &std::path::Path,
    referenced: &std::collections::HashSet<String>,
) -> anyhow::Result<Vec<(std::path::PathBuf, u64)>> {
    let mut result = Vec::new();
    for subdir in &["raw", "raw_tweets"] {
        let dir = store_path.join(subdir);
        if !dir.exists() {
            continue;
        }
        let mut stack = vec![dir];
        while let Some(current) = stack.pop() {
            for entry in std::fs::read_dir(&current)? {
                let entry = entry?;
                let path = entry.path();
                let ft = entry.file_type()?;
                if ft.is_dir() {
                    stack.push(path);
                } else if ft.is_file() {
                    if let Ok(rel) = path.strip_prefix(store_path) {
                        // Normalise to forward slashes (relevant on Windows if ever deployed there).
                        let relpath = rel.to_string_lossy().replace('\\', "/");
                        if !referenced.contains(&relpath) {
                            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                            result.push((path, size));
                        }
                    }
                }
            }
        }
    }
    Ok(result)
}

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
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "token_uid": token_uid,
            "raw_token": raw_token,
            "name": body.name,
        })),
    ))
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

#[derive(Debug, serde::Deserialize)]
struct AdminCreateUserBody {
    username: String,
    password: String,
    email: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct AdminSetStatusBody {
    status: String,
}

#[derive(Debug, serde::Deserialize)]
struct AdminAssignRoleBody {
    role_slug: String,
}

#[derive(Debug, serde::Deserialize)]
struct AdminCreateRoleBody {
    slug: String,
    name: String,
}

#[derive(Debug, serde::Deserialize)]
struct UpdateProfileBody {
    display_name: Option<String>,
    current_password: Option<String>,
    new_password: Option<String>,
    humanize_slugs: Option<bool>,
}

#[derive(Debug, serde::Deserialize)]
struct UpdateInstanceSettingsBody {
    public_index_enabled: Option<bool>,
    public_entry_content_enabled: Option<bool>,
    open_registration_enabled: Option<bool>,
    default_entry_visibility: Option<u32>,
    ublock_enabled: Option<bool>,
    cookie_ext_enabled: Option<bool>,
    modal_closer_enabled: Option<bool>,
}

async fn admin_list_users(
    State(state): State<AppState>,
    auth_user: AuthUser,
) -> Result<Json<Vec<database::UserSummary>>, ApiError> {
    auth_user.require_role(ROLE_ADMIN)?;
    let conn = database::open_auth_db(&state.auth_db_path)?;
    Ok(Json(database::list_users(&conn)?))
}

async fn admin_create_user(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Json(body): Json<AdminCreateUserBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    auth_user.require_role(ROLE_ADMIN)?;
    let (caller_id, _) = auth_user.require_auth()?;
    if body.username.trim().is_empty() || body.password.len() < 8 {
        return Err(ApiError::bad_request(
            "username required, password >= 8 chars",
        ));
    }
    let conn = database::open_auth_db(&state.auth_db_path)?;
    let hash = auth::hash_password(&body.password).map_err(ApiError::from)?;
    let uid = database::create_user(
        &conn,
        &body.username,
        body.email.as_deref(),
        &hash,
        caller_id,
    )?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "user_uid": uid, "username": body.username })),
    ))
}

async fn admin_set_user_status(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(uid): Path<String>,
    Json(body): Json<AdminSetStatusBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    auth_user.require_role(ROLE_ADMIN)?;
    if body.status != "active" && body.status != "disabled" {
        return Err(ApiError::bad_request(
            "status must be 'active' or 'disabled'",
        ));
    }
    let conn = database::open_auth_db(&state.auth_db_path)?;
    if !database::set_user_status(&conn, &uid, &body.status)? {
        return Err(ApiError::not_found("user not found"));
    }
    Ok(Json(
        serde_json::json!({ "user_uid": uid, "status": body.status }),
    ))
}

async fn admin_assign_role(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(uid): Path<String>,
    Json(body): Json<AdminAssignRoleBody>,
) -> Result<StatusCode, ApiError> {
    auth_user.require_role(ROLE_ADMIN)?;
    let (caller_id, _) = auth_user.require_auth()?;
    let conn = database::open_auth_db(&state.auth_db_path)?;
    let target_id = database::get_user_id_by_uid(&conn, &uid)?
        .ok_or_else(|| ApiError::not_found("user not found"))?;
    database::assign_role(&conn, target_id, &body.role_slug, caller_id)?;
    Ok(StatusCode::OK)
}

async fn admin_remove_role(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path((uid, role_slug)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    auth_user.require_role(ROLE_ADMIN)?;
    let conn = database::open_auth_db(&state.auth_db_path)?;
    let target_id = database::get_user_id_by_uid(&conn, &uid)?
        .ok_or_else(|| ApiError::not_found("user not found"))?;
    database::remove_role(&conn, target_id, &role_slug)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn admin_list_roles(
    State(state): State<AppState>,
    auth_user: AuthUser,
) -> Result<Json<Vec<database::RoleRecord>>, ApiError> {
    auth_user.require_role(ROLE_ADMIN)?;
    let conn = database::open_auth_db(&state.auth_db_path)?;
    Ok(Json(database::list_roles(&conn)?))
}

async fn admin_create_role(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Json(body): Json<AdminCreateRoleBody>,
) -> Result<(StatusCode, Json<database::RoleRecord>), ApiError> {
    auth_user.require_role(ROLE_ADMIN)?;
    let conn = database::open_auth_db(&state.auth_db_path)?;
    let role =
        database::create_custom_role(&conn, &body.slug, &body.name).map_err(ApiError::from)?;
    Ok((StatusCode::CREATED, Json(role)))
}

fn auth_to_caller_bits(auth: &AuthUser) -> u32 {
    match auth {
        AuthUser::Authenticated { role_bits, .. } => *role_bits,
        AuthUser::Guest => ROLE_GUEST,
    }
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

    pub fn unauthorized(message: &str) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: message.to_string(),
        }
    }

    pub fn forbidden(message: &str) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.to_string(),
        }
    }

    fn conflict(message: &str) -> Self {
        Self {
            status: StatusCode::CONFLICT,
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
        let body = serde_json::json!({ "error": self.message });
        (self.status, axum::Json(body)).into_response()
    }
}

// ── Collection handlers ────────────────────────────────────────────────────────

async fn list_collections_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(archive_id): Path<String>,
) -> Result<Json<Vec<archive::CollectionSummary>>, ApiError> {
    auth_user.require_auth()?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    Ok(Json(archive::list_collections(&conn)?))
}

async fn create_collection_handler(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(archive_id): Path<String>,
    Json(body): Json<CreateCollectionBody>,
) -> Result<(StatusCode, Json<archive::CollectionSummary>), ApiError> {
    auth.require_role(ROLE_USER)?;
    if body.name.trim().is_empty() {
        return Err(ApiError::bad_request("collection name must not be empty"));
    }
    if body.slug.trim().is_empty() || body.slug.starts_with('_') {
        return Err(ApiError::bad_request(
            "collection slug must not be empty or start with underscore",
        ));
    }
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    let record =
        database::create_collection(&conn, &body.name, &body.slug, body.default_visibility_bits)
            .map_err(|e| ApiError::bad_request(&format!("{e:#}")))?;
    Ok((
        StatusCode::CREATED,
        Json(archive::CollectionSummary {
            collection_uid: record.collection_uid,
            name: record.name,
            slug: record.slug,
            default_visibility_bits: record.default_visibility_bits,
            created_at: record.created_at,
        }),
    ))
}

async fn get_collection_handler(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((archive_id, coll_uid)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    auth.require_auth()?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    let record = database::get_collection_by_uid(&conn, &coll_uid)?
        .ok_or(ApiError::not_found("collection not found"))?;
    let caller_bits = auth_to_caller_bits(&auth);
    let entries = archive::list_entries_for_collection(&conn, record.id, caller_bits)?;
    // Collect per-entry visibility bits from collection_entries
    let mut vis_map: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT ae.entry_uid, ce.visibility_bits \
             FROM collection_entries ce \
             JOIN archived_entries ae ON ae.id = ce.entry_id \
             WHERE ce.collection_id = ?1",
        )?;
        let rows = stmt.query_map([record.id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u32))
        })?;
        for r in rows {
            if let Ok((uid, bits)) = r {
                vis_map.insert(uid, bits);
            }
        }
    }
    let entries_json: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| {
            let vis = vis_map
                .get(&e.entry_uid)
                .copied()
                .unwrap_or(record.default_visibility_bits);
            serde_json::json!({
                "entry_uid": e.entry_uid,
                "title": e.title,
                "source_kind": e.source_kind,
                "archived_at": e.archived_at,
                "collection_visibility_bits": vis,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({
        "collection_uid": record.collection_uid,
        "name": record.name,
        "slug": record.slug,
        "default_visibility_bits": record.default_visibility_bits,
        "created_at": record.created_at,
        "entries": entries_json,
    })))
}

async fn add_entry_to_collection_handler(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((archive_id, coll_uid)): Path<(String, String)>,
    Json(body): Json<AddEntryBody>,
) -> Result<StatusCode, ApiError> {
    auth.require_role(ROLE_USER)?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    let coll = database::get_collection_by_uid(&conn, &coll_uid)?
        .ok_or(ApiError::not_found("collection not found"))?;
    if coll.slug == "_default_" {
        return Err(ApiError::bad_request(
            "cannot manually add entries to the default collection",
        ));
    }
    let entry_id: i64 = conn
        .query_row(
            "SELECT id FROM archived_entries WHERE entry_uid = ?1",
            [body.entry_uid.as_str()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(ApiError::not_found("entry not found"))?;
    database::add_entry_to_collection(&conn, coll.id, entry_id, body.visibility_bits)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_entry_from_collection_handler(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((archive_id, coll_uid, entry_uid)): Path<(String, String, String)>,
) -> Result<StatusCode, ApiError> {
    auth.require_role(ROLE_USER)?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    let coll = database::get_collection_by_uid(&conn, &coll_uid)?
        .ok_or(ApiError::not_found("collection not found"))?;
    if coll.slug == "_default_" {
        return Err(ApiError::bad_request(
            "cannot manually remove entries from the default collection",
        ));
    }
    let entry_id: i64 = conn
        .query_row(
            "SELECT id FROM archived_entries WHERE entry_uid = ?1",
            [entry_uid.as_str()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(ApiError::not_found("entry not found"))?;
    if database::remove_entry_from_collection(&conn, coll.id, entry_id)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("entry not in collection"))
    }
}

async fn update_entry_visibility_handler(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((archive_id, coll_uid, entry_uid)): Path<(String, String, String)>,
    Json(body): Json<UpdateVisibilityBody>,
) -> Result<StatusCode, ApiError> {
    auth.require_role(ROLE_USER)?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    let coll = database::get_collection_by_uid(&conn, &coll_uid)?
        .ok_or(ApiError::not_found("collection not found"))?;
    let entry_id: i64 = conn
        .query_row(
            "SELECT id FROM archived_entries WHERE entry_uid = ?1",
            [entry_uid.as_str()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(ApiError::not_found("entry not found"))?;
    if database::update_collection_entry_visibility(&conn, coll.id, entry_id, body.visibility_bits)?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("entry not in collection"))
    }
}

async fn list_entry_collections_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path((archive_id, entry_uid)): Path<(String, String)>,
) -> Result<Json<Vec<archive::EntryCollectionMembership>>, ApiError> {
    auth_user.require_auth()?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    match archive::get_entry_collections(&conn, &entry_uid)? {
        Some(memberships) => Ok(Json(memberships)),
        None => Err(ApiError::not_found("entry not found")),
    }
}

async fn patch_collection_handler(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((archive_id, coll_uid)): Path<(String, String)>,
    Json(body): Json<PatchCollectionBody>,
) -> Result<StatusCode, ApiError> {
    auth.require_role(ROLE_USER)?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    let name_ref: Option<&str> = body.name.as_deref();
    let updated =
        database::update_collection(&conn, &coll_uid, name_ref, body.default_visibility_bits)?;
    if updated {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("collection not found"))
    }
}

async fn delete_collection_handler(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((archive_id, coll_uid)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    auth.require_role(ROLE_USER)?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    let deleted = database::delete_collection(&conn, &coll_uid)?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("collection not found"))
    }
}

// ── t.co resolver ──────────────────────────────────────────────────────────────
// POST /api/util/resolve-tco
// Body: JSON array of t.co short URLs (max 50).
// Returns: JSON object mapping each input URL to its expanded destination.
//
// Security:
// - Input restricted to https://t.co/<alphanumeric token> only (no SSRF via input).
// - redirect(Policy::none()): makes ONE HEAD to t.co, reads Location header, never
//   fetches the expanded destination (no open-proxy).
// - 3 s timeout, 1-hop max.
// - No auth required (t.co is public; no data exposed).

static TCO_RE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"^https://t\.co/[A-Za-z0-9]+$").unwrap());

async fn resolve_tco_handler(
    Json(urls): Json<Vec<String>>,
) -> Result<Json<std::collections::HashMap<String, String>>, ApiError> {
    const MAX_BATCH: usize = 50;
    let urls: Vec<String> = urls
        .into_iter()
        .filter(|u| TCO_RE.is_match(u))
        .take(MAX_BATCH)
        .collect();

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|e| ApiError::internal(&format!("http client: {e}")))?;

    let mut map = std::collections::HashMap::new();
    let futs: Vec<_> = urls
        .iter()
        .map(|url| {
            let client = client.clone();
            let url = url.clone();
            tokio::spawn(async move {
                // Try HEAD first; fall back to GET if HEAD returns no Location.
                // Neither follows redirects (Policy::none), so the server only
                // ever connects to t.co itself — never to the destination.
                // Only accept http/https destinations — never javascript:, data:, etc.
                let safe_location = |resp: reqwest::Response| {
                    resp.headers()
                        .get(reqwest::header::LOCATION)
                        .and_then(|v| v.to_str().ok())
                        .map(|s| s.to_string())
                        .filter(|s| s.starts_with("http://") || s.starts_with("https://"))
                };
                let expanded = match client.head(&url).send().await.ok().and_then(safe_location) {
                    Some(loc) => loc,
                    None => match client.get(&url).send().await.ok().and_then(safe_location) {
                        Some(loc) => loc,
                        None => url.clone(),
                    },
                };
                (url, expanded)
            })
        })
        .collect();

    for fut in futs {
        if let Ok((k, v)) = fut.await {
            map.insert(k, v);
        }
    }

    Ok(Json(map))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn make_test_app() -> (Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let auth_path = dir.path().join("auth.sqlite");
        // Seed owner so setup_guard passes in normal tests
        {
            let conn = archivr_core::database::open_auth_db(&auth_path).unwrap();
            archivr_core::database::create_owner(&conn, "testowner", "test_hash_not_real").unwrap();
        }
        let registry = ServerRegistry {
            archives: vec![],
            bind: None,
            auth_db_path: None,
        };
        (app(registry, auth_path), dir)
    }

    fn make_setup_test_app() -> (Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let auth_path = dir.path().join("auth.sqlite");
        // NO owner seeded - for testing setup-required behavior
        let registry = ServerRegistry {
            archives: vec![],
            bind: None,
            auth_db_path: None,
        };
        (app(registry, auth_path), dir)
    }

    fn make_test_registry(
        dir: &tempfile::TempDir,
    ) -> (ServerRegistry, std::path::PathBuf, std::path::PathBuf) {
        let paths = archivr_core::archive::initialize_archive(
            dir.path(),
            &dir.path().join("store"),
            "test",
            false,
        )
        .unwrap();
        let auth_path = dir.path().join("auth.sqlite");
        {
            let conn = archivr_core::database::open_auth_db(&auth_path).unwrap();
            archivr_core::database::create_owner(&conn, "testowner", "dummy").unwrap();
        }
        let registry = ServerRegistry {
            archives: vec![MountedArchive {
                id: "test".to_string(),
                label: "Test".to_string(),
                archive_path: paths.archive_path.clone(),
            }],
            bind: None,
            auth_db_path: None,
        };
        (registry, paths.archive_path, auth_path)
    }

    /// Creates a session for the seeded 'testowner' and returns the cookie string.
    fn make_test_session(auth_path: &std::path::Path) -> String {
        let conn = archivr_core::database::open_auth_db(auth_path).unwrap();
        let user_id: i64 = conn
            .query_row(
                "SELECT id FROM users WHERE username = 'testowner'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let role_bits = archivr_core::database::compute_role_bits(&conn, user_id).unwrap();
        let sess_uid =
            archivr_core::database::create_session(&conn, user_id, role_bits, None).unwrap();
        format!("session={}", sess_uid)
    }

    fn make_test_entry(archive_path: &std::path::Path) -> archivr_core::database::ArchivedEntry {
        let conn = database::open_or_initialize(archive_path).unwrap();
        let user_id = database::ensure_default_user(&conn).unwrap();
        let run = database::create_archive_run(&conn, user_id, 1).unwrap();
        let si = database::upsert_source_identity(
            &conn,
            "web",
            "page",
            None,
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

    #[tokio::test]
    async fn archives_endpoint_lists_mounted_archives() {
        let dir = tempfile::tempdir().unwrap();
        let auth_path = dir.path().join("auth.sqlite");
        {
            let conn = archivr_core::database::open_auth_db(&auth_path).unwrap();
            archivr_core::database::create_owner(&conn, "testowner", "dummy").unwrap();
        }
        let registry = ServerRegistry {
            archives: vec![MountedArchive {
                id: "personal".to_string(),
                label: "Personal".to_string(),
                archive_path: std::path::PathBuf::from("/tmp/personal/.archivr"),
            }],
            bind: None,
            auth_db_path: None,
        };
        let response = app(registry, auth_path)
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
        let dir = tempfile::tempdir().unwrap();
        let auth_path = dir.path().join("auth.sqlite");
        {
            let conn = archivr_core::database::open_auth_db(&auth_path).unwrap();
            archivr_core::database::create_owner(&conn, "testowner", "dummy").unwrap();
        }
        let session_cookie = make_test_session(&auth_path);
        let registry = ServerRegistry {
            archives: vec![],
            bind: None,
            auth_db_path: None,
        };
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/missing/entries")
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn artifact_missing_archive_returns_404() {
        let dir = tempfile::tempdir().unwrap();
        let auth_path = dir.path().join("auth.sqlite");
        {
            let conn = archivr_core::database::open_auth_db(&auth_path).unwrap();
            archivr_core::database::create_owner(&conn, "testowner", "dummy").unwrap();
        }
        let session_cookie = make_test_session(&auth_path);
        let registry = ServerRegistry {
            archives: vec![],
            bind: None,
            auth_db_path: None,
        };
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/nope/entries/entry_abc/artifacts/0")
                    .header("cookie", &session_cookie)
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
        let auth_path = dir.path().join("auth.sqlite");
        {
            let conn = archivr_core::database::open_auth_db(&auth_path).unwrap();
            archivr_core::database::create_owner(&conn, "testowner", "dummy").unwrap();
        }
        let session_cookie = make_test_session(&auth_path);
        let registry = ServerRegistry {
            archives: vec![MountedArchive {
                id: "test".to_string(),
                label: "Test".to_string(),
                archive_path,
            }],
            bind: None,
            auth_db_path: None,
        };
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/entries/entry_doesnotexist/artifacts/0")
                    .header("cookie", &session_cookie)
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
        let auth_path = dir.path().join("auth.sqlite");
        {
            let conn = archivr_core::database::open_auth_db(&auth_path).unwrap();
            archivr_core::database::create_owner(&conn, "testowner", "dummy").unwrap();
        }
        let session_cookie = make_test_session(&auth_path);
        let registry = ServerRegistry {
            archives: vec![MountedArchive {
                id: "test".to_string(),
                label: "Test".to_string(),
                archive_path,
            }],
            bind: None,
            auth_db_path: None,
        };
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/entries/entry_doesnotexist/artifacts/99")
                    .header("cookie", &session_cookie)
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

        let auth_path = dir.path().join("auth.sqlite");
        {
            let conn = archivr_core::database::open_auth_db(&auth_path).unwrap();
            archivr_core::database::create_owner(&conn, "testowner", "dummy").unwrap();
        }
        let session_cookie = make_test_session(&auth_path);
        let registry = ServerRegistry {
            archives: vec![MountedArchive {
                id: "test".to_string(),
                label: "Test".to_string(),
                archive_path: paths.archive_path.clone(),
            }],
            bind: None,
            auth_db_path: None,
        };
        let uri = format!("/api/archives/test/entries/{}/artifacts/0", entry.entry_uid);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri(&uri)
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn search_missing_archive_returns_404() {
        let dir = tempfile::tempdir().unwrap();
        let auth_path = dir.path().join("auth.sqlite");
        {
            let conn = archivr_core::database::open_auth_db(&auth_path).unwrap();
            archivr_core::database::create_owner(&conn, "testowner", "dummy").unwrap();
        }
        let session_cookie = make_test_session(&auth_path);
        let registry = ServerRegistry {
            archives: vec![],
            bind: None,
            auth_db_path: None,
        };
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/nope/entries/search?q=anything")
                    .header("cookie", &session_cookie)
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
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/entries/search")
                    .header("cookie", &session_cookie)
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
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/entries/search?q=unknownprefix%3Aval")
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    // ---- tag route tests ----

    #[tokio::test]
    async fn test_list_tags_unknown_archive() {
        let dir = tempfile::tempdir().unwrap();
        let auth_path = dir.path().join("auth.sqlite");
        {
            let conn = archivr_core::database::open_auth_db(&auth_path).unwrap();
            archivr_core::database::create_owner(&conn, "testowner", "dummy").unwrap();
        }
        let session_cookie = make_test_session(&auth_path);
        let registry = ServerRegistry {
            archives: vec![],
            bind: None,
            auth_db_path: None,
        };
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/ghost/tags")
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_create_tag_unknown_archive() {
        let (test_app, _dir) = make_test_app();
        let response = test_app
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
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED); // auth fires before archive lookup
    }

    #[tokio::test]
    async fn test_create_tag_empty_path() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let response = app(registry, auth_path)
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
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED); // auth fires before validation
    }

    #[tokio::test]
    async fn test_tag_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);

        let create_response = app(registry.clone(), auth_path.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/archives/test/tags")
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(json_body(&serde_json::json!({"path": "/science"})))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create_response.status(), StatusCode::CREATED);
        let list_response = app(registry.clone(), auth_path.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/tags")
                    .header("cookie", &session_cookie)
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
        assert!(
            slugs.contains(&"science"),
            "expected 'science' in tag tree, got {slugs:?}"
        );
    }

    #[tokio::test]
    async fn test_entry_tag_assign_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let entry = make_test_entry(&archive_path);
        let entry_uid = entry.entry_uid.clone();
        let entry_tags_uri = format!("/api/archives/test/entries/{entry_uid}/tags");
        let session_cookie = make_test_session(&auth_path);

        // Assign tag
        let assign_response = app(registry.clone(), auth_path.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(&entry_tags_uri)
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(json_body(&serde_json::json!({"tag_path": "/science"})))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(assign_response.status(), StatusCode::CREATED);
        let assigned_tag = body_json(assign_response).await;
        let tag_uid = assigned_tag["tag_uid"].as_str().unwrap().to_string();

        // List entry tags — should contain the assigned tag
        let list_response = app(registry.clone(), auth_path.clone())
            .oneshot(
                Request::builder()
                    .uri(&entry_tags_uri)
                    .header("cookie", &session_cookie)
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
        let delete_response = app(registry.clone(), auth_path.clone())
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(&delete_uri)
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(delete_response.status(), StatusCode::NO_CONTENT);

        // List entry tags again — should be empty
        let list2_response = app(registry.clone(), auth_path.clone())
            .oneshot(
                Request::builder()
                    .uri(&entry_tags_uri)
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list2_response.status(), StatusCode::OK);
        let tags2 = body_json(list2_response).await;
        assert!(
            tags2.as_array().unwrap().is_empty(),
            "tags should be empty after removal"
        );
    }

    #[tokio::test]
    async fn test_search_with_tag_param() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let entry = make_test_entry(&archive_path);
        let entry_uid = entry.entry_uid.clone();
        let session_cookie = make_test_session(&auth_path);

        // Assign /science tag to entry
        let assign_resp = app(registry.clone(), auth_path.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/archives/test/entries/{entry_uid}/tags"))
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(json_body(&serde_json::json!({"tag_path": "/science"})))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            assign_resp.status(),
            StatusCode::CREATED,
            "assign tag should return 201"
        );

        // Search with ?tag=/science — entry should appear (requires auth since entry is private)
        let response = app(registry.clone(), auth_path.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/entries/search?tag=%2Fscience")
                    .header("cookie", &session_cookie)
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
        let response2 = app(registry.clone(), auth_path.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/entries/search?tag=%2Fart")
                    .header("cookie", &session_cookie)
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
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/entries/ghost_uid/tags")
                    .header("cookie", &session_cookie)
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
        let (registry, _, auth_path) = make_test_registry(&dir);
        let response = app(registry, auth_path)
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
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED); // auth fires before entry lookup
    }

    #[tokio::test]
    async fn test_assign_entry_tag_empty_tag_path() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let entry = make_test_entry(&archive_path);
        let entry_uid = entry.entry_uid.clone();
        let response = app(registry, auth_path)
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
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED); // auth fires before validation
    }

    #[tokio::test]
    async fn test_remove_entry_tag_unknown_entry() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/archives/test/entries/ghost_uid/tags/ghost_tag_uid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED); // auth fires before entry lookup
    }

    #[tokio::test]
    async fn capture_rejects_empty_locator() {
        let (test_app, _dir) = make_test_app();
        let response = test_app
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
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED); // auth fires before validation
    }

    #[tokio::test]
    async fn capture_rejects_unknown_archive() {
        let (test_app, _dir) = make_test_app();
        let response = test_app
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
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED); // auth fires before archive lookup
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
        let auth_path = dir.path().join("auth.sqlite");
        {
            let conn = archivr_core::database::open_auth_db(&auth_path).unwrap();
            archivr_core::database::create_owner(&conn, "testowner", "dummy").unwrap();
        }
        let session_cookie = make_test_session(&auth_path);
        let registry = ServerRegistry {
            archives: vec![MountedArchive {
                id: "test".to_string(),
                label: "Test".to_string(),
                archive_path,
            }],
            bind: None,
            auth_db_path: None,
        };
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/blobs/0000000000000000000000000000000000000000000000000000000000000000")
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn setup_required_before_owner_created() {
        let (test_app, _dir) = make_setup_test_app();
        let response = test_app
            .oneshot(
                Request::builder()
                    .uri("/api/auth/setup")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["setup_required"], true);
    }

    #[tokio::test]
    async fn setup_post_returns_409_on_repeat() {
        let dir = tempfile::tempdir().unwrap();
        let auth_path = dir.path().join("auth.sqlite");
        // Seed an owner directly so the second POST hits CONFLICT
        {
            let conn = archivr_core::database::open_auth_db(&auth_path).unwrap();
            archivr_core::database::create_owner(&conn, "owner", "dummy").unwrap();
        }
        let registry = ServerRegistry {
            archives: vec![],
            bind: None,
            auth_db_path: None,
        };
        let second_app = app(registry, auth_path);
        let r2 = second_app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/setup")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"username":"owner2","password":"hunter2!"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r2.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn login_wrong_password_returns_401() {
        let dir = tempfile::tempdir().unwrap();
        let auth_path = dir.path().join("auth.sqlite");
        {
            let conn = archivr_core::database::open_auth_db(&auth_path).unwrap();
            let hash = crate::auth::hash_password("correct_password").unwrap();
            archivr_core::database::create_owner(&conn, "owner", &hash).unwrap();
        }
        let registry = ServerRegistry {
            archives: vec![],
            bind: None,
            auth_db_path: None,
        };
        let response = app(registry, auth_path)
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
    async fn create_token_requires_auth() {
        let (test_app, _dir) = make_test_app();
        let response = test_app
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

    #[tokio::test]
    async fn capture_returns_401_for_unauthenticated() {
        let (test_app, _dir) = make_test_app();
        let response = test_app
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

    #[tokio::test]
    async fn capture_post_returns_accepted_with_job_uid() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/archives/test/captures")
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(Body::from(r#"{"locator":"local:/nonexistent"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            json["job_uid"].as_str().is_some(),
            "response must have job_uid"
        );
        assert_eq!(json["status"], "pending");
    }

    #[tokio::test]
    async fn capture_with_valid_quality_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/archives/test/captures")
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(Body::from(
                        r#"{"locator":"local:/nonexistent","quality":"720p"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            json["job_uid"].as_str().is_some(),
            "response must have job_uid"
        );
        assert_eq!(json["status"], "pending");
    }

    #[tokio::test]
    async fn capture_with_invalid_quality_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/archives/test/captures")
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(Body::from(
                        r#"{"locator":"local:/nonexistent","quality":"4K"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            json["error"]
                .as_str()
                .is_some_and(|e| e.contains("invalid quality"))
        );
    }

    #[tokio::test]
    async fn probe_requires_auth() {
        let (test_app, _dir) = make_test_app();
        let response = test_app
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/captures/probe?locator=local%3A%2Fnonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn probe_non_video_locator_returns_has_video_false() {
        // local:/nonexistent is not a yt-dlp source — the handler returns
        // immediately without spawning yt-dlp, so this is fast and deterministic.
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/captures/probe?locator=local%3A%2Fnonexistent")
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["has_video"], false);
        assert_eq!(json["qualities"], serde_json::json!([]));
        assert_eq!(json["has_audio"], false);
    }

    #[tokio::test]
    async fn admin_users_requires_admin_role() {
        let (test_app, _dir) = make_test_app();
        let response = test_app
            .oneshot(
                Request::builder()
                    .uri("/api/admin/users")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn admin_list_users_returns_ok_for_admin() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/admin/users")
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_me_returns_display_name_field() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/auth/me")
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            json.get("display_name").is_some(),
            "auth/me must include display_name field"
        );
        assert!(json.get("username").is_some());
    }

    #[tokio::test]
    async fn patch_me_updates_display_name() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/auth/me")
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(Body::from(r#"{"display_name":"Test Owner"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn patch_me_requires_auth() {
        let (test_app, _dir) = make_test_app();
        let response = test_app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/auth/me")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"display_name":"anon"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn patch_me_rejects_wrong_current_password() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        // Set a real password hash on the owner
        {
            let conn = archivr_core::database::open_auth_db(&auth_path).unwrap();
            let hash = crate::auth::hash_password("real_password").unwrap();
            let user_id: i64 = conn
                .query_row(
                    "SELECT id FROM users WHERE username = 'testowner'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            archivr_core::database::update_user_password(&conn, user_id, &hash).unwrap();
        }
        let session_cookie = make_test_session(&auth_path);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/auth/me")
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(Body::from(
                        r#"{"current_password":"wrong","new_password":"newpass"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn instance_settings_requires_admin() {
        let (test_app, _dir) = make_test_app();
        let response = test_app
            .oneshot(
                Request::builder()
                    .uri("/api/admin/instance-settings")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn instance_settings_get_returns_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/admin/instance-settings")
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["public_index_enabled"], false);
        assert_eq!(json["open_registration_enabled"], false);
    }

    #[tokio::test]
    async fn instance_settings_patch_updates_fields() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/admin/instance-settings")
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(Body::from(r#"{"open_registration_enabled":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
    #[tokio::test]
    async fn cookie_rules_require_admin() {
        // Non-admin (no session) should get 401.
        let (test_app, _dir) = make_test_app();
        let response = test_app
            .oneshot(
                Request::builder()
                    .uri("/api/admin/cookie-rules")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn cookie_rules_create_list_delete() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session = make_test_session(&auth_path);

        let app = app(registry, auth_path);

        // Create a global rule with valid string-only cookies.
        let create_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/admin/cookie-rules")
                    .header("content-type", "application/json")
                    .header("cookie", &session)
                    .body(Body::from(
                        r#"{"pattern_kind":"global","cookies_json":"{\"session\":\"abc\"}"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create_resp.status(), StatusCode::CREATED);
        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(create_resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        let rule_uid = body["rule_uid"].as_str().unwrap().to_string();
        assert_eq!(body["pattern_kind"], "global");

        // List: should contain the created rule.
        let list_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/admin/cookie-rules")
                    .header("cookie", &session)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list_resp.status(), StatusCode::OK);
        let list: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(list_resp.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(list.as_array().unwrap().len(), 1);

        // Delete.
        let del_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/admin/cookie-rules/{rule_uid}"))
                    .header("cookie", &session)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(del_resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn cookie_rules_rejects_non_string_values() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session = make_test_session(&auth_path);

        // cookies_json with a numeric value must be rejected — core only accepts string values.
        let resp = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/admin/cookie-rules")
                    .header("content-type", "application/json")
                    .header("cookie", &session)
                    .body(Body::from(
                        r#"{"pattern_kind":"global","cookies_json":"{\"session\":123}"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn cookie_rules_rejects_invalid_regex() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session = make_test_session(&auth_path);

        let resp = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/admin/cookie-rules")
                    .header("content-type", "application/json")
                    .header("cookie", &session)
                    .body(Body::from(r#"{"pattern_kind":"regex","url_pattern":"[invalid","cookies_json":"{\"x\":\"y\"}"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
    #[tokio::test]
    async fn security_headers_present_on_success_response() {
        let (test_app, _dir) = make_test_app();
        let response = test_app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("x-content-type-options").unwrap(),
            "nosniff"
        );
        assert_eq!(response.headers().get("x-frame-options").unwrap(), "DENY");
        assert_eq!(
            response.headers().get("referrer-policy").unwrap(),
            "strict-origin-when-cross-origin"
        );
        assert!(
            response.headers().get("content-security-policy").is_some(),
            "content-security-policy header must be present"
        );
        assert!(
            response.headers().get("permissions-policy").is_some(),
            "permissions-policy header must be present"
        );
    }

    #[tokio::test]
    async fn security_headers_present_on_error_response() {
        let (test_app, _dir) = make_test_app();
        let response = test_app
            .oneshot(
                Request::builder()
                    .uri("/api/archives/nosucharchive/entries")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(response.status(), StatusCode::OK);
        assert!(response.headers().get("x-content-type-options").is_some());
        assert!(response.headers().get("x-frame-options").is_some());
        assert!(response.headers().get("referrer-policy").is_some());
        assert!(response.headers().get("content-security-policy").is_some());
        assert!(response.headers().get("permissions-policy").is_some());
    }

    #[tokio::test]
    async fn login_rate_limit_blocks_after_max_attempts() {
        let dir = tempfile::tempdir().unwrap();
        let auth_path = dir.path().join("auth.sqlite");
        {
            let conn = archivr_core::database::open_auth_db(&auth_path).unwrap();
            archivr_core::database::create_owner(&conn, "testowner", "dummy").unwrap();
        }
        let state = AppState {
            registry: Arc::new(ServerRegistry {
                archives: vec![],
                bind: None,
                auth_db_path: None,
            }),
            auth_db_path: Arc::new(auth_path),
            login_attempts: Arc::new(Mutex::new(HashMap::new())),
        };
        let bad_creds = serde_json::json!({ "username": "nobody", "password": "wrong" });
        for _ in 0..LOGIN_MAX_ATTEMPTS {
            let resp = app_with_state(state.clone())
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/auth/login")
                        .header("content-type", "application/json")
                        .header("x-forwarded-for", "10.0.0.1")
                        .body(json_body(&bad_creds))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::UNAUTHORIZED,
                "attempt within limit should reach handler"
            );
        }
        let resp = app_with_state(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/login")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", "10.0.0.1")
                    .body(json_body(&bad_creds))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "attempt over limit must be 429"
        );
        assert!(
            resp.headers().contains_key("retry-after"),
            "429 must carry Retry-After header"
        );
        let body = body_json(resp).await;
        assert_eq!(body["error"], "rate_limited");
        assert!(body["retry_after_secs"].as_i64().unwrap() > 0);
    }

    #[tokio::test]
    async fn login_rate_limit_does_not_affect_other_routes() {
        let dir = tempfile::tempdir().unwrap();
        let auth_path = dir.path().join("auth.sqlite");
        {
            let conn = archivr_core::database::open_auth_db(&auth_path).unwrap();
            archivr_core::database::create_owner(&conn, "testowner", "dummy").unwrap();
        }
        let state = AppState {
            registry: Arc::new(ServerRegistry {
                archives: vec![],
                bind: None,
                auth_db_path: None,
            }),
            auth_db_path: Arc::new(auth_path),
            login_attempts: Arc::new(Mutex::new(HashMap::new())),
        };
        let bad_creds = serde_json::json!({ "username": "x", "password": "y" });
        for _ in 0..LOGIN_MAX_ATTEMPTS {
            app_with_state(state.clone())
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/auth/login")
                        .header("content-type", "application/json")
                        .header("x-forwarded-for", "10.0.0.2")
                        .body(json_body(&bad_creds))
                        .unwrap(),
                )
                .await
                .unwrap();
        }
        let resp = app_with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "/health must be unaffected");
    }

    // ── Task 1: read-endpoint auth enforcement ────────────────────────────────

    #[tokio::test]
    async fn entry_detail_requires_auth() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/entries/fake_uid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn entry_detail_with_auth_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let entry = make_test_entry(&archive_path);
        let session_cookie = make_test_session(&auth_path);
        let uri = format!("/api/archives/test/entries/{}", entry.entry_uid);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri(&uri)
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn list_runs_requires_auth() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/runs")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_runs_with_auth_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/runs")
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn serve_artifact_requires_auth() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/entries/fake_uid/artifacts/0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn serve_artifact_with_auth_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().join("store");
        let paths =
            archivr_core::archive::initialize_archive(dir.path(), &store_path, "test", false)
                .unwrap();
        let artifact_relpath = "raw/a/u/page.html";
        let artifact_dir = store_path.join("raw").join("a").join("u");
        std::fs::create_dir_all(&artifact_dir).unwrap();
        std::fs::write(artifact_dir.join("page.html"), b"<html>auth test</html>").unwrap();
        let conn = database::open_or_initialize(&paths.archive_path).unwrap();
        let user_id = database::ensure_default_user(&conn).unwrap();
        let sid = database::upsert_source_identity(
            &conn,
            "web",
            "page",
            Some("auth-page"),
            Some("https://example.com/auth"),
            "https://example.com/auth",
        )
        .unwrap();
        let run = database::create_archive_run(&conn, user_id, 1).unwrap();
        let entry = database::create_archived_entry(
            &conn,
            &database::NewEntry {
                source_identity_id: sid,
                archive_run_id: run.id,
                parent_entry_id: None,
                root_entry_id: None,
                created_by_user_id: user_id,
                owned_by_user_id: user_id,
                source_kind: "web".to_string(),
                entity_kind: "page".to_string(),
                title: Some("Auth Test Page".to_string()),
                visibility: "private".to_string(),
                representation_kind: "html".to_string(),
                source_metadata_json: "{}".to_string(),
                display_metadata_json: None,
            },
        )
        .unwrap();
        let blob_id = database::upsert_blob(
            &conn,
            &database::BlobRecord {
                sha256: "aaaa1111bbbb2222cccc3333dddd4444aaaa1111bbbb2222cccc3333dddd4444"
                    .to_string(),
                byte_size: 21,
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
        drop(conn);
        let auth_path = dir.path().join("auth.sqlite");
        {
            let conn = archivr_core::database::open_auth_db(&auth_path).unwrap();
            archivr_core::database::create_owner(&conn, "testowner", "dummy").unwrap();
        }
        let session_cookie = make_test_session(&auth_path);
        let registry = ServerRegistry {
            archives: vec![MountedArchive {
                id: "test".to_string(),
                label: "Test".to_string(),
                archive_path: paths.archive_path.clone(),
            }],
            bind: None,
            auth_db_path: None,
        };
        let uri = format!("/api/archives/test/entries/{}/artifacts/0", entry.entry_uid);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri(&uri)
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn serve_entry_favicon_requires_auth() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/entries/fake_uid/favicon")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn serve_entry_favicon_with_auth_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().join("store");
        let paths =
            archivr_core::archive::initialize_archive(dir.path(), &store_path, "test", false)
                .unwrap();
        let favicon_relpath = "raw/f/a/favicon.png";
        let favicon_dir = store_path.join("raw").join("f").join("a");
        std::fs::create_dir_all(&favicon_dir).unwrap();
        std::fs::write(
            favicon_dir.join("favicon.png"),
            &[0x89u8, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A],
        )
        .unwrap();
        let conn = database::open_or_initialize(&paths.archive_path).unwrap();
        let user_id = database::ensure_default_user(&conn).unwrap();
        let sid = database::upsert_source_identity(
            &conn,
            "web",
            "page",
            Some("fav-page"),
            Some("https://example.com/fav"),
            "https://example.com/fav",
        )
        .unwrap();
        let run = database::create_archive_run(&conn, user_id, 1).unwrap();
        let entry = database::create_archived_entry(
            &conn,
            &database::NewEntry {
                source_identity_id: sid,
                archive_run_id: run.id,
                parent_entry_id: None,
                root_entry_id: None,
                created_by_user_id: user_id,
                owned_by_user_id: user_id,
                source_kind: "web".to_string(),
                entity_kind: "page".to_string(),
                title: Some("Favicon Test".to_string()),
                visibility: "private".to_string(),
                representation_kind: "html".to_string(),
                source_metadata_json: "{}".to_string(),
                display_metadata_json: None,
            },
        )
        .unwrap();
        let blob_id = database::upsert_blob(
            &conn,
            &database::BlobRecord {
                sha256: "ffffffffffff1111ffffffffffff1111ffffffffffff1111ffffffffffff1111"
                    .to_string(),
                byte_size: 8,
                mime_type: Some("image/png".to_string()),
                extension: Some("png".to_string()),
                raw_relpath: favicon_relpath.to_string(),
            },
        )
        .unwrap();
        database::add_entry_artifact(
            &conn,
            &database::NewArtifact {
                entry_id: entry.id,
                artifact_role: "favicon".to_string(),
                storage_area: "raw".to_string(),
                relpath: favicon_relpath.to_string(),
                blob_id: Some(blob_id),
                logical_path: None,
                metadata_json: None,
            },
        )
        .unwrap();
        drop(conn);
        let auth_path = dir.path().join("auth.sqlite");
        {
            let conn = archivr_core::database::open_auth_db(&auth_path).unwrap();
            archivr_core::database::create_owner(&conn, "testowner", "dummy").unwrap();
        }
        let session_cookie = make_test_session(&auth_path);
        let registry = ServerRegistry {
            archives: vec![MountedArchive {
                id: "test".to_string(),
                label: "Test".to_string(),
                archive_path: paths.archive_path.clone(),
            }],
            bind: None,
            auth_db_path: None,
        };
        let uri = format!("/api/archives/test/entries/{}/favicon", entry.entry_uid);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri(&uri)
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn serve_blob_requires_auth() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let sha256 = "0000000000000000000000000000000000000000000000000000000000000000";
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri(&format!("/api/archives/test/blobs/{sha256}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn serve_blob_with_auth_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().join("store");
        let paths =
            archivr_core::archive::initialize_archive(dir.path(), &store_path, "test", false)
                .unwrap();
        let blob_relpath = "raw/b/l/data.bin";
        let blob_dir = store_path.join("raw").join("b").join("l");
        std::fs::create_dir_all(&blob_dir).unwrap();
        std::fs::write(blob_dir.join("data.bin"), b"blob content here").unwrap();
        let sha256 = "bbbb2222cccc4444bbbb2222cccc4444bbbb2222cccc4444bbbb2222cccc4444";
        let conn = database::open_or_initialize(&paths.archive_path).unwrap();
        database::upsert_blob(
            &conn,
            &database::BlobRecord {
                sha256: sha256.to_string(),
                byte_size: 17,
                mime_type: Some("application/octet-stream".to_string()),
                extension: Some("bin".to_string()),
                raw_relpath: blob_relpath.to_string(),
            },
        )
        .unwrap();
        drop(conn);
        let auth_path = dir.path().join("auth.sqlite");
        {
            let conn = archivr_core::database::open_auth_db(&auth_path).unwrap();
            archivr_core::database::create_owner(&conn, "testowner", "dummy").unwrap();
        }
        let session_cookie = make_test_session(&auth_path);
        let registry = ServerRegistry {
            archives: vec![MountedArchive {
                id: "test".to_string(),
                label: "Test".to_string(),
                archive_path: paths.archive_path.clone(),
            }],
            bind: None,
            auth_db_path: None,
        };
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri(&format!("/api/archives/test/blobs/{sha256}"))
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn list_tags_requires_auth() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/tags")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_tags_with_auth_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/tags")
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn list_entry_tags_requires_auth() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/entries/fake_uid/tags")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_entry_tags_with_auth_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let entry = make_test_entry(&archive_path);
        let session_cookie = make_test_session(&auth_path);
        let uri = format!("/api/archives/test/entries/{}/tags", entry.entry_uid);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri(&uri)
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn list_collections_requires_auth() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/collections")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_collections_with_auth_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/collections")
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn list_entry_collections_requires_auth() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/entries/fake_uid/collections")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_entry_collections_with_auth_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let entry = make_test_entry(&archive_path);
        let session_cookie = make_test_session(&auth_path);
        let uri = format!("/api/archives/test/entries/{}/collections", entry.entry_uid);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri(&uri)
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn get_collection_requires_auth() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/collections/coll_notexist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn get_collection_with_auth_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let create_resp = app(registry.clone(), auth_path.clone())
            .oneshot(Request::builder().method("POST").uri("/api/archives/test/collections")
                .header("content-type", "application/json").header("cookie", &session_cookie)
                .body(json_body(&serde_json::json!({"name": "Auth Test Collection", "slug": "auth-test-coll", "default_visibility_bits": 2})))
                .unwrap()).await.unwrap();
        assert_eq!(create_resp.status(), StatusCode::CREATED);
        let coll = body_json(create_resp).await;
        let coll_uid = coll["collection_uid"].as_str().unwrap().to_string();
        let uri = format!("/api/archives/test/collections/{coll_uid}");
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri(&uri)
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    // ── Task 2: list_entries / search_entries auth enforcement ───────────────

    #[tokio::test]
    async fn list_entries_requires_auth() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/entries")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_entries_with_auth_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/entries")
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn search_entries_requires_auth() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/entries/search")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn search_entries_with_auth_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/entries/search")
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn patch_entry_title_requires_auth() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/archives/test/entries/nonexistent")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"title":"New Title"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn patch_entry_title_persists_and_reflects_in_list() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let entry = make_test_entry(&archive_path);

        // PATCH the title
        let response = app(registry.clone(), auth_path.clone())
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(&format!("/api/archives/test/entries/{}", entry.entry_uid))
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(Body::from(r#"{"title":"Renamed Title"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // Verify via entry detail
        let get_resp = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri(&format!("/api/archives/test/entries/{}", entry.entry_uid))
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get_resp.status(), StatusCode::OK);
        let json = body_json(get_resp).await;
        assert_eq!(json["summary"]["title"], "Renamed Title");
    }

    #[tokio::test]
    async fn patch_entry_title_returns_404_for_unknown_uid() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/archives/test/entries/no-such-uid")
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(Body::from(r#"{"title":"Anything"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn auth_me_returns_humanize_slugs_false_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/auth/me")
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        assert_eq!(
            json["humanize_slugs"], false,
            "humanize_slugs must default to false for new users"
        );
    }

    #[tokio::test]
    async fn patch_me_humanize_slugs_persists() {
        let dir = tempfile::tempdir().unwrap();
        let auth_path = dir.path().join("auth.sqlite");
        {
            let conn = archivr_core::database::open_auth_db(&auth_path).unwrap();
            archivr_core::database::create_owner(&conn, "testowner", "dummy").unwrap();
        }
        let state = AppState {
            registry: Arc::new(ServerRegistry {
                archives: vec![],
                bind: None,
                auth_db_path: None,
            }),
            auth_db_path: Arc::new(auth_path.clone()),
            login_attempts: Arc::new(Mutex::new(HashMap::new())),
        };
        let session_cookie = make_test_session(&auth_path);

        // PATCH humanize_slugs = true
        let patch_resp = app_with_state(state.clone())
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/auth/me")
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(Body::from(r#"{"humanize_slugs":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(patch_resp.status(), StatusCode::NO_CONTENT);

        // GET /api/auth/me — must now return humanize_slugs: true
        let get_resp = app_with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/api/auth/me")
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get_resp.status(), StatusCode::OK);
        let json = body_json(get_resp).await;
        assert_eq!(
            json["humanize_slugs"], true,
            "humanize_slugs must be true after PATCH"
        );
    }

    #[tokio::test]
    async fn delete_entry_returns_204_and_entry_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let session = make_test_session(&auth_path);
        let entry = make_test_entry(&archive_path);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/archives/test/entries/{}", entry.entry_uid))
                    .header("cookie", &session)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        // Confirm the row is actually gone from the DB.
        let conn = database::open_or_initialize(&archive_path).unwrap();
        let exists: Option<i64> = conn
            .query_row(
                "SELECT id FROM archived_entries WHERE entry_uid = ?1",
                [&entry.entry_uid],
                |r| r.get(0),
            )
            .optional()
            .unwrap();
        assert!(exists.is_none(), "entry row should be deleted");
    }

    #[tokio::test]
    async fn delete_entry_returns_404_for_missing_entry() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _archive_path, auth_path) = make_test_registry(&dir);
        let session = make_test_session(&auth_path);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/archives/test/entries/entry_doesnotexist")
                    .header("cookie", &session)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_entry_requires_auth() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let entry = make_test_entry(&archive_path);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/archives/test/entries/{}", entry.entry_uid))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    // ── Blob cleanup route tests ──────────────────────────────────────────────────

    #[tokio::test]
    async fn blob_cleanup_scan_requires_auth() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/blob-cleanup")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn blob_cleanup_delete_requires_auth() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/archives/test/blob-cleanup")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn blob_cleanup_scan_returns_409_when_capture_pending() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let session = make_test_session(&auth_path);
        {
            let conn = database::open_or_initialize(&archive_path).unwrap();
            database::create_capture_job(&conn, "test").unwrap();
        }
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/archives/test/blob-cleanup")
                    .header("cookie", &session)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn blob_cleanup_delete_returns_409_when_capture_pending() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let session = make_test_session(&auth_path);
        {
            let conn = database::open_or_initialize(&archive_path).unwrap();
            database::create_capture_job(&conn, "test").unwrap();
        }
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/archives/test/blob-cleanup")
                    .header("cookie", &session)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn blob_cleanup_delete_removes_orphan_and_preserves_referenced() {
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().join("store");
        let paths =
            archivr_core::archive::initialize_archive(dir.path(), &store_path, "test", false)
                .unwrap();
        let auth_path = dir.path().join("auth.sqlite");
        {
            let conn = archivr_core::database::open_auth_db(&auth_path).unwrap();
            archivr_core::database::create_owner(&conn, "testowner", "dummy").unwrap();
        }
        let session = make_test_session(&auth_path);
        let entry = make_test_entry(&paths.archive_path);

        let live_relpath = "raw/l/i/live.bin";
        let orphan_relpath = "raw/o/r/orphan.bin";
        let extra_relpath = "raw/x/t/extra.bin"; // on disk only, no blob row

        {
            let conn = database::open_or_initialize(&paths.archive_path).unwrap();
            // Referenced blob
            let live_id = database::upsert_blob(
                &conn,
                &database::BlobRecord {
                    sha256: "aaaa1111bbbb2222cccc3333dddd4444aaaa1111bbbb2222cccc3333dddd4444"
                        .to_string(),
                    byte_size: 10,
                    mime_type: None,
                    extension: Some("bin".to_string()),
                    raw_relpath: live_relpath.to_string(),
                },
            )
            .unwrap();
            database::add_entry_artifact(
                &conn,
                &database::NewArtifact {
                    entry_id: entry.id,
                    artifact_role: "main".to_string(),
                    storage_area: "raw".to_string(),
                    relpath: live_relpath.to_string(),
                    blob_id: Some(live_id),
                    logical_path: None,
                    metadata_json: None,
                },
            )
            .unwrap();
            // Orphaned blob (no artifact references it)
            database::upsert_blob(
                &conn,
                &database::BlobRecord {
                    sha256: "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
                        .to_string(),
                    byte_size: 20,
                    mime_type: None,
                    extension: Some("bin".to_string()),
                    raw_relpath: orphan_relpath.to_string(),
                },
            )
            .unwrap();
        }

        // Write all three files to disk
        for relpath in &[live_relpath, orphan_relpath, extra_relpath] {
            let abs = store_path.join(relpath);
            std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
            std::fs::write(&abs, b"content").unwrap();
        }

        let registry = ServerRegistry {
            archives: vec![MountedArchive {
                id: "test".to_string(),
                label: "Test".to_string(),
                archive_path: paths.archive_path.clone(),
            }],
            bind: None,
            auth_db_path: None,
        };

        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/api/archives/test/blob-cleanup")
                    .header("cookie", &session)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["deleted_blob_rows"], 1, "one orphaned DB row removed");
        assert_eq!(
            body["deleted_files"], 2,
            "orphan blob file and extra disk file removed"
        );
        assert!(body["errors"].as_array().unwrap().is_empty());

        assert!(
            store_path.join(live_relpath).exists(),
            "referenced file must be preserved"
        );
        assert!(
            !store_path.join(orphan_relpath).exists(),
            "orphaned blob file must be deleted"
        );
        assert!(
            !store_path.join(extra_relpath).exists(),
            "extra disk-only file must be deleted"
        );
    }
}
