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
//   PERMISSION  — authenticated + caller role_bits ∩ instance-settings mask (else 403):
//                   PUT /api/archives/:id/entries/:uid/children/order
//                   (mask = reorder_children_role_bits, default ADMIN|OWNER)
//   ADMIN       — requires ROLE_ADMIN: /api/admin/* (users, roles, cookie-rules)
//   OWNER       — requires ROLE_OWNER:
//                   changing reorder_children_role_bits via PATCH /api/admin/instance-settings
//   AUTH_SELF   — own resources, require_auth() only:
//                   GET/POST/DELETE /api/auth/tokens
//                   POST /api/auth/logout, GET/PATCH /api/auth/me
//   SETTINGS    — instance settings:
//                   GET/PATCH /api/admin/instance-settings (ROLE_ADMIN; the reorder mask field is OWNER-only)
//                   GET /api/admin/yt-dlp, POST /api/admin/yt-dlp/update (ROLE_ADMIN; 409 while an update runs)
// ────────────────────────────────────────────────────────────────────────────

use parking_lot::Mutex;
use std::{
    collections::{HashMap, VecDeque},
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use archivr_core::{archive, capture, database, downloader, summarizer, text_title, thread_title};
use axum::{
    Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, Multipart, Path, Query, Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post, put},
};
use tower::ServiceExt;
use tower_http::services::{ServeDir, ServeFile};

use crate::auth;
pub use crate::auth::{AuthUser, ROLE_ADMIN, ROLE_GUEST, ROLE_OWNER, ROLE_USER};
use crate::registry::{MountedArchive, ServerRegistry};
use axum_extra::extract::CookieJar;
use rusqlite::OptionalExtension;
use crate::jobs;

const LOGIN_WINDOW: Duration = Duration::from_secs(15 * 60);
const LOGIN_MAX_ATTEMPTS: usize = 5;
const MAX_TEXT_CAPTURE_BODY_BYTES: usize = 2 * 1024 * 1024;
// JSON can expand each body byte into a six-byte `\\u00XX` escape sequence,
// plus a small request envelope.
const MAX_TEXT_CAPTURE_REQUEST_BYTES: usize = MAX_TEXT_CAPTURE_BODY_BYTES * 6 + 64 * 1024;

// Short-lived token granting unauthenticated access to one specific artifact.
// Used so Cast / AirPlay devices (which carry no session cookie) can fetch media.
pub(crate) struct MediaToken {
    archive_id: String,
    entry_uid: String,
    artifact_index: usize,
    expires_at: std::time::Instant,
}

#[derive(Clone)]
pub struct AppState {
    pub(crate) registry: Arc<ServerRegistry>,
    pub auth_db_path: Arc<std::path::PathBuf>,
    pub login_attempts: Arc<Mutex<HashMap<IpAddr, VecDeque<Instant>>>>,
    pub media_tokens: Arc<Mutex<HashMap<String, MediaToken>>>,
}

#[derive(Debug, serde::Deserialize, Default)]
pub struct EntrySearchParams {
    pub q: Option<String>,
    pub tag: Option<String>,
    pub collection: Option<String>,
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
                 script-src 'self' https://www.gstatic.com; \
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
            "/api/archives/:archive_id/entries/:entry_uid/children/order",
            put(reorder_entry_children_handler),
        )
        .route(
            "/api/archives/:archive_id/entries/:entry_uid/artifacts/:artifact_index",
            get(serve_artifact),
        )
        .route(
            "/api/archives/:archive_id/entries/:entry_uid/artifacts/:artifact_index/media-token",
            post(issue_media_token),
        )
        .route(
            "/api/archives/:archive_id/entries/:entry_uid/rearchive",
            post(rearchive_handler),
        )
        .route(
            "/api/archives/:archive_id/entries/:entry_uid/thread-title",
            post(generate_thread_title_handler),
        )
        .route(
            "/api/archives/:archive_id/entries/:entry_uid/summary",
            get(entry_summary_handler).post(request_entry_summary_handler),
        )
        .route(
            "/api/summary/transcription-engines",
            get(transcription_engines_handler),
        )
        .route(
            "/api/archives/:archive_id/entries/:entry_uid/favicon",
            get(serve_entry_favicon),
        )
        .route("/api/archives/:archive_id/blobs/:sha256", get(serve_blob))
        .route("/api/archives/:archive_id/runs", get(list_runs))
        .route("/api/captures/options", get(capture_options_handler))
        .route("/api/archives/:archive_id/captures", post(capture_handler))
        .route(
            "/api/archives/:archive_id/captures/text/title",
            post(generate_text_title_handler)
                .layer(DefaultBodyLimit::max(MAX_TEXT_CAPTURE_REQUEST_BYTES)),
        )
        .route(
            "/api/archives/:archive_id/captures/text",
            post(capture_text_handler)
                .layer(DefaultBodyLimit::max(MAX_TEXT_CAPTURE_REQUEST_BYTES)),
        )
        .route(
            "/api/archives/:archive_id/uploads",
            post(upload_handler)
                .delete(delete_upload_handler)
                .layer(DefaultBodyLimit::max(10 * 1024 * 1024 * 1024)),
        )
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
        .route("/api/admin/yt-dlp", get(get_yt_dlp_status_handler))
        .route("/api/admin/yt-dlp/update", post(update_yt_dlp_handler))
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
        // Workstream routers (admin_users, credentials, jobs, effective_config).
        // Axum 0.7 merges different methods on an existing path but panics on a
        // duplicate path+method, so streams extending an existing path edit the
        // existing handler in place (see the contract spec).
        .merge(crate::admin_users::routes())
        .merge(crate::credentials::routes())
        .merge(crate::jobs::routes())
        .merge(crate::effective_config::routes())
        .fallback_service(ServeDir::new(&static_dir).not_found_service(ServeFile::new(static_dir.join("index.html"))))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::token_scope::enforce_read_scope,
        ))
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
        media_tokens: Arc::new(Mutex::new(HashMap::new())),
    };
    app_with_state(state)
}

fn static_dir() -> PathBuf {
    std::env::var_os("ARCHIVR_STATIC_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("static"))
}

/// `GET /api/archives` item. `archive_path` (a server filesystem path) is only
/// included for ADMIN callers.
#[derive(serde::Serialize)]
struct ArchiveListItem {
    id: String,
    label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    archive_path: Option<PathBuf>,
}

async fn list_archives(State(state): State<AppState>, auth: AuthUser) -> Json<Vec<ArchiveListItem>> {
    let is_admin = auth.has_role(ROLE_ADMIN);
    Json(
        state
            .registry
            .archives
            .iter()
            .map(|a| ArchiveListItem {
                id: a.id.clone(),
                label: a.label.clone(),
                archive_path: is_admin.then(|| a.archive_path.clone()),
            })
            .collect(),
    )
}

#[derive(Debug, serde::Deserialize, Default)]
struct EntriesFilter {
    collection: Option<String>,
}

async fn list_entries(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(archive_id): Path<String>,
    Query(filter): Query<EntriesFilter>,
) -> Result<Json<Vec<archive::EntrySummary>>, ApiError> {
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    // "main" is a URL-friendly alias for the default collection; no param also resolves to it.
    let coll = match filter.collection.as_deref() {
        None | Some("main") => database::get_collection_by_slug(&conn, "_default_")?
            .ok_or(ApiError::not_found("default collection missing"))?,
        Some(uid) => database::get_collection_by_uid(&conn, uid)?
            .ok_or(ApiError::not_found("collection not found"))?,
    };
    if coll.requires_auth {
        auth.require_auth()?;
    }
    let caller_bits = auth_to_caller_bits(&auth);
    Ok(Json(archive::list_entries_for_collection(&conn, coll.id, caller_bits)?))
}

async fn list_entry_children(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((archive_id, entry_uid)): Path<(String, String)>,
) -> Result<Json<Vec<archive::EntrySummary>>, ApiError> {
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    if matches!(auth, AuthUser::Guest) {
        // list_child_entries checks visibility_bits but not collections.requires_auth;
        // gate on the parent being publicly accessible before opening the endpoint.
        if !database::is_entry_publicly_accessible(&conn, &entry_uid)? {
            return Err(ApiError::unauthorized("login required"));
        }
    } else {
        auth.require_auth()?;
    }
    let caller_bits = auth_to_caller_bits(&auth);
    Ok(Json(archive::list_child_entries(&conn, &entry_uid, caller_bits)?))
}

async fn search_entries_handler(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(archive_id): Path<String>,
    Query(params): Query<EntrySearchParams>,
) -> Result<Json<Vec<archive::EntrySummary>>, ApiError> {
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    let coll = match params.collection.as_deref() {
        None | Some("main") => database::get_collection_by_slug(&conn, "_default_")?
            .ok_or(ApiError::not_found("default collection missing"))?,
        Some(uid) => database::get_collection_by_uid(&conn, uid)?
            .ok_or(ApiError::not_found("collection not found"))?,
    };
    if coll.requires_auth {
        auth.require_auth()?;
    }
    let raw = params.q.as_deref().unwrap_or("");
    let mut search_query = archive::parse_search_query(raw)
        .map_err(|prefix| ApiError::bad_request(&format!("unknown search prefix: {prefix}")))?;
    if let Some(tag) = params.tag {
        search_query.tag = Some(tag);
    }
    search_query.caller_bits = auth_to_caller_bits(&auth);
    search_query.collection_id = Some(coll.id);
    Ok(Json(archive::search_entries(&conn, &search_query)?))
}

async fn entry_detail(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path((archive_id, entry_uid)): Path<(String, String)>,
) -> Result<Json<archive::EntryDetail>, ApiError> {
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    if matches!(auth_user, AuthUser::Guest) {
        if !database::is_entry_publicly_accessible(&conn, &entry_uid)? {
            return Err(ApiError::unauthorized("login required"));
        }
    }
    ensure_entry_visible(&conn, &auth_user, &entry_uid)?;
    let mut detail = archive::get_entry_detail(&conn, &entry_uid)?
        .ok_or(ApiError::not_found("entry not found"))?;
    if matches!(auth_user, AuthUser::Guest) {
        let entry_id = database::entry_id_for_uid(&conn, &entry_uid)?
            .ok_or(ApiError::not_found("entry not found"))?;
        detail.latest_summary = database::latest_completed_entry_summary(&conn, entry_id)?;
        detail.summary_attempt = None;
    }
    Ok(Json(detail))
}

#[derive(Debug, serde::Deserialize)]
struct SummaryRequestBody {
    provider: String,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    include_images: bool,
    /// Local transcription engine to use only if a YouTube video has no
    /// subtitles; empty or absent means none.
    #[serde(default)]
    transcribe_engine: Option<String>,
}

/// `GET /api/archives/:id/entries/:uid/summary`
///
/// Read-only, gated exactly like entry detail: a guest may read a summary only
/// for an entry whose content they could already read. Returns
/// `{ entry_uid, summary }` with a null summary when none has been requested,
/// which is also what the frontend polls while a job is running.
async fn entry_summary_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path((archive_id, entry_uid)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    if matches!(auth_user, AuthUser::Guest)
        && !database::is_entry_publicly_accessible(&conn, &entry_uid)?
    {
        return Err(ApiError::unauthorized("login required"));
    }
    ensure_entry_visible(&conn, &auth_user, &entry_uid)?;
    let entry_id = database::entry_id_for_uid(&conn, &entry_uid)?
        .ok_or(ApiError::not_found("entry not found"))?;
    let (summary, attempt) = if matches!(auth_user, AuthUser::Guest) {
        (database::latest_completed_entry_summary(&conn, entry_id)?, None)
    } else {
        (
            database::latest_completed_entry_summary(&conn, entry_id)?,
            database::latest_entry_summary_attempt(&conn, entry_id)?,
        )
    };
    let mut body = serde_json::json!({ "entry_uid": entry_uid, "summary": summary });
    if !matches!(auth_user, AuthUser::Guest) {
        body["attempt"] = serde_json::to_value(attempt)?;
    }
    Ok(Json(body))
}

/// `POST /api/archives/:id/entries/:uid/summary`
///
/// Manual-only summary generation. Body: `{ "provider": "...", "force": false,
/// "include_images": false, "transcribe_engine": "whisper" }`; the optional
/// `transcribe_engine` is used only for YouTube videos that end up without
/// subtitles, and the transcription runs in the background.
///
/// Provider and transcription-engine configuration and content extraction are
/// all resolved *before* spawning, so a missing env var or an unsummarizable
/// artifact comes back as a synchronous 400 naming the exact problem rather
/// than as a background job the caller has to poll only to learn about a
/// config typo.
///
/// Returns 200 with the existing row when an identical cache key already
/// completed and `force` is false; otherwise 202 with a pending row.
///
/// YouTube videos without archived subtitles also return 202: the pending row
/// carries a placeholder input digest while subtitles are fetched in the
/// background, and a failed fetch fails the row with a safe explanation.
async fn request_entry_summary_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path((archive_id, entry_uid)): Path<(String, String)>,
    Json(body): Json<SummaryRequestBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    auth_user.require_role(ROLE_USER)?;
    let mounted = mounted_archive(&state, &archive_id)?;
    // A hidden entry is not found: it must not be summarized, nor probe provider config.
    ensure_entry_visible(
        &database::open_or_initialize(&mounted.archive_path)?,
        &auth_user,
        &entry_uid,
    )?;
    let archive_paths =
        archive::read_archive_paths(&mounted.archive_path).map_err(ApiError::from)?;

    // 1. Provider config from the environment. The error text carries the exact
    //    variable name, which is the whole point of returning it as a 400.
    let provider_cfg = summarizer::provider_from_env(&body.provider)
        .map_err(|e| ApiError::bad_request(&format!("{e:#}")))?;
    if body.include_images && matches!(provider_cfg, summarizer::ProviderConfig::ClaudeCli(_)) {
        return Err(ApiError::bad_request(
            "Claude CLI cannot attach local images; choose an HTTP provider or Codex CLI",
        ));
    }
    let transcription = match body
        .transcribe_engine
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
    {
        Some(kind) => Some(
            archivr_core::transcriber::request_from_env(kind)
                .map_err(|e| ApiError::bad_request(&format!("{e:#}")))?,
        ),
        None => None,
    };
    let provider = summarizer::provider_from_config(provider_cfg);
    let summary_options = summarizer::SummaryBuildOptions {
        include_images: body.include_images,
    };

    // 2. Preflight extraction and SQLite cache/attempt work are synchronous
    // core operations, so keep them off the Axum runtime. This also means the
    // input claimed here is passed directly to the provider worker below.
    let preflight_paths = archive_paths.clone();
    let preflight_uid = entry_uid.clone();
    let provider_kind = provider.kind().to_string();
    let provider_model = provider.model().map(str::to_string);
    let force = body.force;
    enum PreflightOutcome {
        Cached(database::EntrySummaryRecord),
        Pending { input: summarizer::SummaryInput, summary_uid: String },
        FetchSubtitles { summary_uid: String },
    }
    let outcome = tokio::task::spawn_blocking(move || -> anyhow::Result<PreflightOutcome> {
        let conn = database::open_or_initialize(&preflight_paths.archive_path)?;
        let entry_id = database::entry_id_for_uid(&conn, &preflight_uid)?
            .ok_or_else(|| anyhow::anyhow!("entry not found"))?;
        // Preserve the route's historical 404 before attempting content
        // extraction, whose own missing-entry error includes the uid.
        let input = match summarizer::build_summary_input(&preflight_paths, &preflight_uid, summary_options) {
            Ok(input) => input,
            Err(e) if summarizer::is_no_subtitles_error(&e) => {
                // The real digest is unknown until subtitles are fetched, so
                // there is no cache key to look up yet.
                let summary_uid = database::upsert_pending_entry_summary(
                    &conn, entry_id, &provider_kind, provider_model.as_deref(),
                    summarizer::PROMPT_VERSION, summarizer::SUBTITLE_FETCH_PENDING_INPUT_SHA256,
                )?;
                return Ok(PreflightOutcome::FetchSubtitles { summary_uid });
            }
            Err(e) => return Err(e),
        };
        if !force {
            if let Some(existing) = database::find_entry_summary(
                &conn, entry_id, &provider_kind, provider_model.as_deref(),
                summarizer::PROMPT_VERSION, &input.input_sha256,
            )? {
                if existing.status == "completed" {
                    return Ok(PreflightOutcome::Cached(existing));
                }
            }
        }
        let summary_uid = database::upsert_pending_entry_summary(
            &conn, entry_id, &provider_kind, provider_model.as_deref(),
            summarizer::PROMPT_VERSION, &input.input_sha256,
        )?;
        Ok(PreflightOutcome::Pending { input, summary_uid })
    })
    .await
    .map_err(|e| ApiError::internal(&format!("summary preflight task failed: {e}")))?
    .map_err(|e| {
        if summarizer::is_unsupported_summary_content_error(&e) {
            ApiError::bad_request(summarizer::UNSUPPORTED_SUMMARY_CONTENT_MESSAGE)
        } else if format!("{e:#}") == "entry not found" {
            ApiError::not_found("entry not found")
        } else {
            ApiError::bad_request(&format!("{e:#}"))
        }
    })?;
    let (input, summary_uid) = match outcome {
        PreflightOutcome::Cached(existing) => return Ok((
            StatusCode::OK,
            serde_json::to_value(&existing).map(Json)
                .map_err(|e| ApiError::internal(&e.to_string()))?,
        )),
        PreflightOutcome::Pending { input, summary_uid } => (Some(input), summary_uid),
        PreflightOutcome::FetchSubtitles { summary_uid } => (None, summary_uid),
    };

    let archive_path = mounted.archive_path.clone();
    let auth_db_path = state.auth_db_path.clone();
    let entry_uid_bg = entry_uid.clone();
    let summary_uid_bg = summary_uid.clone();
    tokio::task::spawn_blocking(move || {
        // A prebuilt input was claimed during blocking preflight, so no row is
        // reset and no archive content is read twice. Without one, subtitles
        // are fetched first and the row gets its real input digest.
        let run = || -> anyhow::Result<()> {
            let input = match input {
                Some(input) => input,
                None => {
                    let cookie_rules = match database::open_auth_db(&auth_db_path) {
                        Ok(conn) => database::list_cookie_rules(&conn).unwrap_or_default(),
                        Err(_) => vec![],
                    };
                    let input = summarizer::build_summary_input_with_subtitle_fetch(
                        &archive_paths,
                        &entry_uid_bg,
                        summary_options,
                        &cookie_rules,
                        transcription.as_ref(),
                    )?;
                    let conn = database::open_or_initialize(&archive_path)?;
                    database::update_entry_summary_input_sha256(
                        &conn,
                        &summary_uid_bg,
                        &input.input_sha256,
                    )?;
                    input
                }
            };
            summarizer::summarize_prebuilt_entry(
                &archive_paths,
                input,
                &summary_uid_bg,
                provider.as_ref(),
            )
            .map(|_| ())
        };
        if let Err(e) = run() {
            eprintln!("warn: summary {summary_uid_bg}: {e:#}");
            record_background_summary_failure(&archive_path, &summary_uid_bg, &e);
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "summary_uid": summary_uid,
            "status": "pending",
            "entry_uid": entry_uid,
        })),
    ))
}

fn summary_failure_error_text(error: &anyhow::Error) -> String {
    if let Some(copy) = archivr_core::transcriber::transcription_user_message(error) {
        copy
    } else if summarizer::is_no_subtitles_error(error) {
        summarizer::NO_SUBTITLES_SUMMARY_MESSAGE.to_string()
    } else if summarizer::is_unsupported_summary_content_error(error) {
        summarizer::UNSUPPORTED_SUMMARY_CONTENT_MESSAGE.to_string()
    } else {
        format!("{error:#}")
    }
}

/// `GET /api/summary/transcription-engines`
///
/// Local transcription engines that are enabled (`ARCHIVR_TRANSCRIBE_ENGINES`)
/// and fully configured. Reads env only; an empty array means the feature is off.
async fn transcription_engines_handler(
    auth_user: AuthUser,
) -> Result<Json<Vec<archivr_core::transcriber::TranscriberInfo>>, ApiError> {
    auth_user.require_role(ROLE_USER)?;
    Ok(Json(archivr_core::transcriber::available_transcribers()))
}

fn record_background_summary_failure(
    archive_path: &std::path::Path,
    summary_uid: &str,
    error: &anyhow::Error,
) {
    if let Ok(conn) = database::open_or_initialize(archive_path) {
        if let Ok(Some(row)) = database::get_entry_summary_by_uid(&conn, summary_uid) {
            if row.status != "failed" {
                database::update_entry_summary_status(
                    &conn,
                    summary_uid,
                    "failed",
                    None,
                    Some(&summary_failure_error_text(error)),
                )
                .ok();
            }
        }
    }
}

async fn list_runs(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(archive_id): Path<String>,
    Query(page): Query<RunsPageQuery>,
) -> Result<Json<Vec<archive::RunSummary>>, ApiError> {
    let (_, role_bits) = auth_user.require_auth()?;
    let limit = jobs::parse_int_param("limit", page.limit.as_deref())?.map(|n| n.max(0));
    let offset = jobs::parse_int_param("offset", page.offset.as_deref())?
        .unwrap_or(0)
        .max(0);
    let mounted = mounted_archive(&state, &archive_id)?;
    let caller_uid = jobs::caller_user_uid(&state, &auth_user)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    // Visibility follows access: admins see all runs; others see runs whose capture job
    // they created or that produced at least one entry they can see.
    Ok(Json(archive::list_runs_for_caller(
        &conn,
        role_bits,
        caller_uid.as_deref(),
        limit,
        offset,
    )?))
}

#[derive(Debug, serde::Deserialize, Default)]
struct RunsPageQuery {
    limit: Option<String>,
    offset: Option<String>,
}
const MEDIA_TOKEN_TTL: Duration = Duration::from_secs(2 * 60 * 60); // 2 h

#[derive(Debug, serde::Deserialize, Default)]
struct ArtifactQuery {
    token: Option<String>,
}

#[derive(serde::Serialize)]
struct MediaTokenResponse {
    url: String,
    expires_in_secs: u64,
}

async fn serve_artifact(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path((archive_id, entry_uid, artifact_index)): Path<(String, String, usize)>,
    Query(params): Query<ArtifactQuery>,
    req: Request,
) -> Result<Response, ApiError> {
    // Auth: valid scoped token OR authenticated session OR publicly accessible entry.
    // A token present but invalid/expired falls back to session/public check so that
    // a logged-in browser player keeps working after a token expires.
    let token_valid = params.token.as_deref().map_or(false, |tok| {
        let tokens = state.media_tokens.lock();
        tokens.get(tok).map_or(false, |t| {
            t.archive_id == archive_id
                && t.entry_uid == entry_uid
                && t.artifact_index == artifact_index
                && t.expires_at > std::time::Instant::now()
        })
    });
    if !token_valid {
        if matches!(auth_user, AuthUser::Guest) {
            let mounted_check = mounted_archive(&state, &archive_id)?;
            let conn_check = database::open_or_initialize(&mounted_check.archive_path)?;
            if !database::is_entry_publicly_accessible(&conn_check, &entry_uid)? {
                return Err(ApiError::unauthorized("login required"));
            }
        } else {
            auth_user.require_auth()?;
        }
    }
    let mounted = mounted_archive(&state, &archive_id)?;
    let paths = archive::read_archive_paths(&mounted.archive_path)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    if !token_valid {
        // A media token was only issued after this check passed (issue_media_token).
        ensure_entry_visible(&conn, &auth_user, &entry_uid)?;
    }
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

/// POST /api/archives/:archive_id/entries/:entry_uid/artifacts/:artifact_index/media-token
///
/// Requires an authenticated session. Returns a short-lived signed URL that
/// allows unauthenticated GET of the specified artifact — intended for Cast /
/// AirPlay devices that cannot carry the browser's session cookie.
async fn issue_media_token(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path((archive_id, entry_uid, artifact_index)): Path<(String, String, usize)>,
) -> Result<Json<MediaTokenResponse>, ApiError> {
    auth_user.require_auth()?;
    // Verify the artifact actually exists before issuing a token.
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    ensure_entry_visible(&conn, &auth_user, &entry_uid)?;
    let detail = archive::get_entry_detail(&conn, &entry_uid)?
        .ok_or(ApiError::not_found("entry not found"))?;
    if artifact_index >= detail.artifacts.len() {
        return Err(ApiError::not_found("artifact index out of range"));
    }
    let token = auth::generate_token();
    let now = std::time::Instant::now();
    {
        let mut tokens = state.media_tokens.lock();
        // GC expired tokens on each issuance to keep the map bounded.
        tokens.retain(|_, t| t.expires_at > now);
        tokens.insert(
            token.clone(),
            MediaToken {
                archive_id: archive_id.clone(),
                entry_uid: entry_uid.clone(),
                artifact_index,
                expires_at: now + MEDIA_TOKEN_TTL,
            },
        );
    }
    let url = format!(
        "/api/archives/{}/entries/{}/artifacts/{}?token={}",
        archive_id, entry_uid, artifact_index, token
    );
    Ok(Json(MediaTokenResponse {
        url,
        expires_in_secs: MEDIA_TOKEN_TTL.as_secs(),
    }))
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
    ensure_entry_visible(&conn, &auth_user, &entry_uid)?;
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
    // Blobs are content-addressed and shared between entries: readable when the caller can
    // see at least one entry that uses the blob (ADMIN/OWNER: any existing blob).
    if !database::caller_can_access_blob(&conn, &sha256, auth_to_caller_bits(&auth_user))? {
        return Err(ApiError::not_found("blob not found"));
    }
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
    #[serde(default = "default_requires_auth")]
    requires_auth: bool,
}

fn default_user_visibility() -> u32 {
    2
}

fn default_requires_auth() -> bool {
    true
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
    requires_auth: Option<bool>,
}

async fn list_tags(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(archive_id): Path<String>,
) -> Result<Json<Vec<archive::TagNode>>, ApiError> {
    auth_user.require_auth()?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    Ok(Json(archive::list_tag_tree(&conn, auth_to_caller_bits(&auth_user))?))
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
    ensure_entry_visible(&conn, &auth_user, &entry_uid)?;
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
    ensure_entry_visible(&conn, &auth_user, &entry_uid)?;
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
    ensure_entry_visible(&conn, &auth_user, &entry_uid)?;
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
    ensure_entry_visible(&conn, &auth_user, &entry_uid)?;
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
    ensure_entry_visible(&conn, &auth_user, &entry_uid)?;
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

/// Inspect the already-resolved CLI path without invoking a provider. Bare
/// names use PATH, while explicit paths must themselves be executable files.
pub(crate) fn cli_executable_available(
    executable: &std::path::Path,
    search_path: Option<&std::ffi::OsStr>,
) -> bool {
    let is_executable = |path: &std::path::Path| {
        let Ok(metadata) = std::fs::metadata(path) else {
            return false;
        };
        if !metadata.is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            true
        }
    };
    if executable.is_absolute() || executable.components().count() > 1 {
        return is_executable(executable);
    }
    search_path.is_some_and(|path| {
        std::env::split_paths(path).any(|directory| is_executable(&directory.join(executable)))
    })
}

/// Safe capture defaults for all users who can submit a capture.
async fn capture_options_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
) -> Result<Json<serde_json::Value>, ApiError> {
    auth_user.require_role(ROLE_USER)?;
    let conn = database::open_auth_db(&state.auth_db_path)?;
    let settings = database::get_instance_settings(&conn)?;
    let extension_available = |var: &str| {
        std::env::var(var)
            .ok()
            .filter(|value| !value.is_empty())
            .is_some_and(|path| std::path::Path::new(&path).is_dir())
    };
    let providers: Vec<_> = [
        ("anthropic_http", "Anthropic"),
        ("openai_compatible", "OpenAI-compatible"),
        ("claude_cli", "Claude CLI"),
        ("codex_cli", "Codex CLI"),
    ]
    .into_iter()
    .filter(|(kind, _)| {
        summarizer::provider_from_env(kind).is_ok_and(|config| match config {
            summarizer::ProviderConfig::AnthropicHttp(_)
            | summarizer::ProviderConfig::OpenAiCompatible(_) => true,
            summarizer::ProviderConfig::ClaudeCli(config)
            | summarizer::ProviderConfig::CodexCli(config) => {
                cli_executable_available(&config.executable, std::env::var_os("PATH").as_deref())
            }
        })
    })
    .map(|(kind, label)| serde_json::json!({"kind": kind, "label": label}))
    .collect();
    Ok(Json(serde_json::json!({
        "ublock_enabled": settings.ublock_enabled,
        "cookie_ext_enabled": settings.cookie_ext_enabled,
        "modal_closer_enabled": settings.modal_closer_enabled,
        "ublock_ext_available": extension_available("ARCHIVR_UBLOCK_EXT"),
        "cookie_ext_available": extension_available("ARCHIVR_COOKIE_EXT"),
        "reader_mode": false,
        "via_freedium": true,
        "download_subtitles": true,
        "title_providers": providers,
    })))
}

#[derive(Debug, serde::Deserialize)]
struct TextTitleBody {
    body: String,
    provider: String,
}

async fn generate_text_title_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(archive_id): Path<String>,
    Json(body): Json<TextTitleBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    auth_user.require_role(ROLE_USER)?;
    if body.body.trim().is_empty() {
        return Err(ApiError::bad_request("body must not be empty"));
    }
    if body.body.len() > MAX_TEXT_CAPTURE_BODY_BYTES {
        return Err(ApiError::bad_request("body must not exceed 2 MiB"));
    }
    mounted_archive(&state, &archive_id)?;
    let settings = database::get_instance_settings(&database::open_auth_db(&state.auth_db_path)?)?;
    let cfg = thread_title::title_provider_from_env(
        &body.provider,
        settings.title_model_override(&body.provider),
    )
    .map_err(|error| ApiError::bad_request(&format!("{error:#}")))?;
    let title = tokio::task::spawn_blocking(move || {
        text_title::generate_text_title(&cfg, &body.body).map_err(|error| {
            eprintln!("warn: text title: {error:#}");
            ApiError {
                status: StatusCode::BAD_GATEWAY,
                message: format!("{error:#}"),
            }
        })
    })
    .await
    .map_err(|error| ApiError::internal(&format!("text title task failed: {error}")))??;
    Ok(Json(serde_json::json!({"title": title})))
}

#[derive(Debug, serde::Deserialize)]
struct ThreadTitleBody {
    provider: String,
}

/// `POST /api/archives/:id/entries/:uid/thread-title` — names an X thread with a
/// cheap model (`thread_title`) and saves it as the entry title. Same role gate as
/// title PATCH. Synchronous: load, provider call and save run in one blocking task.
async fn generate_thread_title_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path((archive_id, entry_uid)): Path<(String, String)>,
    Json(body): Json<ThreadTitleBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    auth_user.require_role(ROLE_USER)?;
    let mounted = mounted_archive(&state, &archive_id)?;
    ensure_entry_visible(
        &database::open_or_initialize(&mounted.archive_path)?,
        &auth_user,
        &entry_uid,
    )?;
    let paths = archive::read_archive_paths(&mounted.archive_path).map_err(ApiError::from)?;
    let settings = database::get_instance_settings(&database::open_auth_db(&state.auth_db_path)?)?;
    let cfg = thread_title::title_provider_from_env(
        &body.provider,
        settings.title_model_override(&body.provider),
    )
    .map_err(|e| ApiError::bad_request(&format!("{e:#}")))?;
    let uid = entry_uid.clone();
    let title = tokio::task::spawn_blocking(move || -> Result<String, ApiError> {
        let input = thread_title::load_thread_title_input(&paths, &uid)
            .map_err(|e| thread_title_load_error(&uid, e))?
            .ok_or_else(|| ApiError::not_found("entry not found"))?;
        let title = thread_title::generate_thread_title(&cfg, &input).map_err(|e| {
            eprintln!("warn: thread title {uid}: {e:#}");
            ApiError {
                status: StatusCode::BAD_GATEWAY,
                message: format!("{e:#}"),
            }
        })?;
        let conn = database::open_or_initialize(&paths.archive_path)?;
        if !database::update_entry_title(&conn, &uid, Some(&title))? {
            return Err(ApiError::not_found("entry not found"));
        }
        eprintln!("info: thread title {uid}: {title}");
        Ok(title)
    })
    .await
    .map_err(|e| ApiError::internal(&format!("thread title task failed: {e}")))??;
    Ok(Json(serde_json::json!({ "entry_uid": entry_uid, "title": title })))
}

/// Expected load failures (not a thread, no archived text) are 400 with their
/// message; anything else (DB/IO) is logged and returned as a generic 500 so
/// absolute store paths never reach the client.
fn thread_title_load_error(uid: &str, error: anyhow::Error) -> ApiError {
    match thread_title::thread_title_user_message(&error) {
        Some(message) => ApiError::bad_request(&message),
        None => {
            eprintln!("error: thread title {uid}: {error:#}");
            ApiError::internal("failed to load thread for title generation")
        }
    }
}

async fn reorder_entry_children_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path((archive_id, entry_uid)): Path<(String, String)>,
    Json(body): Json<ReorderChildrenBody>,
) -> Result<StatusCode, ApiError> {
    let (_, role_bits) = auth_user.require_auth()?; // guests → 401
    let settings = database::get_instance_settings(&database::open_auth_db(&state.auth_db_path)?)?;
    if !settings.can_reorder_children(role_bits) {
        return Err(ApiError::forbidden(
            "your role is not allowed to reorder child entries",
        ));
    }
    let mounted = mounted_archive(&state, &archive_id)?;
    let mut conn = database::open_or_initialize(&mounted.archive_path)?;
    // IMMEDIATE: take the write lock before the set check so a concurrent
    // sync capture cannot add a sibling between validation and the writes.
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    // A role granted reorder must still be able to see the parent's children
    // (same rule as the children GET); hidden parents are indistinguishable
    // from missing ones.
    if !database::caller_sees_all_children(&tx, &entry_uid, role_bits)? {
        return Err(ApiError::not_found("entry not found"));
    }
    match database::reorder_child_entries(&tx, &entry_uid, &body.child_uids)? {
        database::ReorderChildrenOutcome::Reordered => {
            tx.commit()?;
            Ok(StatusCode::NO_CONTENT)
        }
        database::ReorderChildrenOutcome::ParentNotFound => {
            Err(ApiError::not_found("entry not found"))
        }
        database::ReorderChildrenOutcome::ChildSetMismatch => Err(ApiError::bad_request(
            "child_uids must list every current child of the entry exactly once",
        )),
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
    /// Download YouTube subtitles. Absent = true.
    download_subtitles: Option<bool>,
    /// Per-video quality overrides for playlist captures.
    /// Keys are yt-dlp video IDs; values are quality strings ("best", "1080p", "audio", etc.).
    #[serde(default)]
    per_item_quality: std::collections::HashMap<String, String>,
    /// When true, skip playlist items already archived under an existing container.
    #[serde(default)]
    sync: bool,
}

#[derive(Debug, serde::Deserialize)]
struct CaptureTextBody {
    title: String,
    body: String,
    /// MIME type: "text/plain" or "text/markdown" (defaults to "text/markdown")
    mime: Option<String>,
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
    /// 1..=3650; absent/null = never expires.
    expires_in_days: Option<i64>,
    /// `full` (default) or `read`.
    scope: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct PatchEntryBody {
    title: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct ReorderChildrenBody {
    /// Every current direct child UID of the parent, in the desired order.
    child_uids: Vec<String>,
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

#[derive(Debug, serde::Deserialize)]
struct DeleteUploadBody {
    locator: String,
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
        if let Some(bad) = body
            .per_item_quality
            .values()
            .find(|q| !is_valid_quality(q))
        {
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

    let locator = body.locator.trim().to_string();
    // A file:// locator is accepted only for a file staged by upload_handler under
    // temp/uploads/ (it is also tracked for cleanup). Anything else would let an API
    // caller capture and read back arbitrary server files, e.g. file:///etc/passwd.
    // Canonicalize both sides to prevent path-traversal via `..` components in the locator.
    // Same pattern as artifact serving. The staged file must already exist on disk
    // (it was written by upload_handler), so canonicalize() will resolve symlinks correctly.
    // A bare path (absolute, or relative to the server's cwd) is classified as a local
    // file by core, so the file:// check alone would let it through; staged uploads
    // are always submitted as file:// locators, so bare paths have no legitimate use here.
    if !locator.starts_with("file://") && capture::locator_is_local_path(&locator) {
        return Err(ApiError::bad_request(
            "local paths are not accepted as locators; upload the file and capture the returned file:// locator",
        ));
    }
    let staged_upload_path: Option<std::path::PathBuf> = if locator.starts_with("file://") {
        let file_path = std::path::PathBuf::from(locator.trim_start_matches("file://"));
        let staging_dir = archive_paths.store_path.join("temp").join("uploads");
        match (file_path.canonicalize(), staging_dir.canonicalize()) {
            (Ok(canonical_file), Ok(canonical_staging))
                if canonical_file.starts_with(&canonical_staging) =>
            {
                Some(canonical_file)
            }
            _ => {
                return Err(ApiError::bad_request(
                    "file:// locators must reference a staged upload",
                ));
            }
        }
    } else {
        None
    };

    // Create job record in the archive DB.
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    let created_by = jobs::caller_user_uid(&state, &auth_user)?;
    let job_uid = database::create_capture_job_as(&conn, &archive_id, created_by.as_deref())?;
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
        download_subtitles: body.download_subtitles.unwrap_or(true),
        per_item_quality: body.per_item_quality.clone(),
        sync: body.sync,
        job_uid: Some(job_uid.clone()),
    };

    // Spawn background capture.
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
                // Clean up staged upload file — content is now in the raw store.
                // `staged` is already the canonicalized path (safe to remove_file directly).
                // Also attempt to remove the now-empty UUID parent dir; fails silently if
                // non-empty or already gone.
                if let Some(staged) = staged_upload_path {
                    let _ = std::fs::remove_file(&staged);
                    if let Some(parent) = staged.parent() {
                        let _ = std::fs::remove_dir(parent);
                    }
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
                // Failed captures never move the file into the raw store,
                // so clean up the staged upload here rather than waiting
                // for the next startup or periodic prune.
                if let Some(staged) = staged_upload_path {
                    let _ = std::fs::remove_file(&staged);
                    if let Some(parent) = staged.parent() {
                        let _ = std::fs::remove_dir(parent);
                    }
                }
            }
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "job_uid": job_uid, "status": "pending" })),
    ))
}

async fn capture_text_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(archive_id): Path<String>,
    Json(body): Json<CaptureTextBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    auth_user.require_role(ROLE_USER)?;

    // Validate title and body
    if body.title.trim().is_empty() {
        return Err(ApiError::bad_request("title must not be empty"));
    }
    if body.body.trim().is_empty() {
        return Err(ApiError::bad_request("body must not be empty"));
    }
    if body.body.len() > MAX_TEXT_CAPTURE_BODY_BYTES {
        return Err(ApiError::bad_request("body must not exceed 2 MiB"));
    }

    // Determine MIME type (default to markdown)
    let mime = body.mime.as_deref().unwrap_or("text/markdown");

    // Validate MIME type
    if mime != "text/plain" && mime != "text/markdown" {
        return Err(ApiError::bad_request(
            "unsupported MIME type: must be 'text/plain' or 'text/markdown'"
        ));
    }

    let mounted = mounted_archive(&state, &archive_id)?;
    let archive_paths =
        archive::read_archive_paths(&mounted.archive_path).map_err(ApiError::from)?;

    // Create job record in the archive DB.
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    let created_by = jobs::caller_user_uid(&state, &auth_user)?;
    let job_uid = database::create_capture_job_as(&conn, &archive_id, created_by.as_deref())?;
    drop(conn);

    // Spawn background text capture.
    let title = body.title.trim().to_string();
    let text_body = body.body;
    let mime_str = mime.to_string();
    let job_uid_bg = job_uid.clone();
    let archive_path = mounted.archive_path.clone();
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

        match capture::perform_text_capture_for_job(
            &archive_paths,
            &title,
            &text_body,
            &mime_str,
            Some(&archive_id_bg),
            Some(&job_uid_bg),
        ) {
            Ok(result) => {
                let job_status = if result.status == "completed" {
                    "completed"
                } else {
                    "failed"
                };
                database::update_capture_job_status(
                    &conn,
                    &job_uid_bg,
                    job_status,
                    Some(&result.run_uid),
                    None,
                    None,
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

async fn upload_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(archive_id): Path<String>,
    mut multipart: Multipart,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    auth_user.require_role(ROLE_USER)?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let archive_paths =
        archive::read_archive_paths(&mounted.archive_path).map_err(ApiError::from)?;

    // Stage under temp/uploads/<uuid>/<safe_name> so Source::Local derives the
    // entry title from Path::file_name() of the locator rather than the uuid.
    let staging_base = archive_paths.store_path.join("temp").join("uploads");

    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::bad_request(&e.to_string()))?
    {
        if field.name() != Some("file") {
            continue;
        }
        let filename = field.file_name().unwrap_or("upload").to_string();

        // Sanitize: strip path separators and control chars, truncate to 200 chars.
        let safe_name: String = filename
            .chars()
            .filter(|c| !matches!(*c, '/' | '\\' | '\0'))
            .collect::<String>()
            .trim()
            .to_string();
        let safe_name = if safe_name.is_empty() {
            "upload".to_string()
        } else {
            safe_name.chars().take(200).collect()
        };

        let uuid_dir = staging_base.join(uuid::Uuid::new_v4().simple().to_string());
        tokio::fs::create_dir_all(&uuid_dir).await.map_err(|e| {
            ApiError::from(anyhow::anyhow!("failed to create upload staging dir: {e}"))
        })?;

        // Sentinel: exists while the XHR is streaming. The prune task skips any
        // uuid_dir that contains this file, so a slow upload is never deleted
        // mid-transfer regardless of wall-clock age.
        let sentinel = uuid_dir.join(".uploading");
        tokio::fs::File::create(&sentinel).await.map_err(|e| {
            ApiError::from(anyhow::anyhow!("failed to create upload sentinel: {e}"))
        })?;

        let staged_path = uuid_dir.join(&safe_name);

        // Stream chunks directly to disk — never buffers the full file in RAM.
        // The body limit (10 GiB) is enforced by the DefaultBodyLimit layer.
        let stream_result: Result<(), ApiError> = async {
            use tokio::io::AsyncWriteExt as _;
            let file = tokio::fs::File::create(&staged_path).await.map_err(|e| {
                ApiError::from(anyhow::anyhow!("failed to create staged file: {e}"))
            })?;
            let mut writer = tokio::io::BufWriter::new(file);
            while let Some(chunk) = field
                .chunk()
                .await
                .map_err(|e| ApiError::bad_request(&e.to_string()))?
            {
                writer
                    .write_all(&chunk)
                    .await
                    .map_err(|e| ApiError::from(anyhow::anyhow!("failed to write chunk: {e}")))?;
            }
            writer
                .flush()
                .await
                .map_err(|e| ApiError::from(anyhow::anyhow!("failed to flush upload: {e}")))?;
            Ok(())
        }
        .await;
        if let Err(e) = stream_result {
            // remove_dir_all cleans up the partial file and the sentinel together.
            let _ = tokio::fs::remove_dir_all(&uuid_dir).await;
            return Err(e);
        }

        // Stream complete — drop the sentinel so the prune task can reclaim the
        // dir if it is later abandoned without being submitted for capture.
        let _ = tokio::fs::remove_file(&sentinel).await;

        let size = tokio::fs::metadata(&staged_path)
            .await
            .map(|m| m.len() as i64)
            .unwrap_or(0);

        let locator = format!("file://{}", staged_path.display());
        return Ok((
            StatusCode::OK,
            Json(serde_json::json!({
                "locator": locator,
                "filename": filename,
                "size": size,
            })),
        ));
    }

    Err(ApiError::bad_request(
        "no file field found in multipart upload",
    ))
}

/// `DELETE /api/archives/:archive_id/uploads`
///
/// Discards a staged upload file that was never submitted for capture —
/// called by the frontend when the user removes a file row or cancels the
/// dialog.  The same canonicalize-then-prefix-check used in `capture_handler`
/// prevents path traversal via crafted `file://` locators.
async fn delete_upload_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(archive_id): Path<String>,
    Json(body): Json<DeleteUploadBody>,
) -> Result<StatusCode, ApiError> {
    auth_user.require_role(ROLE_USER)?;
    let mounted = mounted_archive(&state, &archive_id)?;
    let archive_paths =
        archive::read_archive_paths(&mounted.archive_path).map_err(ApiError::from)?;

    if !body.locator.starts_with("file://") {
        return Err(ApiError::bad_request("locator must be a file:// URI"));
    }
    let file_path = std::path::PathBuf::from(body.locator.trim_start_matches("file://"));
    let staging_dir = archive_paths.store_path.join("temp").join("uploads");

    // Canonicalize both sides before the prefix check (path-traversal guard).
    let (canonical_file, canonical_staging) =
        match (file_path.canonicalize(), staging_dir.canonicalize()) {
            (Ok(f), Ok(s)) => (f, s),
            _ => return Err(ApiError::not_found("staged upload not found")),
        };
    if !canonical_file.starts_with(&canonical_staging) {
        return Err(ApiError::bad_request("locator is not a staged upload"));
    }

    tokio::fs::remove_file(&canonical_file).await.ok();
    if let Some(parent) = canonical_file.parent() {
        tokio::fs::remove_dir(parent).await.ok(); // no-op if non-empty
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn get_capture_job_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path((archive_id, job_uid)): Path<(String, String)>,
) -> Result<Json<jobs::CaptureJobDetail>, ApiError> {
    auth_user.require_role(ROLE_USER)?;
    let mounted = mounted_archive(&state, &archive_id)?;
    jobs::get_job_detail(&state, &auth_user, &mounted.archive_path, &job_uid).map(Json)
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
    ensure_entry_visible(&conn, &auth_user, &entry_uid)?;
    // Unknown (and, above, hidden) entries are a 404 for every caller; no doomed job row.
    if database::entry_id_for_uid(&conn, &entry_uid)?.is_none() {
        return Err(ApiError::not_found("entry not found"));
    }
    let created_by = jobs::caller_user_uid(&state, &auth_user)?;
    let job_uid = database::create_capture_job_as(&conn, &archive_id, created_by.as_deref())?;
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
        download_subtitles: true,
        per_item_quality: std::collections::HashMap::new(),
        sync: false,
        // Rearchive replaces an entry's artifacts in place and creates no run to link.
        job_uid: None,
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
    let canonical_url = capture::locator_to_playlist_url(&locator).ok_or_else(|| {
        ApiError::bad_request(
            "locator is not a YouTube playlist, channel, YTM playlist, or Spotify album/playlist",
        )
    })?;
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
    let can_reorder_children =
        database::get_instance_settings(&conn)?.can_reorder_children(role_bits);
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
            "can_reorder_children": can_reorder_children,
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
    let settings = database::get_instance_settings(&conn)?;
    let user_uid = database::get_user_uid(&conn, user_id)?;
    let roles = archivr_core::auth_credentials::list_user_role_slugs(&conn, user_id)?;
    Ok(Json(serde_json::json!({
        "user_uid": user_uid,
        "roles": roles,
        "role_bits": role_bits,
        "username": username,
        "display_name": display_name,
        "humanize_slugs": humanize_slugs,
        "can_reorder_children": settings.can_reorder_children(role_bits),
    })))
}

async fn patch_me(
    State(state): State<AppState>,
    auth_user: AuthUser,
    jar: CookieJar,
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
        if new_pw.chars().count() < 8 {
            return Err(ApiError::bad_request(
                "new_password must be at least 8 characters",
            ));
        }
        let new_hash = auth::hash_password(new_pw).map_err(ApiError::from)?;
        database::update_user_password(&conn, user_id, &new_hash)?;
        // Every other session is stale now; the caller's own cookie session survives.
        archivr_core::auth_credentials::delete_other_sessions(
            &conn,
            user_id,
            jar.get("session").map(|c| c.value()),
        )?;
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
        let title_models: serde_json::Map<String, serde_json::Value> = summarizer::PROVIDER_KINDS
            .iter()
            .filter_map(|kind| {
                let instance = settings.title_model_override(kind);
                let (model, source) = thread_title::resolve_title_model(kind, instance)?;
                // Fallback = what applies if the instance value is cleared (placeholder).
                let (fallback, fallback_source) = thread_title::resolve_title_model(kind, None)?;
                Some((
                    kind.to_string(),
                    serde_json::json!({
                        "model": model,
                        "source": source.as_str(),
                        "fallback_model": fallback,
                        "fallback_source": fallback_source.as_str(),
                        "env_var": thread_title::title_model_env(kind),
                    }),
                ))
            })
            .collect();
        obj.insert("title_models".into(), serde_json::Value::Object(title_models));
    }
    Ok(Json(val))
}

async fn update_instance_settings_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Json(body): Json<UpdateInstanceSettingsBody>,
) -> Result<StatusCode, ApiError> {
    auth_user.require_role(ROLE_ADMIN)?;
    let mut conn = database::open_auth_db(&state.auth_db_path)?;
    // IMMEDIATE: the read-merge-write below rewrites every column, so it must
    // not interleave with another PATCH (an admin save could otherwise write a
    // stale reorder mask over the owner's change).
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut settings = database::get_instance_settings(&tx)?;
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
    if let Some(mask) = body.reorder_children_role_bits {
        if mask != settings.reorder_children_role_bits {
            if !auth_user.has_role(ROLE_OWNER) {
                return Err(ApiError::forbidden(
                    "only the owner can change who may reorder child entries",
                ));
            }
            let grantable = database::grantable_role_bits(&tx)?;
            if mask & !grantable != 0 {
                return Err(ApiError::bad_request(
                    "reorder_children_role_bits may only contain bits of existing non-guest roles",
                ));
            }
            settings.reorder_children_role_bits = mask;
        }
    }
    for (kind, value) in [
        ("anthropic_http", body.title_model_anthropic_http),
        ("openai_compatible", body.title_model_openai_compatible),
        ("claude_cli", body.title_model_claude_cli),
        ("codex_cli", body.title_model_codex_cli),
    ] {
        if let Some(raw) = value {
            let model = validate_title_model(kind, &raw)?;
            if let Some(slot) = settings.title_model_slot_mut(kind) {
                *slot = model;
            }
        }
    }
    database::update_instance_settings(&tx, &settings)?;
    tx.commit()?;
    Ok(StatusCode::NO_CONTENT)
}

const MAX_TITLE_MODEL_CHARS: usize = 100;

/// Trims an admin-supplied title model; empty clears it (`None`). Rejects
/// overlong values and any whitespace/control characters inside.
fn validate_title_model(kind: &str, raw: &str) -> Result<Option<String>, ApiError> {
    let model = raw.trim();
    if model.is_empty() {
        return Ok(None);
    }
    if model.chars().count() > MAX_TITLE_MODEL_CHARS {
        return Err(ApiError::bad_request(&format!(
            "title_model_{kind} must be at most {MAX_TITLE_MODEL_CHARS} characters"
        )));
    }
    if model.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(ApiError::bad_request(&format!(
            "title_model_{kind} must not contain spaces or control characters"
        )));
    }
    Ok(Some(model.to_string()))
}

// ── yt-dlp tools ──────────────────────────────────────────────────────────────

use std::sync::atomic::{AtomicBool, Ordering};

/// Process-wide: the state dir the update writes into is process-global too.
static YT_DLP_UPDATE_RUNNING: AtomicBool = AtomicBool::new(false);

/// Holds the update slot; released on drop (including on panic in the blocking task).
struct YtDlpUpdateGuard;

impl YtDlpUpdateGuard {
    fn try_acquire() -> Option<Self> {
        YT_DLP_UPDATE_RUNNING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self)
    }
}

impl Drop for YtDlpUpdateGuard {
    fn drop(&mut self) {
        YT_DLP_UPDATE_RUNNING.store(false, Ordering::Release);
    }
}

/// `ToolsStatus` plus this server's live state: whether an update is running and the
/// cached JS runtime that new yt-dlp processes actually get.
fn tools_status_json(
    status: downloader::ytdlp_tools::ToolsStatus,
    in_use: Option<downloader::js_runtime::JsRuntime>,
) -> serde_json::Value {
    let mut val = serde_json::to_value(&status).unwrap_or_default();
    if let Some(obj) = val.as_object_mut() {
        obj.insert(
            "update_running".into(),
            serde_json::Value::Bool(YT_DLP_UPDATE_RUNNING.load(Ordering::Acquire)),
        );
        obj.insert(
            "js_runtime_in_use".into(),
            in_use.map_or(serde_json::Value::Null, |rt| {
                serde_json::json!({
                    "kind": rt.kind.as_str(),
                    "path": rt.path.map(|p| p.display().to_string()),
                })
            }),
        );
    }
    val
}

fn component_outcome(r: &anyhow::Result<String>) -> serde_json::Value {
    match r {
        Ok(m) => serde_json::json!({ "ok": true, "message": m }),
        Err(e) => serde_json::json!({ "ok": false, "message": format!("{e:#}") }),
    }
}

async fn get_yt_dlp_status_handler(
    auth_user: AuthUser,
) -> Result<Json<serde_json::Value>, ApiError> {
    auth_user.require_role(ROLE_ADMIN)?;
    // Both probe binaries (the first resolve_js_runtime() call may too): keep them off
    // the async workers.
    let (status, in_use) = tokio::task::spawn_blocking(|| {
        (
            downloader::ytdlp_tools::tools_status(),
            downloader::js_runtime::resolve_js_runtime(),
        )
    })
    .await
    .map_err(|e| ApiError::internal(&format!("yt-dlp status task failed: {e}")))?;
    Ok(Json(tools_status_json(status, in_use)))
}

async fn update_yt_dlp_handler(
    auth_user: AuthUser,
) -> Result<Json<serde_json::Value>, ApiError> {
    auth_user.require_role(ROLE_ADMIN)?;
    let guard = YtDlpUpdateGuard::try_acquire()
        .ok_or_else(|| ApiError::conflict("a yt-dlp update is already running"))?;
    // The guard moves into the blocking task, so a client disconnecting mid-update
    // can't release it before the install finishes.
    let (report, status, in_use) = tokio::task::spawn_blocking(move || {
        let _guard = guard;
        let report = downloader::ytdlp_tools::update_tools(
            None,
            concat!("archivr-server/", env!("CARGO_PKG_VERSION")),
            true,
            &mut |l| eprintln!("info: yt-dlp update: {l}"),
        )?;
        for (component, result) in [("yt-dlp", &report.yt_dlp), ("deno", &report.deno)] {
            if let Err(e) = result {
                eprintln!("warn: yt-dlp update: {component} failed: {e:#}");
            }
        }
        let status = downloader::ytdlp_tools::tools_status();
        let in_use = downloader::js_runtime::resolve_js_runtime();
        anyhow::Ok((report, status, in_use))
    })
    .await
    .map_err(|e| ApiError::internal(&format!("yt-dlp update task failed: {e}")))?
    .map_err(|e| ApiError::internal(&format!("{e:#}")))?;
    Ok(Json(serde_json::json!({
        "yt_dlp": component_outcome(&report.yt_dlp),
        "deno": component_outcome(&report.deno),
        "status": tools_status_json(status, in_use),
    })))
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
    if database::has_pending_subtitle_fetches(&conn)? {
        return Err(ApiError::conflict(
            "subtitle fetches are in progress; wait for summaries to finish before scanning",
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
    if database::has_pending_subtitle_fetches(&conn)? {
        return Err(ApiError::conflict(
            "subtitle fetches are in progress; wait for summaries to finish before cleaning up",
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
    if database::has_pending_subtitle_fetches(&conn)? {
        return Err(ApiError::conflict(
            "a subtitle fetch started during the scan; retry after summaries finish",
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
    let scope = body.scope.as_deref().unwrap_or("full");
    if scope != "full" && scope != "read" {
        return Err(ApiError::bad_request("scope must be 'full' or 'read'"));
    }
    let expires_at = match body.expires_in_days {
        None => None,
        Some(days) if (1..=3650).contains(&days) => Some(
            (chrono::Utc::now() + chrono::Duration::days(days)).to_rfc3339(),
        ),
        Some(_) => {
            return Err(ApiError::bad_request(
                "expires_in_days must be between 1 and 3650",
            ));
        }
    };
    let raw_token = auth::generate_token();
    let token_hash = auth::hash_token(&raw_token);
    let conn = database::open_auth_db(&state.auth_db_path)?;
    let token_uid = database::create_api_token(
        &conn,
        user_id,
        &token_hash,
        &body.name,
        expires_at.as_deref(),
        scope,
    )?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "token_uid": token_uid,
            "raw_token": raw_token,
            "name": body.name,
            "expires_at": expires_at,
            "scope": scope,
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
    reorder_children_role_bits: Option<u32>,
    title_model_anthropic_http: Option<String>,
    title_model_openai_compatible: Option<String>,
    title_model_claude_cli: Option<String>,
    title_model_codex_cli: Option<String>,
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
    let (caller_id, caller_bits) = auth_user.require_auth()?;
    if body.status != "active" && body.status != "disabled" {
        return Err(ApiError::bad_request(
            "status must be 'active' or 'disabled'",
        ));
    }
    let mut conn = database::open_auth_db(&state.auth_db_path)?;
    let target_id = database::get_user_id_by_uid(&conn, &uid)?
        .ok_or_else(|| ApiError::not_found("user not found"))?;
    if body.status == "disabled" {
        crate::guards::ensure_not_self(caller_id, target_id)?;
    }
    crate::guards::ensure_can_manage(caller_bits, database::compute_role_bits(&conn, target_id)?)?;
    let updated = crate::guards::with_write_lock(&mut conn, |tx| {
        if body.status == "disabled" {
            crate::guards::ensure_not_last_owner(tx, target_id)?;
        }
        Ok(database::set_user_status(tx, &uid, &body.status)?)
    })?;
    if !updated {
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
    let (caller_id, caller_bits) = auth_user.require_auth()?;
    let conn = database::open_auth_db(&state.auth_db_path)?;
    let target_id = database::get_user_id_by_uid(&conn, &uid)?
        .ok_or_else(|| ApiError::not_found("user not found"))?;
    crate::guards::ensure_can_manage(caller_bits, database::compute_role_bits(&conn, target_id)?)?;
    archivr_core::auth_users::get_role_by_slug(&conn, &body.role_slug)?
        .ok_or_else(|| ApiError::not_found("role not found"))?;
    if matches!(body.role_slug.as_str(), "owner" | "admin") && caller_bits & ROLE_OWNER == 0 {
        return Err(ApiError::forbidden(
            "only an owner can grant the owner or admin role",
        ));
    }
    database::assign_role(&conn, target_id, &body.role_slug, caller_id)?;
    Ok(StatusCode::OK)
}

async fn admin_remove_role(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path((uid, role_slug)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    auth_user.require_role(ROLE_ADMIN)?;
    let (_, caller_bits) = auth_user.require_auth()?;
    let mut conn = database::open_auth_db(&state.auth_db_path)?;
    let target_id = database::get_user_id_by_uid(&conn, &uid)?
        .ok_or_else(|| ApiError::not_found("user not found"))?;
    crate::guards::ensure_can_manage(caller_bits, database::compute_role_bits(&conn, target_id)?)?;
    archivr_core::auth_users::get_role_by_slug(&conn, &role_slug)?
        .ok_or_else(|| ApiError::not_found("role not found"))?;
    if matches!(role_slug.as_str(), "owner" | "admin") && caller_bits & ROLE_OWNER == 0 {
        return Err(ApiError::forbidden(
            "only an owner can remove the owner or admin role",
        ));
    }
    if role_slug == "owner" {
        // The has-role check, last-owner checks and the removal share one write lock.
        // Removing a role the user does not hold is a no-op.
        crate::guards::with_write_lock(&mut conn, |tx| {
            if !archivr_core::auth_users::user_has_role(tx, target_id, "owner")? {
                return Ok(());
            }
            crate::guards::ensure_not_last_owner(tx, target_id)?;
            // `remove_role` also refuses to drop the sole owner row (even a disabled one).
            if archivr_core::auth_users::count_role_holders(tx, "owner")? <= 1 {
                return Err(ApiError::conflict("cannot remove the last owner"));
            }
            Ok(database::remove_role(tx, target_id, "owner")?)
        })?;
    } else if archivr_core::auth_users::user_has_role(&conn, target_id, &role_slug)? {
        // Removing a role the user does not hold is a no-op.
        database::remove_role(&conn, target_id, &role_slug)?;
    }
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

pub(crate) fn auth_to_caller_bits(auth: &AuthUser) -> u32 {
    match auth {
        AuthUser::Authenticated { role_bits, .. } => *role_bits,
        AuthUser::Guest => ROLE_GUEST,
    }
}

/// Gate for every by-uid entry endpoint: a logged-in caller who cannot see the entry in the
/// lists (`database::caller_can_access_entry`) gets the same 404 as for an unknown uid, so
/// hiding an entry from a role also hides it from direct requests. ADMIN/OWNER always pass
/// without a query. Guests are a no-op here: endpoints that allow them check
/// `is_entry_publicly_accessible` themselves.
pub(crate) fn ensure_entry_visible(
    conn: &rusqlite::Connection,
    auth: &AuthUser,
    entry_uid: &str,
) -> Result<(), ApiError> {
    if let AuthUser::Authenticated { role_bits, .. } = auth
        && role_bits & (ROLE_ADMIN | ROLE_OWNER) == 0
        && !database::caller_can_access_entry(conn, entry_uid, *role_bits)?
    {
        return Err(ApiError::not_found("entry not found"));
    }
    Ok(())
}

/// Runs `change` (a mutation of the entry's collection memberships or their visibility) and, for
/// callers who are not admins, rolls it back with a 400 when it would leave the entry hidden from
/// the caller's own roles. Without this a plain user could hide an entry from every role they hold
/// and could never reach it again to undo it (only an admin could).
fn apply_without_self_lockout<T>(
    conn: &rusqlite::Connection,
    auth: &AuthUser,
    entry_uid: &str,
    change: impl FnOnce() -> Result<T, ApiError>,
) -> Result<T, ApiError> {
    let AuthUser::Authenticated { role_bits, .. } = auth else {
        return change();
    };
    if role_bits & (ROLE_ADMIN | ROLE_OWNER) != 0 {
        return change();
    }
    let tx = conn.unchecked_transaction()?;
    let out = change()?;
    if !database::caller_can_access_entry(conn, entry_uid, *role_bits)? {
        return Err(ApiError::bad_request(
            "this change would hide the entry from all of your own roles; ask an admin to do it",
        ));
    }
    tx.commit()?;
    Ok(out)
}

pub(crate) fn mounted_archive<'a>(
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
    pub(crate) status: StatusCode,
    pub(crate) message: String,
}

impl ApiError {
    pub(crate) fn not_found(message: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.to_string(),
        }
    }

    pub(crate) fn bad_request(message: &str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.to_string(),
        }
    }

    pub(crate) fn internal(message: &str) -> Self {
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

    pub(crate) fn conflict(message: &str) -> Self {
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
    auth: AuthUser,
    Path(archive_id): Path<String>,
) -> Result<Json<Vec<archive::CollectionSummary>>, ApiError> {
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    let all = archive::list_collections(&conn)?;
    // Guests only see public collections; authenticated users see all.
    let visible = if matches!(auth, AuthUser::Guest) {
        all.into_iter().filter(|c| !c.requires_auth).collect()
    } else {
        all
    };
    Ok(Json(visible))
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
        database::create_collection(&conn, &body.name, &body.slug, body.default_visibility_bits, body.requires_auth)
            .map_err(|e| ApiError::bad_request(&format!("{e:#}")))?;
    Ok((
        StatusCode::CREATED,
        Json(archive::CollectionSummary {
            collection_uid: record.collection_uid,
            name: record.name,
            slug: record.slug,
            default_visibility_bits: record.default_visibility_bits,
            requires_auth: record.requires_auth,
            created_at: record.created_at,
        }),
    ))
}

async fn get_collection_handler(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((archive_id, coll_uid)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mounted = mounted_archive(&state, &archive_id)?;
    let conn = database::open_or_initialize(&mounted.archive_path)?;
    let record = database::get_collection_by_uid(&conn, &coll_uid)?
        .ok_or(ApiError::not_found("collection not found"))?;
    if record.requires_auth {
        auth.require_auth()?;
    }
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
                "original_url": e.original_url,
                "collection_visibility_bits": vis,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({
        "collection_uid": record.collection_uid,
        "name": record.name,
        "slug": record.slug,
        "default_visibility_bits": record.default_visibility_bits,
        "requires_auth": record.requires_auth,
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
    ensure_entry_visible(&conn, &auth, &body.entry_uid)?;
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
    ensure_entry_visible(&conn, &auth, &entry_uid)?;
    let entry_id: i64 = conn
        .query_row(
            "SELECT id FROM archived_entries WHERE entry_uid = ?1",
            [entry_uid.as_str()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(ApiError::not_found("entry not found"))?;
    let removed = apply_without_self_lockout(&conn, &auth, &entry_uid, || {
        Ok(database::remove_entry_from_collection(&conn, coll.id, entry_id)?)
    })?;
    if removed {
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
    ensure_entry_visible(&conn, &auth, &entry_uid)?;
    let entry_id: i64 = conn
        .query_row(
            "SELECT id FROM archived_entries WHERE entry_uid = ?1",
            [entry_uid.as_str()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(ApiError::not_found("entry not found"))?;
    let updated = apply_without_self_lockout(&conn, &auth, &entry_uid, || {
        Ok(database::update_collection_entry_visibility(
            &conn,
            coll.id,
            entry_id,
            body.visibility_bits,
        )?)
    })?;
    if updated {
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
    ensure_entry_visible(&conn, &auth_user, &entry_uid)?;
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
        database::update_collection(&conn, &coll_uid, name_ref, body.default_visibility_bits, body.requires_auth)?;
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
    // Same rule (and status) as adding/removing entries: the default collection is permanent.
    // core also refuses, but as an anyhow error that would surface as a 500.
    if database::get_collection_by_uid(&conn, &coll_uid)?
        .is_some_and(|coll| coll.slug == "_default_")
    {
        return Err(ApiError::bad_request(
            "cannot delete the default collection",
        ));
    }
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

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn capture_text_title_request(
        archive: &str,
        body: &str,
        provider: &str,
        cookie: Option<&str>,
    ) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri(format!("/api/archives/{archive}/captures/text/title"))
            .header("content-type", "application/json");
        if let Some(cookie) = cookie {
            builder = builder.header("cookie", cookie);
        }
        builder
            .body(json_body(
                &serde_json::json!({"body": body, "provider": provider}),
            ))
            .unwrap()
    }

    #[tokio::test]
    async fn capture_options_only_exposes_safe_fields_to_users_and_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let cookie = make_role_session(&auth_path, "capture-user", &["user"]);
        let conn = database::open_auth_db(&auth_path).unwrap();
        let uid: i64 = conn
            .query_row(
                "SELECT id FROM users WHERE username = 'capture-user'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        database::create_api_token(
            &conn,
            uid,
            &auth::hash_token("capture-token"),
            "Extension",
            None,
            "full",
        )
        .unwrap();
        let mut settings = database::get_instance_settings(&conn).unwrap();
        settings.ublock_enabled = false;
        settings.cookie_ext_enabled = false;
        settings.modal_closer_enabled = false;
        settings.title_model_openai_compatible = Some("private-model".into());
        database::update_instance_settings(&conn, &settings).unwrap();
        for (header, value) in [
            ("cookie", cookie.as_str()),
            ("authorization", "Bearer capture-token"),
        ] {
            let response = app(registry.clone(), auth_path.clone())
                .oneshot(
                    Request::builder()
                        .uri("/api/captures/options")
                        .header(header, value)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let payload = body_json(response).await;
            let expected = [
                "ublock_enabled",
                "cookie_ext_enabled",
                "modal_closer_enabled",
                "ublock_ext_available",
                "cookie_ext_available",
                "reader_mode",
                "via_freedium",
                "download_subtitles",
                "title_providers",
            ];
            assert_eq!(payload.as_object().unwrap().len(), expected.len());
            for key in expected {
                assert!(payload.get(key).is_some(), "missing {key}");
            }
            assert_eq!(payload["ublock_enabled"], false);
            assert_eq!(payload["cookie_ext_enabled"], false);
            assert_eq!(payload["modal_closer_enabled"], false);
            assert_eq!(payload["reader_mode"], false);
            assert_eq!(payload["via_freedium"], true);
            assert_eq!(payload["download_subtitles"], true);
            for provider in payload["title_providers"].as_array().unwrap() {
                assert_eq!(provider.as_object().unwrap().len(), 2);
                assert!(summarizer::PROVIDER_KINDS.contains(&provider["kind"].as_str().unwrap()));
                assert!(provider["label"].as_str().is_some_and(|v| !v.is_empty()));
            }
            assert!(!payload.to_string().contains("private-model"));
            let admin = app(registry.clone(), auth_path.clone())
                .oneshot(
                    Request::builder()
                        .uri("/api/admin/instance-settings")
                        .header(header, value)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(admin.status(), StatusCode::FORBIDDEN);
        }
    }

    #[cfg(unix)]
    #[test]
    fn cli_provider_availability_searches_supplied_path_for_executable_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let filename = "archivr-fake-provider";
        for (directory, permissions) in [(&first, 0o644), (&second, 0o755)] {
            let file = directory.join(filename);
            std::fs::write(&file, "#!/bin/sh\nexit 99\n").unwrap();
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(permissions)).unwrap();
        }
        let search_path = std::env::join_paths([&first, &second]).unwrap();
        assert!(cli_executable_available(
            std::path::Path::new(filename),
            Some(&search_path)
        ));
        let nonexecutable_path = std::env::join_paths([&first]).unwrap();
        assert!(!cli_executable_available(
            std::path::Path::new(filename),
            Some(&nonexecutable_path)
        ));
        assert!(!cli_executable_available(
            std::path::Path::new(filename),
            None
        ));
        assert!(!cli_executable_available(
            std::path::Path::new("absent-provider"),
            Some(&search_path)
        ));
        assert!(cli_executable_available(&second.join(filename), None));
        assert!(!cli_executable_available(
            &first.join(filename),
            Some(&search_path)
        ));
        assert!(!cli_executable_available(&second, Some(&search_path)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn capture_options_only_advertises_executable_cli_providers() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        struct RestoreEnv(Vec<(&'static str, Option<std::ffi::OsString>)>);
        impl Drop for RestoreEnv {
            fn drop(&mut self) {
                for (name, previous) in &self.0 {
                    unsafe {
                        match previous {
                            Some(value) => std::env::set_var(name, value),
                            None => std::env::remove_var(name),
                        }
                    }
                }
            }
        }
        let _restore = RestoreEnv(
            ["ARCHIVR_CLAUDE_CLI", "ARCHIVR_CODEX_CLI"]
                .into_iter()
                .map(|name| (name, std::env::var_os(name)))
                .collect(),
        );
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let cookie = make_role_session(&auth_path, "options-cli-user", &["user"]);
        let executable = dir.path().join("fake-provider");
        // The script fails if called; listing providers must only inspect the file.
        std::fs::write(&executable, "#!/bin/sh\nexit 99\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let nonexecutable = dir.path().join("not-executable");
        std::fs::write(&nonexecutable, "provider").unwrap();
        std::fs::set_permissions(&nonexecutable, std::fs::Permissions::from_mode(0o644)).unwrap();
        let missing = dir.path().join("missing-provider");
        for (claude, codex, expected) in [
            (missing.as_path(), missing.as_path(), vec![]),
            (executable.as_path(), missing.as_path(), vec!["claude_cli"]),
            (missing.as_path(), executable.as_path(), vec!["codex_cli"]),
            (nonexecutable.as_path(), nonexecutable.as_path(), vec![]),
            (dir.path(), dir.path(), vec![]),
        ] {
            unsafe {
                std::env::set_var("ARCHIVR_CLAUDE_CLI", claude);
                std::env::set_var("ARCHIVR_CODEX_CLI", codex);
            }
            let response = app(registry.clone(), auth_path.clone())
                .oneshot(
                    Request::builder()
                        .uri("/api/captures/options")
                        .header("cookie", &cookie)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let payload = body_json(response).await;
            let actual: Vec<_> = payload["title_providers"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|provider| {
                    provider["kind"]
                        .as_str()
                        .filter(|kind| kind.ends_with("_cli"))
                })
                .collect();
            assert_eq!(actual, expected);
        }
    }

    #[tokio::test]
    async fn capture_options_and_text_title_require_user_role() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let conn = database::open_auth_db(&auth_path).unwrap();
        let owner_id: i64 = conn
            .query_row(
                "SELECT id FROM users WHERE username = 'testowner'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let guest = format!(
            "session={}",
            database::create_session(&conn, owner_id, ROLE_GUEST, None).unwrap()
        );
        for (cookie, expected) in [
            (None, StatusCode::UNAUTHORIZED),
            (Some(guest.as_str()), StatusCode::FORBIDDEN),
        ] {
            let mut request = Request::builder().uri("/api/captures/options");
            if let Some(cookie) = cookie {
                request = request.header("cookie", cookie);
            }
            let response = app(registry.clone(), auth_path.clone())
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
            let response = app(registry.clone(), auth_path.clone())
                .oneshot(capture_text_title_request(
                    "test",
                    "Note",
                    "claude_cli",
                    cookie,
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
        }
    }

    #[tokio::test]
    async fn capture_text_title_validates_body_archive_and_provider() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let cookie = make_role_session(&auth_path, "title-user", &["user"]);
        let oversized = "a".repeat(MAX_TEXT_CAPTURE_BODY_BYTES + 1);
        for (archive, body, provider, status, message) in [
            (
                "test",
                " \n\t ",
                "claude_cli",
                StatusCode::BAD_REQUEST,
                "body must not be empty",
            ),
            (
                "test",
                oversized.as_str(),
                "claude_cli",
                StatusCode::BAD_REQUEST,
                "body must not exceed 2 MiB",
            ),
            (
                "missing",
                "Note",
                "claude_cli",
                StatusCode::NOT_FOUND,
                "archive not found",
            ),
            (
                "test",
                "Note",
                "unknown",
                StatusCode::BAD_REQUEST,
                "unknown summary provider",
            ),
        ] {
            let response = app(registry.clone(), auth_path.clone())
                .oneshot(capture_text_title_request(
                    archive,
                    body,
                    provider,
                    Some(&cookie),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), status);
            let value = body_json(response).await;
            assert!(
                value["error"].as_str().unwrap().contains(message),
                "{value}"
            );
        }
    }

    #[tokio::test]
    async fn capture_text_title_uses_title_model_and_never_mutates_archive() {
        use std::io::{Read, Write};
        struct RestoreEnv(Vec<(&'static str, Option<std::ffi::OsString>)>);
        impl Drop for RestoreEnv {
            fn drop(&mut self) {
                for (name, previous) in &self.0 {
                    unsafe {
                        match previous {
                            Some(value) => std::env::set_var(name, value),
                            None => std::env::remove_var(name),
                        }
                    }
                }
            }
        }
        let _restore = RestoreEnv(
            [
                "ARCHIVR_OPENAI_API_KEY",
                "ARCHIVR_OPENAI_URL",
                "ARCHIVR_OPENAI_TITLE_MODEL",
                "ARCHIVR_UBLOCK_EXT",
                "ARCHIVR_COOKIE_EXT",
            ]
            .into_iter()
            .map(|name| (name, std::env::var_os(name)))
            .collect(),
        );
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let entry = make_test_entry(&archive_path);
        let cookie = make_role_session(&auth_path, "text-title-user", &["user"]);
        let conn = database::open_auth_db(&auth_path).unwrap();
        let uid: i64 = conn
            .query_row(
                "SELECT id FROM users WHERE username = 'text-title-user'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        database::create_api_token(
            &conn,
            uid,
            &auth::hash_token("text-title-token"),
            "Extension",
            None,
            "full",
        )
        .unwrap();
        let mut settings = database::get_instance_settings(&conn).unwrap();
        settings.title_model_openai_compatible = Some("instance-text-title".into());
        database::update_instance_settings(&conn, &settings).unwrap();
        unsafe {
            std::env::remove_var("ARCHIVR_OPENAI_API_KEY");
        }
        let missing = app(registry.clone(), auth_path.clone())
            .oneshot(capture_text_title_request(
                "test",
                "Note",
                "openai_compatible",
                Some(&cookie),
            ))
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
        assert!(
            body_json(missing).await["error"]
                .as_str()
                .unwrap()
                .contains("ARCHIVR_OPENAI_API_KEY")
        );

        let unavailable_options = app(registry.clone(), auth_path.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/captures/options")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let unavailable = body_json(unavailable_options).await;
        assert!(
            !unavailable["title_providers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|provider| provider["kind"] == "openai_compatible")
        );

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        unsafe {
            std::env::set_var("ARCHIVR_OPENAI_API_KEY", "test-secret");
            std::env::set_var("ARCHIVR_OPENAI_URL", endpoint);
            std::env::set_var("ARCHIVR_OPENAI_TITLE_MODEL", "ignored-env-model");
            std::env::set_var("ARCHIVR_UBLOCK_EXT", dir.path());
            std::env::set_var("ARCHIVR_COOKIE_EXT", dir.path().join("missing-extension"));
        }
        let available_options = app(registry.clone(), auth_path.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/captures/options")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let available = body_json(available_options).await;
        assert!(
            available["title_providers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|provider| provider["kind"] == "openai_compatible")
        );
        assert_eq!(available["ublock_ext_available"], true);
        assert_eq!(available["cookie_ext_available"], false);
        assert!(!available.to_string().contains("test-secret"));
        let stub = std::thread::spawn(move || {
            let mut prompts = Vec::new();
            for index in 0..4 {
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && std::time::Instant::now() < deadline =>
                        {
                            std::thread::sleep(Duration::from_millis(10))
                        }
                        Err(error) => panic!("stub accept failed: {error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut chunk = [0; 4096];
                let payload = loop {
                    let read = stream.read(&mut chunk).unwrap();
                    assert!(read > 0);
                    bytes.extend_from_slice(&chunk[..read]);
                    if let Some(head_end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..head_end]);
                        let length: usize = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse().unwrap())
                            })
                            .unwrap();
                        if bytes.len() >= head_end + 4 + length {
                            break serde_json::from_slice::<serde_json::Value>(
                                &bytes[head_end + 4..head_end + 4 + length],
                            )
                            .unwrap();
                        }
                    }
                };
                prompts.push(payload);
                let (status, reply) = if index == 2 {
                    ("502 Bad Gateway", "upstream failed".to_string())
                } else {
                    ("200 OK", serde_json::json!({"choices":[{"message":{"content": if index == 3 { "" } else { "```\nTitle: **Thread about Rust async runtimes**\n```" }}}]}).to_string())
                };
                write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
            }
            prompts
        });
        let archive_snapshot = || {
            let conn = database::open_or_initialize(&archive_path).unwrap();
            let mut snapshot = Vec::new();
            for table in [
                "archived_entries",
                "archive_runs",
                "entry_artifacts",
                "entry_summaries",
                "capture_jobs",
            ] {
                snapshot.push(
                    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| {
                        r.get::<_, i64>(0)
                    })
                    .unwrap(),
                );
            }
            let title = conn
                .query_row(
                    "SELECT title FROM archived_entries WHERE entry_uid = ?1",
                    [&entry.entry_uid],
                    |r| r.get::<_, Option<String>>(0),
                )
                .unwrap();
            (snapshot, title)
        };
        let before = archive_snapshot();
        for index in 0..4 {
            let request = if index == 0 {
                Request::builder().method("POST").uri("/api/archives/test/captures/text/title").header("content-type", "application/json").header("authorization", "Bearer text-title-token").body(json_body(&serde_json::json!({"body":"Rust runtime note", "provider":"openai_compatible"}))).unwrap()
            } else {
                // Exercise the full decoded limit and worst-case JSON expansion.
                let body = if index == 1 {
                    "\0".repeat(MAX_TEXT_CAPTURE_BODY_BYTES)
                } else {
                    "Note".into()
                };
                capture_text_title_request("test", &body, "openai_compatible", Some(&cookie))
            };
            let response = app(registry.clone(), auth_path.clone())
                .oneshot(request)
                .await
                .unwrap();
            let status = response.status();
            let value = body_json(response).await;
            if index < 2 {
                assert_eq!(status, StatusCode::OK, "{value}");
                assert_eq!(value, serde_json::json!({"title":"Rust async runtimes"}));
            } else {
                assert_eq!(status, StatusCode::BAD_GATEWAY, "{value}");
            }
        }
        let prompts = stub.join().unwrap();
        assert!(
            prompts
                .iter()
                .all(|body| body["model"] == "instance-text-title")
        );
        assert_eq!(
            prompts[1]["messages"][1]["content"]
                .as_str()
                .unwrap()
                .matches('\0')
                .count(),
            30_000
        );
        assert_eq!(archive_snapshot(), before);
    }

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

    /// Creates an active user holding `roles` (assign_role adds the cumulative ones:
    /// user for any non-guest, admin for owner) and returns a session cookie.
    fn make_role_session(auth_path: &std::path::Path, username: &str, roles: &[&str]) -> String {
        let conn = archivr_core::database::open_auth_db(auth_path).unwrap();
        let owner_id: i64 = conn
            .query_row(
                "SELECT id FROM users WHERE username = 'testowner'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let uid = database::create_user(&conn, username, None, "dummy", owner_id).unwrap();
        let user_id = database::get_user_id_by_uid(&conn, &uid).unwrap().unwrap();
        for role in roles {
            database::assign_role(&conn, user_id, role, owner_id).unwrap();
        }
        // assign_role deletes sessions, so create the session afterwards.
        let bits = database::compute_role_bits(&conn, user_id).unwrap();
        format!(
            "session={}",
            database::create_session(&conn, user_id, bits, None).unwrap()
        )
    }

    fn patch_settings_request(body: serde_json::Value, cookie: &str) -> Request<Body> {
        Request::builder()
            .method("PATCH")
            .uri("/api/admin/instance-settings")
            .header("content-type", "application/json")
            .header("cookie", cookie)
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    async fn get_settings_json(
        registry: ServerRegistry,
        auth_path: std::path::PathBuf,
        cookie: &str,
    ) -> serde_json::Value {
        let resp = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/admin/instance-settings")
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        body_json(resp).await
    }

    async fn me_json(
        registry: ServerRegistry,
        auth_path: std::path::PathBuf,
        cookie: &str,
    ) -> serde_json::Value {
        let resp = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/auth/me")
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        body_json(resp).await
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

    fn make_test_youtube_entry(
        archive_path: &std::path::Path,
        canonical_url: &str,
    ) -> archivr_core::database::ArchivedEntry {
        let conn = database::open_or_initialize(archive_path).unwrap();
        let user_id = database::ensure_default_user(&conn).unwrap();
        let run = database::create_archive_run(&conn, user_id, 1).unwrap();
        let si = database::upsert_source_identity(
            &conn,
            "youtube",
            "video",
            None,
            Some(canonical_url),
            canonical_url,
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
                source_kind: "youtube".to_string(),
                entity_kind: "video".to_string(),
                title: Some("Test Video".to_string()),
                visibility: "private".to_string(),
                representation_kind: "video".to_string(),
                source_metadata_json: "{}".to_string(),
                display_metadata_json: None,
            },
        )
        .unwrap()
    }

    fn make_test_child(
        archive_path: &std::path::Path,
        parent_id: i64,
        title: &str,
        url: &str,
    ) -> archivr_core::database::ArchivedEntry {
        let conn = database::open_or_initialize(archive_path).unwrap();
        let user_id = database::ensure_default_user(&conn).unwrap();
        let run = database::create_archive_run(&conn, user_id, 1).unwrap();
        let si = database::upsert_source_identity(&conn, "web", "page", None, Some(url), url)
            .unwrap();
        database::create_archived_entry(
            &conn,
            &database::NewEntry {
                source_identity_id: si,
                archive_run_id: run.id,
                parent_entry_id: Some(parent_id),
                root_entry_id: Some(parent_id),
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

    fn add_summary_test_artifact(
        archive_path: &std::path::Path,
        entry_id: i64,
        relpath: &str,
        role: &str,
        mime_type: &str,
        contents: &[u8],
    ) {
        let paths = archive::read_archive_paths(archive_path).unwrap();
        let full_path = paths.store_path.join(relpath);
        std::fs::create_dir_all(full_path.parent().unwrap()).unwrap();
        std::fs::write(&full_path, contents).unwrap();

        let conn = database::open_or_initialize(archive_path).unwrap();
        let blob_id = database::upsert_blob(
            &conn,
            &database::BlobRecord {
                sha256: format!("test-summary-{}", relpath.replace('/', "-")),
                byte_size: contents.len().try_into().unwrap(),
                mime_type: Some(mime_type.to_string()),
                extension: relpath.rsplit('.').next().map(str::to_string),
                raw_relpath: relpath.to_string(),
            },
        )
        .unwrap();
        database::add_entry_artifact(
            &conn,
            &database::NewArtifact {
                entry_id,
                artifact_role: role.to_string(),
                storage_area: "raw".to_string(),
                relpath: relpath.to_string(),
                blob_id: Some(blob_id),
                logical_path: None,
                metadata_json: None,
            },
        )
        .unwrap();
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
    async fn summary_include_images_rejects_claude_cli_without_creating_row() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let entry = make_test_entry(&archive_path);
        let session_cookie = make_test_session(&auth_path);

        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/archives/test/entries/{}/summary", entry.entry_uid))
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(json_body(&serde_json::json!({
                        "provider": "claude_cli",
                        "include_images": true,
                    })))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            body_json(response).await["error"],
            "Claude CLI cannot attach local images; choose an HTTP provider or Codex CLI"
        );
        let conn = database::open_or_initialize(&archive_path).unwrap();
        assert!(database::latest_entry_summary(&conn, entry.id).unwrap().is_none());
    }

    #[tokio::test]
    async fn summary_preflight_returns_safe_message_for_unsupported_video_content() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let entry = make_test_entry(&archive_path);
        add_summary_test_artifact(
            &archive_path,
            entry.id,
            "raw/private-video.mp4",
            "primary_media",
            "video/mp4",
            b"video fixture",
        );
        let session_cookie = make_test_session(&auth_path);
        let previous_codex_cli = std::env::var_os("ARCHIVR_CODEX_CLI");
        unsafe { std::env::set_var("ARCHIVR_CODEX_CLI", "/usr/bin/false") };

        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/archives/test/entries/{}/summary", entry.entry_uid))
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(json_body(&serde_json::json!({ "provider": "codex_cli" })))
                    .unwrap(),
            )
            .await
            .unwrap();
        match previous_codex_cli {
            Some(value) => unsafe { std::env::set_var("ARCHIVR_CODEX_CLI", value) },
            None => unsafe { std::env::remove_var("ARCHIVR_CODEX_CLI") },
        }

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let error = body_json(response).await["error"].as_str().unwrap().to_string();
        assert_eq!(error, summarizer::UNSUPPORTED_SUMMARY_CONTENT_MESSAGE);
        assert!(!error.contains("raw/"));
        assert!(!error.contains("mime"));
        assert!(!error.contains("v1 unsupported"));
    }

    #[tokio::test]
    async fn summary_request_for_missing_entry_returns_not_found_before_preflight() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _archive_path, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let previous_codex_cli = std::env::var_os("ARCHIVR_CODEX_CLI");
        unsafe { std::env::set_var("ARCHIVR_CODEX_CLI", "/usr/bin/false") };

        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/archives/test/entries/ent_missing/summary")
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(json_body(&serde_json::json!({ "provider": "codex_cli" })))
                    .unwrap(),
            )
            .await
            .unwrap();
        match previous_codex_cli {
            Some(value) => unsafe { std::env::set_var("ARCHIVR_CODEX_CLI", value) },
            None => unsafe { std::env::remove_var("ARCHIVR_CODEX_CLI") },
        }

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_json(response).await["error"], "entry not found");
    }

    #[tokio::test]
    async fn public_summary_endpoints_hide_failed_diagnostics_but_authenticated_users_keep_them() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let entry = make_test_entry(&archive_path);
        let session = make_test_session(&auth_path);
        let conn = database::open_or_initialize(&archive_path).unwrap();
        let completed_uid = database::upsert_pending_entry_summary(
            &conn, entry.id, "codex_cli", None, summarizer::PROMPT_VERSION, "completed-public-test",
        )
        .unwrap();
        database::update_entry_summary_status(
            &conn, &completed_uid, "completed", Some("previous completed summary"), None,
        )
        .unwrap();
        let summary_uid = database::upsert_pending_entry_summary(
            &conn, entry.id, "codex_cli", None, summarizer::PROMPT_VERSION, "failed-public-test",
        )
        .unwrap();
        database::update_entry_summary_status(
            &conn, &summary_uid, "failed", None, Some("provider secret: raw diagnostic"),
        )
        .unwrap();
        drop(conn);
        let collection = api_make_collection(
            registry.clone(), auth_path.clone(), &session, "Public summaries", "public-summaries", 3, false,
        )
        .await;
        api_add_to_coll(
            registry.clone(), auth_path.clone(), &session, &collection, &entry.entry_uid, 3,
        )
        .await;

        let public_summary = app(registry.clone(), auth_path.clone())
            .oneshot(Request::builder()
                .uri(format!("/api/archives/test/entries/{}/summary", entry.entry_uid))
                .body(Body::empty()).unwrap())
            .await.unwrap();
        assert_eq!(public_summary.status(), StatusCode::OK);
        let public_summary = body_json(public_summary).await;
        assert_eq!(public_summary["summary"]["summary_uid"], completed_uid);
        assert!(public_summary.get("attempt").is_none());

        let public_detail = app(registry.clone(), auth_path.clone())
            .oneshot(Request::builder()
                .uri(format!("/api/archives/test/entries/{}", entry.entry_uid))
                .body(Body::empty()).unwrap())
            .await.unwrap();
        assert_eq!(public_detail.status(), StatusCode::OK);
        let public_detail = body_json(public_detail).await;
        assert_eq!(public_detail["latest_summary"]["summary_uid"], completed_uid);
        assert!(public_detail["summary_attempt"].is_null());

        let authenticated_summary = app(registry.clone(), auth_path.clone())
            .oneshot(Request::builder()
                .uri(format!("/api/archives/test/entries/{}/summary", entry.entry_uid))
                .header("cookie", &session)
                .body(Body::empty()).unwrap())
            .await.unwrap();
        let authenticated_summary = body_json(authenticated_summary).await;
        assert_eq!(authenticated_summary["summary"]["summary_uid"], completed_uid);
        assert_eq!(authenticated_summary["summary"]["summary_text"], "previous completed summary");
        assert_eq!(authenticated_summary["attempt"]["status"], "failed");
        assert_eq!(authenticated_summary["attempt"]["error_text"], "provider secret: raw diagnostic");

        let authenticated_detail = app(registry, auth_path)
            .oneshot(Request::builder()
                .uri(format!("/api/archives/test/entries/{}", entry.entry_uid))
                .header("cookie", &session)
                .body(Body::empty()).unwrap())
            .await.unwrap();
        let authenticated_detail = body_json(authenticated_detail).await;
        assert_eq!(authenticated_detail["latest_summary"]["summary_uid"], completed_uid);
        assert_eq!(authenticated_detail["latest_summary"]["summary_text"], "previous completed summary");
        assert_eq!(authenticated_detail["summary_attempt"]["status"], "failed");
        assert_eq!(authenticated_detail["summary_attempt"]["error_text"], "provider secret: raw diagnostic");
    }

    #[test]
    fn background_summary_failure_stores_safe_copy_only_for_unsupported_content() {
        let dir = tempfile::tempdir().unwrap();
        let (_registry, archive_path, _auth_path) = make_test_registry(&dir);
        let entry = make_test_entry(&archive_path);
        add_summary_test_artifact(
            &archive_path,
            entry.id,
            "raw/private-video.mp4",
            "primary_media",
            "video/mp4",
            b"video fixture",
        );
        let unsupported = summarizer::build_summary_input(
            &archive::read_archive_paths(&archive_path).unwrap(),
            &entry.entry_uid,
            summarizer::SummaryBuildOptions::default(),
        )
        .unwrap_err();
        assert_eq!(
            summary_failure_error_text(&unsupported),
            summarizer::UNSUPPORTED_SUMMARY_CONTENT_MESSAGE
        );

        let conn = database::open_or_initialize(&archive_path).unwrap();
        let summary_uid = database::upsert_pending_entry_summary(
            &conn,
            entry.id,
            "codex_cli",
            Some("test"),
            summarizer::PROMPT_VERSION,
            "unsupported-content-test",
        )
        .unwrap();
        drop(conn);
        record_background_summary_failure(&archive_path, &summary_uid, &unsupported);
        let conn = database::open_or_initialize(&archive_path).unwrap();
        let stored = database::get_entry_summary_by_uid(&conn, &summary_uid)
            .unwrap()
            .unwrap();
        assert_eq!(stored.status, "failed");
        assert_eq!(
            stored.error_text.as_deref(),
            Some(summarizer::UNSUPPORTED_SUMMARY_CONTENT_MESSAGE)
        );

        let provider = anyhow::anyhow!("provider response was malformed");
        assert_eq!(
            summary_failure_error_text(&provider),
            "provider response was malformed"
        );
    }

    async fn post_codex_summary(
        registry: ServerRegistry,
        auth_path: std::path::PathBuf,
        entry_uid: &str,
    ) -> axum::response::Response {
        let session_cookie = make_test_session(&auth_path);
        let previous_codex_cli = std::env::var_os("ARCHIVR_CODEX_CLI");
        unsafe { std::env::set_var("ARCHIVR_CODEX_CLI", "/usr/bin/false") };
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/archives/test/entries/{entry_uid}/summary"))
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(json_body(&serde_json::json!({ "provider": "codex_cli" })))
                    .unwrap(),
            )
            .await
            .unwrap();
        match previous_codex_cli {
            Some(value) => unsafe { std::env::set_var("ARCHIVR_CODEX_CLI", value) },
            None => unsafe { std::env::remove_var("ARCHIVR_CODEX_CLI") },
        }
        response
    }

    #[tokio::test]
    async fn youtube_summary_without_subtitles_fails_row_with_clear_message() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        // Non-HTTP canonical URL: the subtitle fetch never spawns yt-dlp.
        let entry = make_test_youtube_entry(&archive_path, "youtube-test:offline");
        add_summary_test_artifact(
            &archive_path,
            entry.id,
            "raw/youtube-video.mp4",
            "primary_media",
            "video/mp4",
            b"video fixture",
        );

        let response = post_codex_summary(registry, auth_path, &entry.entry_uid).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = body_json(response).await;
        assert_eq!(body["status"], "pending");
        let summary_uid = body["summary_uid"].as_str().unwrap().to_string();

        let mut stored = None;
        for _ in 0..100 {
            let conn = database::open_or_initialize(&archive_path).unwrap();
            let row = database::get_entry_summary_by_uid(&conn, &summary_uid)
                .unwrap()
                .unwrap();
            if row.status == "failed" {
                stored = Some(row);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let stored = stored.expect("summary row should fail");
        // `/usr/bin/false` would yield a different error, so this also proves
        // the provider never ran.
        assert_eq!(
            stored.error_text.as_deref(),
            Some(summarizer::NO_SUBTITLES_SUMMARY_MESSAGE)
        );
        assert_eq!(stored.input_sha256, summarizer::SUBTITLE_FETCH_PENDING_INPUT_SHA256);
    }

    #[tokio::test]
    async fn youtube_summary_with_subtitle_artifact_preflights_synchronously() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let entry = make_test_youtube_entry(&archive_path, "youtube-test:offline");
        add_summary_test_artifact(
            &archive_path,
            entry.id,
            "raw/youtube-video.mp4",
            "primary_media",
            "video/mp4",
            b"video fixture",
        );
        add_summary_test_artifact(
            &archive_path,
            entry.id,
            "raw/youtube-video.en.vtt",
            "subtitle",
            "text/vtt",
            b"WEBVTT\n\n00:00:00.000 --> 00:00:02.000\nHello from the transcript\n",
        );

        let response = post_codex_summary(registry, auth_path, &entry.entry_uid).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let summary_uid = body_json(response).await["summary_uid"]
            .as_str()
            .unwrap()
            .to_string();
        let conn = database::open_or_initialize(&archive_path).unwrap();
        let stored = database::get_entry_summary_by_uid(&conn, &summary_uid)
            .unwrap()
            .unwrap();
        assert_ne!(stored.input_sha256, summarizer::SUBTITLE_FETCH_PENDING_INPUT_SHA256);
    }

    #[test]
    fn background_summary_failure_maps_no_subtitles_copy() {
        let dir = tempfile::tempdir().unwrap();
        let (_registry, archive_path, _auth_path) = make_test_registry(&dir);
        let entry = make_test_youtube_entry(&archive_path, "youtube-test:offline");
        let no_subtitles = summarizer::build_summary_input(
            &archive::read_archive_paths(&archive_path).unwrap(),
            &entry.entry_uid,
            summarizer::SummaryBuildOptions::default(),
        )
        .unwrap_err();
        assert!(summarizer::is_no_subtitles_error(&no_subtitles));
        assert_eq!(
            summary_failure_error_text(&no_subtitles),
            summarizer::NO_SUBTITLES_SUMMARY_MESSAGE
        );
    }

    #[test]
    fn capture_body_download_subtitles_defaults_to_none() {
        let absent: CaptureBody =
            serde_json::from_value(serde_json::json!({ "locator": "x" })).unwrap();
        assert_eq!(absent.download_subtitles, None);
        assert!(absent.download_subtitles.unwrap_or(true));
        let disabled: CaptureBody = serde_json::from_value(
            serde_json::json!({ "locator": "x", "download_subtitles": false }),
        )
        .unwrap();
        assert_eq!(disabled.download_subtitles, Some(false));
    }

    #[tokio::test]
    async fn summary_include_images_uses_distinct_cache_identity() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let entry = make_test_entry(&archive_path);
        add_summary_test_artifact(
            &archive_path,
            entry.id,
            "raw/summary-test.html",
            "primary_media",
            "text/html",
            b"<article>summary fixture</article>",
        );
        add_summary_test_artifact(
            &archive_path,
            entry.id,
            "raw/summary-test.jpg",
            "media",
            "image/jpeg",
            b"image fixture",
        );
        let session_cookie = make_test_session(&auth_path);
        let previous_codex_cli = std::env::var_os("ARCHIVR_CODEX_CLI");
        unsafe { std::env::set_var("ARCHIVR_CODEX_CLI", "/usr/bin/false") };

        let text_response = app(registry.clone(), auth_path.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/archives/test/entries/{}/summary", entry.entry_uid))
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(json_body(&serde_json::json!({ "provider": "codex_cli" })))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(text_response.status(), StatusCode::ACCEPTED);

        let image_response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/archives/test/entries/{}/summary", entry.entry_uid))
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(json_body(&serde_json::json!({
                        "provider": "codex_cli",
                        "include_images": true,
                    })))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(image_response.status(), StatusCode::ACCEPTED);
        match previous_codex_cli {
            Some(value) => unsafe { std::env::set_var("ARCHIVR_CODEX_CLI", value) },
            None => unsafe { std::env::remove_var("ARCHIVR_CODEX_CLI") },
        }

        let conn = database::open_or_initialize(&archive_path).unwrap();
        let input_hashes: Vec<String> = conn
            .prepare("SELECT input_sha256 FROM entry_summaries WHERE entry_id = ?1")
            .unwrap()
            .query_map([entry.id], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(input_hashes.len(), 2);
        assert_ne!(input_hashes[0], input_hashes[1]);
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
    async fn login_response_includes_can_reorder_children() {
        let dir = tempfile::tempdir().unwrap();
        let auth_path = dir.path().join("auth.sqlite");
        {
            let conn = archivr_core::database::open_auth_db(&auth_path).unwrap();
            let hash = crate::auth::hash_password("pw").unwrap();
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
                    .body(Body::from(r#"{"username":"owner","password":"pw"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await["can_reorder_children"], true);
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
    async fn text_capture_post_returns_accepted_with_job_uid() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/archives/test/captures/text")
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(Body::from(
                        r#"{"title":"Test Note","body":"Test content","mime":"text/markdown"}"#,
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
    async fn text_capture_accepts_a_body_just_below_two_mebibytes() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let body = "a".repeat(2 * 1024 * 1024 - 1);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/archives/test/captures/text")
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(json_body(&serde_json::json!({
                        "title": "Maximum-size note",
                        "body": body,
                        "mime": "text/plain"
                    })))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn text_capture_accepts_a_two_mebibyte_body_with_json_escaped_controls() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let body = "\0".repeat(2 * 1024 * 1024);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/archives/test/captures/text")
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(json_body(&serde_json::json!({
                        "title": "Escaped control note",
                        "body": body,
                        "mime": "text/plain"
                    })))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn text_capture_rejects_a_body_over_two_mebibytes_with_validation_error() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let body = "a".repeat(2 * 1024 * 1024 + 1);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/archives/test/captures/text")
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(json_body(&serde_json::json!({
                        "title": "Oversized note",
                        "body": body,
                        "mime": "text/plain"
                    })))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let response_body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&response_body).unwrap();
        assert_eq!(json["error"], "body must not exceed 2 MiB");
    }

    #[tokio::test]
    async fn text_capture_post_preserves_intentional_whitespace() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let text_body = "  \n# Heading\n\nContent with a final newline\n\t \n";
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/archives/test/captures/text")
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(json_body(&serde_json::json!({
                        "title": "Whitespace Note",
                        "body": text_body,
                        "mime": "text/markdown"
                    })))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let response_body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let job_uid = serde_json::from_slice::<serde_json::Value>(&response_body).unwrap()["job_uid"]
            .as_str()
            .unwrap()
            .to_string();

        let status = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let conn = archivr_core::database::open_or_initialize(&archive_path).unwrap();
                let job = archivr_core::database::get_capture_job(&conn, &job_uid)
                    .unwrap()
                    .unwrap();
                if job.status != "pending" && job.status != "running" {
                    break job.status;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("text capture job should finish");
        assert_eq!(status, "completed");

        let archive_paths = archivr_core::archive::read_archive_paths(&archive_path).unwrap();
        let conn = archivr_core::database::open_or_initialize(&archive_path).unwrap();
        let raw_relpath: String = conn
            .query_row(
                "SELECT b.raw_relpath
                 FROM entry_artifacts ea
                 JOIN blobs b ON b.id = ea.blob_id
                 WHERE ea.artifact_role = 'primary_media'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            std::fs::read(archive_paths.store_path.join(raw_relpath)).unwrap(),
            text_body.as_bytes()
        );
    }

    #[tokio::test]
    async fn text_capture_rejects_empty_title() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/archives/test/captures/text")
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(Body::from(
                        r#"{"title":"","body":"Test content","mime":"text/plain"}"#,
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
        assert!(json["error"]
            .as_str()
            .is_some_and(|e| e.contains("title must not be empty")));
    }

    #[tokio::test]
    async fn text_capture_rejects_empty_body() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/archives/test/captures/text")
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(Body::from(
                        r#"{"title":"Test Note","body":"","mime":"text/plain"}"#,
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
        assert!(json["error"]
            .as_str()
            .is_some_and(|e| e.contains("body must not be empty")));
    }

    #[tokio::test]
    async fn text_capture_rejects_unsupported_mime() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/archives/test/captures/text")
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(Body::from(
                        r#"{"title":"Test Note","body":"Test content","mime":"text/html"}"#,
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
        assert!(json["error"]
            .as_str()
            .is_some_and(|e| e.contains("unsupported MIME type")));
    }

    #[tokio::test]
    async fn text_capture_requires_auth() {
        let (test_app, _dir) = make_test_app();
        let response = test_app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/archives/test/captures/text")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"title":"Test","body":"Content"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
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
        assert_eq!(json["reorder_children_role_bits"], 12);
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
    async fn yt_dlp_status_requires_admin() {
        let (test_app, _dir) = make_test_app();
        let response = test_app
            .oneshot(
                Request::builder()
                    .uri("/api/admin/yt-dlp")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let plain = make_role_session(&auth_path, "plain", &["user"]);
        let app = app(registry, auth_path);
        for (method, uri) in [("GET", "/api/admin/yt-dlp"), ("POST", "/api/admin/yt-dlp/update")] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .header("cookie", &plain)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{method} {uri}");
        }
    }

    #[tokio::test]
    async fn yt_dlp_status_returns_candidates() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/admin/yt-dlp")
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        let yt_dlp = json["yt_dlp"].as_array().expect("yt_dlp array");
        assert_eq!(yt_dlp.len(), 4);
        for row in yt_dlp {
            assert!(row["role"].is_string() && row["label"].is_string(), "{row}");
            assert!(row["chosen"].is_boolean(), "{row}");
        }
        assert_eq!(json["js_runtime"].as_array().map(Vec::len), Some(4));
        assert!(json["yt_dlp_chosen"]["path"].is_string());
        // Value not asserted: the concurrent-run test may hold the guard right now.
        assert!(json["update_running"].is_boolean());
        let in_use = json.get("js_runtime_in_use").expect("js_runtime_in_use key");
        assert!(in_use.is_null() || in_use["kind"].is_string(), "{in_use}");
    }

    #[tokio::test]
    async fn yt_dlp_update_rejects_concurrent_run() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let guard = YtDlpUpdateGuard::try_acquire().expect("no other update running");
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/admin/yt-dlp/update")
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let json = body_json(response).await;
        assert!(
            json["error"].as_str().is_some_and(|e| e.contains("already running")),
            "{json}"
        );
        drop(guard);
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
            media_tokens: Arc::new(Mutex::new(HashMap::new())),
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
            media_tokens: Arc::new(Mutex::new(HashMap::new())),
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
    async fn list_collections_is_public() {
        // list_collections no longer requires auth — collection summaries are public metadata
        // needed for the collection switcher in guest mode.
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
        assert_eq!(response.status(), StatusCode::OK);
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
        // Create a collection (requires_auth defaults to true), then confirm
        // that an unauthenticated GET returns 401, not 404.
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let create_resp = app(registry.clone(), auth_path.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/archives/test/collections")
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(json_body(&serde_json::json!({
                        "name": "Auth Required",
                        "slug": "auth-required",
                        "default_visibility_bits": 2,
                        "requires_auth": true
                    })))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create_resp.status(), StatusCode::CREATED);
        let coll = body_json(create_resp).await;
        let coll_uid = coll["collection_uid"].as_str().unwrap().to_string();
        // Now GET without auth — must be 401 because requires_auth == true.
        let uri = format!("/api/archives/test/collections/{coll_uid}");
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri(&uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn get_public_collection_no_auth_returns_ok() {
        // A collection with requires_auth=false must be reachable by guests.
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let create_resp = app(registry.clone(), auth_path.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/archives/test/collections")
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(json_body(&serde_json::json!({
                        "name": "Public Collection",
                        "slug": "public-coll",
                        "default_visibility_bits": 3,
                        "requires_auth": false
                    })))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create_resp.status(), StatusCode::CREATED);
        let coll = body_json(create_resp).await;
        let coll_uid = coll["collection_uid"].as_str().unwrap().to_string();
        assert_eq!(coll["requires_auth"], false, "requires_auth should be false");
        // GET without any auth cookie — must be 200.
        let uri = format!("/api/archives/test/collections/{coll_uid}");
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri(&uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["requires_auth"], false);
        assert_eq!(body["name"], "Public Collection");
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

    #[tokio::test]
    async fn deleting_the_default_collection_is_a_400_not_a_500() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let router = app(registry, auth_path);
        let list = router.clone().oneshot(Request::builder()
            .uri("/api/archives/test/collections").header("cookie", &session_cookie)
            .body(Body::empty()).unwrap()).await.unwrap();
        let collections = body_json(list).await;
        let default_uid = collections.as_array().unwrap().iter()
            .find(|c| c["slug"] == "_default_").unwrap()["collection_uid"].as_str().unwrap().to_string();
        let response = router.oneshot(Request::builder().method("DELETE")
            .uri(format!("/api/archives/test/collections/{default_uid}"))
            .header("cookie", &session_cookie).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(response).await["error"], "cannot delete the default collection");
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

    // ── Collection-scoped entries + search security tests ──────────────────

    #[tokio::test]
    async fn list_entries_public_named_collection_allows_guest() {
        // A named collection with requires_auth=false must be accessible without a cookie.
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        // Create a public collection.
        let create = app(registry.clone(), auth_path.clone())
            .oneshot(Request::builder().method("POST")
                .uri("/api/archives/test/collections")
                .header("content-type", "application/json")
                .header("cookie", &session_cookie)
                .body(json_body(&serde_json::json!({
                    "name": "Public Coll", "slug": "public-coll",
                    "default_visibility_bits": 3, "requires_auth": false
                }))).unwrap()).await.unwrap();
        assert_eq!(create.status(), StatusCode::CREATED);
        let coll = body_json(create).await;
        let uid = coll["collection_uid"].as_str().unwrap().to_string();
        // Guest access to entries scoped to that collection → 200.
        let resp = app(registry, auth_path)
            .oneshot(Request::builder()
                .uri(format!("/api/archives/test/entries?collection={uid}"))
                .body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn list_entries_auth_required_named_collection_blocks_guest() {
        // A named collection with requires_auth=true must block guests.
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let create = app(registry.clone(), auth_path.clone())
            .oneshot(Request::builder().method("POST")
                .uri("/api/archives/test/collections")
                .header("content-type", "application/json")
                .header("cookie", &session_cookie)
                .body(json_body(&serde_json::json!({
                    "name": "Private Coll", "slug": "private-coll",
                    "default_visibility_bits": 2, "requires_auth": true
                }))).unwrap()).await.unwrap();
        assert_eq!(create.status(), StatusCode::CREATED);
        let coll = body_json(create).await;
        let uid = coll["collection_uid"].as_str().unwrap().to_string();
        // Guest access → 401.
        let resp = app(registry, auth_path)
            .oneshot(Request::builder()
                .uri(format!("/api/archives/test/entries?collection={uid}"))
                .body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_entries_public_default_collection_allows_guest() {
        // When the _default_ collection is set to requires_auth=false, guests can list entries.
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        // PATCH the default collection to make it public.
        let default_uid = {
            let conn = database::open_or_initialize(&archive_path).unwrap();
            database::get_collection_by_slug(&conn, "_default_").unwrap().unwrap().collection_uid
        };
        let patch = app(registry.clone(), auth_path.clone())
            .oneshot(Request::builder().method("PATCH")
                .uri(format!("/api/archives/test/collections/{default_uid}"))
                .header("content-type", "application/json")
                .header("cookie", &session_cookie)
                .body(json_body(&serde_json::json!({ "requires_auth": false }))).unwrap())
            .await.unwrap();
        assert_eq!(patch.status(), StatusCode::NO_CONTENT);
        // Guest access to /entries (no collection param → resolves to _default_) → 200.
        let resp = app(registry, auth_path)
            .oneshot(Request::builder()
                .uri("/api/archives/test/entries")
                .body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn search_entries_public_collection_allows_guest() {
        // Guest can search within a public collection.
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let create = app(registry.clone(), auth_path.clone())
            .oneshot(Request::builder().method("POST")
                .uri("/api/archives/test/collections")
                .header("content-type", "application/json")
                .header("cookie", &session_cookie)
                .body(json_body(&serde_json::json!({
                    "name": "Public Search", "slug": "public-search",
                    "default_visibility_bits": 3, "requires_auth": false
                }))).unwrap()).await.unwrap();
        assert_eq!(create.status(), StatusCode::CREATED);
        let coll = body_json(create).await;
        let uid = coll["collection_uid"].as_str().unwrap().to_string();
        // Guest search scoped to that collection → 200.
        let resp = app(registry, auth_path)
            .oneshot(Request::builder()
                .uri(format!("/api/archives/test/entries/search?collection={uid}&q=test"))
                .body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn search_entries_auth_required_collection_blocks_guest() {
        // Guest cannot search within an auth-required collection.
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let create = app(registry.clone(), auth_path.clone())
            .oneshot(Request::builder().method("POST")
                .uri("/api/archives/test/collections")
                .header("content-type", "application/json")
                .header("cookie", &session_cookie)
                .body(json_body(&serde_json::json!({
                    "name": "Private Search", "slug": "private-search",
                    "default_visibility_bits": 2, "requires_auth": true
                }))).unwrap()).await.unwrap();
        assert_eq!(create.status(), StatusCode::CREATED);
        let coll = body_json(create).await;
        let uid = coll["collection_uid"].as_str().unwrap().to_string();
        // Guest search → 401.
        let resp = app(registry, auth_path)
            .oneshot(Request::builder()
                .uri(format!("/api/archives/test/entries/search?collection={uid}&q=test"))
                .body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn collection_scoped_visibility_does_not_leak_across_collections() {
        // Entry is users-only (visibility_bits=2) in CollA and public (visibility_bits=3)
        // in CollB. Guest list + search of CollA must return 0 results; CollB must return 1.
        // This catches the bug where cross-collection visibility check would return the entry
        // because it is public *somewhere*, regardless of the requested collection.
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);

        // Two public collections: CollA (default vis=2) and CollB (default vis=3).
        let coll_a_uid = {
            let r = app(registry.clone(), auth_path.clone())
                .oneshot(Request::builder().method("POST")
                    .uri("/api/archives/test/collections")
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(json_body(&serde_json::json!({
                        "name": "CollA", "slug": "coll-a",
                        "default_visibility_bits": 2, "requires_auth": false
                    }))).unwrap()).await.unwrap();
            assert_eq!(r.status(), StatusCode::CREATED);
            body_json(r).await["collection_uid"].as_str().unwrap().to_string()
        };
        let coll_b_uid = {
            let r = app(registry.clone(), auth_path.clone())
                .oneshot(Request::builder().method("POST")
                    .uri("/api/archives/test/collections")
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(json_body(&serde_json::json!({
                        "name": "CollB", "slug": "coll-b",
                        "default_visibility_bits": 3, "requires_auth": false
                    }))).unwrap()).await.unwrap();
            assert_eq!(r.status(), StatusCode::CREATED);
            body_json(r).await["collection_uid"].as_str().unwrap().to_string()
        };

        // Create a fixture entry (title "Test Entry" is searchable).
        let entry = make_test_entry(&archive_path);

        // Add entry to CollA with visibility_bits=2 (users-only in CollA).
        let add_a = app(registry.clone(), auth_path.clone())
            .oneshot(Request::builder().method("POST")
                .uri(format!("/api/archives/test/collections/{coll_a_uid}/entries"))
                .header("content-type", "application/json")
                .header("cookie", &session_cookie)
                .body(json_body(&serde_json::json!({
                    "entry_uid": entry.entry_uid, "visibility_bits": 2
                }))).unwrap()).await.unwrap();
        assert_eq!(add_a.status(), StatusCode::NO_CONTENT);

        // Add same entry to CollB with visibility_bits=3 (public in CollB).
        let add_b = app(registry.clone(), auth_path.clone())
            .oneshot(Request::builder().method("POST")
                .uri(format!("/api/archives/test/collections/{coll_b_uid}/entries"))
                .header("content-type", "application/json")
                .header("cookie", &session_cookie)
                .body(json_body(&serde_json::json!({
                    "entry_uid": entry.entry_uid, "visibility_bits": 3
                }))).unwrap()).await.unwrap();
        assert_eq!(add_b.status(), StatusCode::NO_CONTENT);

        // ── list_entries scoping ──────────────────────────────────────────
        // Guest list CollA → 0 results (users-only there).
        let list_a = app(registry.clone(), auth_path.clone())
            .oneshot(Request::builder()
                .uri(format!("/api/archives/test/entries?collection={coll_a_uid}"))
                .body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(list_a.status(), StatusCode::OK);
        let body_a = body_json(list_a).await;
        assert_eq!(body_a.as_array().unwrap().len(), 0,
            "guest must not see users-only entry in CollA via list");

        // Guest list CollB → 1 result (public there).
        let list_b = app(registry.clone(), auth_path.clone())
            .oneshot(Request::builder()
                .uri(format!("/api/archives/test/entries?collection={coll_b_uid}"))
                .body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(list_b.status(), StatusCode::OK);
        let body_b = body_json(list_b).await;
        assert_eq!(body_b.as_array().unwrap().len(), 1,
            "guest should see public entry in CollB via list");

        // ── search_entries scoping ────────────────────────────────────────
        // Guest search CollA → 0 results (entry is users-only there, not leaked by CollB).
        let search_a = app(registry.clone(), auth_path.clone())
            .oneshot(Request::builder()
                .uri(format!("/api/archives/test/entries/search?collection={coll_a_uid}&q=Test"))
                .body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(search_a.status(), StatusCode::OK);
        let srch_a = body_json(search_a).await;
        assert_eq!(srch_a.as_array().unwrap().len(), 0,
            "guest search in CollA must not return entry visible only in CollB");

        // Guest search CollB → 1 result (public there).
        let search_b = app(registry, auth_path)
            .oneshot(Request::builder()
                .uri(format!("/api/archives/test/entries/search?collection={coll_b_uid}&q=Test"))
                .body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(search_b.status(), StatusCode::OK);
        let srch_b = body_json(search_b).await;
        assert_eq!(srch_b.as_array().unwrap().len(), 1,
            "guest search in CollB must return the public entry");
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
                    .uri(format!("/api/archives/test/entries/{}", entry.entry_uid))
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
                    .uri(format!("/api/archives/test/entries/{}", entry.entry_uid))
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

    fn make_test_thread_entry(archive_path: &std::path::Path) -> archivr_core::database::ArchivedEntry {
        let conn = database::open_or_initialize(archive_path).unwrap();
        let user_id = database::ensure_default_user(&conn).unwrap();
        let run = database::create_archive_run(&conn, user_id, 1).unwrap();
        let si = database::upsert_source_identity(
            &conn,
            "x",
            "tweet_thread",
            Some("9001"),
            Some("https://x.com/alice/status/9001"),
            "x:thread:9001",
        )
        .unwrap();
        let entry = database::create_archived_entry(
            &conn,
            &database::NewEntry {
                source_identity_id: si,
                archive_run_id: run.id,
                parent_entry_id: None,
                root_entry_id: None,
                created_by_user_id: user_id,
                owned_by_user_id: user_id,
                source_kind: "x".to_string(),
                entity_kind: "tweet_thread".to_string(),
                title: Some("Thread by @alice".to_string()),
                visibility: "private".to_string(),
                representation_kind: "tweet_thread".to_string(),
                source_metadata_json: r#"{"tweet_id":"9001"}"#.to_string(),
                display_metadata_json: None,
            },
        )
        .unwrap();
        add_summary_test_artifact(
            archive_path,
            entry.id,
            "raw_tweets/tweet-9001.json",
            "raw_tweet_json",
            "application/json",
            br#"{"full_text":"1/ Comparing Rust async runtimes.","author":{"screen_name":"alice"}}"#,
        );
        add_summary_test_artifact(
            archive_path,
            entry.id,
            "raw_tweets/tweet-9002.json",
            "raw_tweet_json",
            "application/json",
            br#"{"full_text":"2/ Tokio wins on ecosystem.","author":{"screen_name":"alice"}}"#,
        );
        entry
    }

    fn thread_title_request(entry_uid: &str, provider: &str, cookie: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri(format!("/api/archives/test/entries/{entry_uid}/thread-title"))
            .header("content-type", "application/json");
        if let Some(cookie) = cookie {
            builder = builder.header("cookie", cookie);
        }
        builder
            .body(json_body(&serde_json::json!({ "provider": provider })))
            .unwrap()
    }

    #[tokio::test]
    async fn thread_title_requires_auth() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let entry = make_test_thread_entry(&archive_path);
        let response = app(registry, auth_path)
            .oneshot(thread_title_request(&entry.entry_uid, "claude_cli", None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn thread_title_unknown_entry_returns_404() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let response = app(registry, auth_path)
            .oneshot(thread_title_request("no-such-uid", "claude_cli", Some(&session_cookie)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_json(response).await["error"], "entry not found");
    }

    #[tokio::test]
    async fn thread_title_rejects_non_thread_entry() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let entry = make_test_entry(&archive_path);
        let response = app(registry, auth_path)
            .oneshot(thread_title_request(&entry.entry_uid, "claude_cli", Some(&session_cookie)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let error = body_json(response).await["error"].as_str().unwrap().to_string();
        assert!(error.contains("not an X thread"), "{error}");
    }

    #[test]
    fn thread_title_load_error_maps_user_errors_to_400_and_others_to_500() {
        let user = anyhow::Error::new(thread_title::ThreadTitleUserError("not a thread".into()));
        let e = thread_title_load_error("uid", user);
        assert_eq!(e.status, StatusCode::BAD_REQUEST);
        assert_eq!(e.message, "not a thread");

        let io = anyhow::anyhow!("disk gone").context("failed to read /abs/store/x.json");
        let e = thread_title_load_error("uid", io);
        assert_eq!(e.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!e.message.contains("/abs"), "{}", e.message);
    }

    #[tokio::test]
    async fn thread_title_unknown_provider_returns_400() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let entry = make_test_thread_entry(&archive_path);
        let response = app(registry, auth_path)
            .oneshot(thread_title_request(&entry.entry_uid, "gemini", Some(&session_cookie)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let error = body_json(response).await["error"].as_str().unwrap().to_string();
        assert!(error.contains("unknown summary provider"), "{error}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn thread_title_generates_and_persists() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let entry = make_test_thread_entry(&archive_path);
        let script = dir.path().join("fake-claude");
        std::fs::write(
            &script,
            "#!/bin/sh\ncat >/dev/null\ncase \" $* \" in *\" --model haiku \"*) printf '%s\\n' '\"Rust async runtimes compared.\"';; *\" --model custom-title-1 \"*) printf '%s\\n' 'Instance model topic';; *) echo \"bad args: $*\" >&2; exit 3;; esac\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let previous_cli = std::env::var_os("ARCHIVR_CLAUDE_CLI");
        let previous_model = std::env::var_os("ARCHIVR_CLAUDE_TITLE_MODEL");
        unsafe {
            std::env::set_var("ARCHIVR_CLAUDE_CLI", &script);
            std::env::remove_var("ARCHIVR_CLAUDE_TITLE_MODEL");
        }
        // Parallel tests forking while the script fd was open can briefly make
        // exec fail with ETXTBSY (rust-lang/rust#114554); retry that case only.
        let run = || async {
            let mut attempt = 0;
            loop {
                let response = app(registry.clone(), auth_path.clone())
                    .oneshot(thread_title_request(&entry.entry_uid, "claude_cli", Some(&session_cookie)))
                    .await
                    .unwrap();
                let status = response.status();
                let body = body_json(response).await;
                let busy = body["error"].as_str().is_some_and(|e| e.contains("Text file busy"));
                if busy && attempt < 20 {
                    attempt += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    continue;
                }
                break (status, body);
            }
        };
        let stored_title = || -> Option<String> {
            database::open_or_initialize(&archive_path)
                .unwrap()
                .query_row(
                    "SELECT title FROM archived_entries WHERE entry_uid = ?1",
                    [&entry.entry_uid],
                    |row| row.get(0),
                )
                .unwrap()
        };
        let (status, body) = run().await;
        let first_stored = stored_title();
        // Instance setting beats env/default: the fake CLI sees --model custom-title-1.
        let patch = app(registry.clone(), auth_path.clone())
            .oneshot(patch_settings_request(
                serde_json::json!({ "title_model_claude_cli": " custom-title-1 " }),
                &session_cookie,
            ))
            .await
            .unwrap();
        assert_eq!(patch.status(), StatusCode::NO_CONTENT);
        let (instance_status, instance_body) = run().await;
        unsafe {
            match previous_cli {
                Some(value) => std::env::set_var("ARCHIVR_CLAUDE_CLI", value),
                None => std::env::remove_var("ARCHIVR_CLAUDE_CLI"),
            }
            if let Some(value) = previous_model {
                std::env::set_var("ARCHIVR_CLAUDE_TITLE_MODEL", value);
            }
        }

        let expected = "Thread about Rust async runtimes compared — @alice";
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["entry_uid"], entry.entry_uid.as_str());
        assert_eq!(body["title"], expected);
        assert_eq!(first_stored.as_deref(), Some(expected));
        let instance_expected = "Thread about Instance model topic — @alice";
        assert_eq!(instance_status, StatusCode::OK, "{instance_body}");
        assert_eq!(instance_body["title"], instance_expected);
        assert_eq!(stored_title().as_deref(), Some(instance_expected));
    }

    #[tokio::test]
    async fn instance_settings_title_models_validate_and_report_source() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let owner = make_test_session(&auth_path);
        let patch = |body: serde_json::Value| {
            app(registry.clone(), auth_path.clone()).oneshot(patch_settings_request(body, &owner))
        };
        let long = "m".repeat(101);
        for bad in ["has space", "tab\there", "ctl\u{7}x", long.as_str()] {
            let resp = patch(serde_json::json!({ "title_model_codex_cli": bad })).await.unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{bad:?}");
        }
        let resp = patch(serde_json::json!({ "title_model_anthropic_http": "  claude-x-1  " }))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let json = get_settings_json(registry.clone(), auth_path.clone(), &owner).await;
        assert_eq!(json["title_model_anthropic_http"], "claude-x-1");
        assert!(json["title_model_codex_cli"].is_null());
        let anthropic = &json["title_models"]["anthropic_http"];
        assert_eq!(anthropic["model"], "claude-x-1");
        assert_eq!(anthropic["source"], "instance");
        assert_ne!(anthropic["fallback_source"], "instance");
        assert_eq!(anthropic["env_var"], "ARCHIVR_ANTHROPIC_TITLE_MODEL");
        // Empty clears back to NULL.
        let resp = patch(serde_json::json!({ "title_model_anthropic_http": "   " })).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let json = get_settings_json(registry, auth_path, &owner).await;
        assert!(json["title_model_anthropic_http"].is_null());
        assert_ne!(json["title_models"]["anthropic_http"]["source"], "instance");
    }

    fn reorder_request(parent_uid: &str, uids: &[&str], cookie: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .method("PUT")
            .uri(format!("/api/archives/test/entries/{parent_uid}/children/order"))
            .header("content-type", "application/json");
        if let Some(cookie) = cookie {
            builder = builder.header("cookie", cookie);
        }
        builder
            .body(Body::from(serde_json::json!({ "child_uids": uids }).to_string()))
            .unwrap()
    }

    async fn child_uids_via_api(
        registry: ServerRegistry,
        auth_path: std::path::PathBuf,
        cookie: &str,
        parent_uid: &str,
    ) -> Vec<String> {
        let resp = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri(format!("/api/archives/test/entries/{parent_uid}/children"))
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        body_json(resp)
            .await
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["entry_uid"].as_str().unwrap().to_string())
            .collect()
    }

    #[tokio::test]
    async fn reorder_entry_children_requires_auth() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let parent = make_test_entry(&archive_path);
        let a = make_test_child(&archive_path, parent.id, "A", "https://example.com/a");
        let resp = app(registry, auth_path)
            .oneshot(reorder_request(&parent.entry_uid, &[&a.entry_uid], None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // owner is allowed by the default mask
    #[tokio::test]
    async fn reorder_entry_children_persists_order() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let cookie = make_test_session(&auth_path);
        let parent = make_test_entry(&archive_path);
        let a = make_test_child(&archive_path, parent.id, "A", "https://example.com/a");
        let b = make_test_child(&archive_path, parent.id, "B", "https://example.com/b");
        let c = make_test_child(&archive_path, parent.id, "C", "https://example.com/c");
        let order = [c.entry_uid.as_str(), a.entry_uid.as_str(), b.entry_uid.as_str()];
        let resp = app(registry.clone(), auth_path.clone())
            .oneshot(reorder_request(&parent.entry_uid, &order, Some(&cookie)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            child_uids_via_api(registry, auth_path, &cookie, &parent.entry_uid).await,
            order
        );
    }

    #[tokio::test]
    async fn reorder_entry_children_rejects_mismatched_set() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let cookie = make_test_session(&auth_path);
        let parent = make_test_entry(&archive_path);
        let a = make_test_child(&archive_path, parent.id, "A", "https://example.com/a");
        let b = make_test_child(&archive_path, parent.id, "B", "https://example.com/b");
        let other = make_test_entry(&archive_path);
        let (a_u, b_u) = (a.entry_uid.as_str(), b.entry_uid.as_str());
        let bad: [&[&str]; 3] = [&[a_u], &[a_u, b_u, other.entry_uid.as_str()], &[a_u, a_u]];
        for uids in bad {
            let resp = app(registry.clone(), auth_path.clone())
                .oneshot(reorder_request(&parent.entry_uid, uids, Some(&cookie)))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{uids:?}");
        }
        assert_eq!(
            child_uids_via_api(registry, auth_path, &cookie, &parent.entry_uid).await,
            [a_u, b_u]
        );
    }

    #[tokio::test]
    async fn reorder_entry_children_returns_404_for_unknown_parent() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let cookie = make_test_session(&auth_path);
        let resp = app(registry, auth_path)
            .oneshot(reorder_request("no-such-uid", &[], Some(&cookie)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn reorder_entry_children_denies_plain_user_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let owner = make_test_session(&auth_path);
        let plain = make_role_session(&auth_path, "plain", &[]);
        let parent = make_test_entry(&archive_path);
        let a = make_test_child(&archive_path, parent.id, "A", "https://example.com/a");
        let b = make_test_child(&archive_path, parent.id, "B", "https://example.com/b");
        let (a_u, b_u) = (a.entry_uid.as_str(), b.entry_uid.as_str());
        let resp = app(registry.clone(), auth_path.clone())
            .oneshot(reorder_request(&parent.entry_uid, &[b_u, a_u], Some(&plain)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            child_uids_via_api(registry, auth_path, &owner, &parent.entry_uid).await,
            [a_u, b_u]
        );
    }

    #[tokio::test]
    async fn reorder_entry_children_allows_admin_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let admin = make_role_session(&auth_path, "adm", &["admin"]);
        let parent = make_test_entry(&archive_path);
        let a = make_test_child(&archive_path, parent.id, "A", "https://example.com/a");
        let b = make_test_child(&archive_path, parent.id, "B", "https://example.com/b");
        let (a_u, b_u) = (a.entry_uid.as_str(), b.entry_uid.as_str());
        let resp = app(registry.clone(), auth_path.clone())
            .oneshot(reorder_request(&parent.entry_uid, &[b_u, a_u], Some(&admin)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            child_uids_via_api(registry, auth_path, &admin, &parent.entry_uid).await,
            [b_u, a_u]
        );
    }

    #[tokio::test]
    async fn reorder_entry_children_allows_custom_role_after_owner_grants_it() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let owner = make_test_session(&auth_path);
        {
            let conn = archivr_core::database::open_auth_db(&auth_path).unwrap();
            let role = database::create_custom_role(&conn, "editor", "Editor").unwrap();
            assert_eq!(role.bit_position, 4);
        }
        let editor = make_role_session(&auth_path, "ed", &["editor"]);
        let parent = make_test_entry(&archive_path);
        let a = make_test_child(&archive_path, parent.id, "A", "https://example.com/a");
        let b = make_test_child(&archive_path, parent.id, "B", "https://example.com/b");
        let order = [b.entry_uid.as_str(), a.entry_uid.as_str()];
        let resp = app(registry.clone(), auth_path.clone())
            .oneshot(reorder_request(&parent.entry_uid, &order, Some(&editor)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let resp = app(registry.clone(), auth_path.clone())
            .oneshot(patch_settings_request(
                serde_json::json!({ "reorder_children_role_bits": 4 | 8 | 16 }),
                &owner,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let resp = app(registry, auth_path)
            .oneshot(reorder_request(&parent.entry_uid, &order, Some(&editor)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn reorder_entry_children_hides_parent_invisible_to_granted_role() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let owner = make_test_session(&auth_path);
        let user = make_role_session(&auth_path, "plain", &["user"]);
        let parent = make_test_entry(&archive_path);
        let a = make_test_child(&archive_path, parent.id, "A", "https://example.com/a");
        let b = make_test_child(&archive_path, parent.id, "B", "https://example.com/b");
        // Admin/owner-only membership: the USER role cannot see the parent or children.
        database::open_or_initialize(&archive_path)
            .unwrap()
            .execute("UPDATE collection_entries SET visibility_bits = 12", [])
            .unwrap();
        let resp = app(registry.clone(), auth_path.clone())
            .oneshot(patch_settings_request(
                serde_json::json!({ "reorder_children_role_bits": 2 | 4 | 8 }),
                &owner,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let order = [b.entry_uid.as_str(), a.entry_uid.as_str()];
        let resp = app(registry.clone(), auth_path.clone())
            .oneshot(reorder_request(&parent.entry_uid, &order, Some(&user)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "granted but cannot see the parent");
        let resp = app(registry, auth_path)
            .oneshot(reorder_request(&parent.entry_uid, &order, Some(&owner)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT, "owner sees every entry");
    }

    #[tokio::test]
    async fn reorder_entry_children_empty_mask_denies_everyone() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let owner = make_test_session(&auth_path);
        let parent = make_test_entry(&archive_path);
        let a = make_test_child(&archive_path, parent.id, "A", "https://example.com/a");
        let resp = app(registry.clone(), auth_path.clone())
            .oneshot(patch_settings_request(
                serde_json::json!({ "reorder_children_role_bits": 0 }),
                &owner,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let resp = app(registry, auth_path)
            .oneshot(reorder_request(&parent.entry_uid, &[&a.entry_uid], Some(&owner)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn instance_settings_reorder_mask_requires_owner_to_change() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let owner = make_test_session(&auth_path);
        let admin = make_role_session(&auth_path, "adm", &["admin"]);
        let resp = app(registry.clone(), auth_path.clone())
            .oneshot(patch_settings_request(
                serde_json::json!({ "reorder_children_role_bits": 2 }),
                &admin,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let json = get_settings_json(registry.clone(), auth_path.clone(), &admin).await;
        assert_eq!(json["reorder_children_role_bits"], 12);
        let resp = app(registry.clone(), auth_path.clone())
            .oneshot(patch_settings_request(
                serde_json::json!({ "reorder_children_role_bits": 12, "open_registration_enabled": true }),
                &admin,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let json = get_settings_json(registry.clone(), auth_path.clone(), &admin).await;
        assert_eq!(json["open_registration_enabled"], true);
        let resp = app(registry.clone(), auth_path.clone())
            .oneshot(patch_settings_request(
                serde_json::json!({ "reorder_children_role_bits": 14 }),
                &owner,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let json = get_settings_json(registry, auth_path, &owner).await;
        assert_eq!(json["reorder_children_role_bits"], 14);
    }

    #[tokio::test]
    async fn instance_settings_reorder_mask_rejects_invalid_bits() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let owner = make_test_session(&auth_path);
        for mask in [16u32, 13, 1u32 << 31] {
            let resp = app(registry.clone(), auth_path.clone())
                .oneshot(patch_settings_request(
                    serde_json::json!({ "reorder_children_role_bits": mask }),
                    &owner,
                ))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{mask}");
        }
        let json = get_settings_json(registry, auth_path, &owner).await;
        assert_eq!(json["reorder_children_role_bits"], 12);
    }

    #[tokio::test]
    async fn auth_me_reports_can_reorder_children() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let owner = make_test_session(&auth_path);
        let plain = make_role_session(&auth_path, "plain", &[]);
        let me = me_json(registry.clone(), auth_path.clone(), &owner).await;
        assert_eq!(me["can_reorder_children"], true);
        let me = me_json(registry.clone(), auth_path.clone(), &plain).await;
        assert_eq!(me["can_reorder_children"], false);
        let resp = app(registry.clone(), auth_path.clone())
            .oneshot(patch_settings_request(
                serde_json::json!({ "reorder_children_role_bits": 14 }),
                &owner,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let me = me_json(registry, auth_path, &plain).await;
        assert_eq!(me["can_reorder_children"], true);
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
            media_tokens: Arc::new(Mutex::new(HashMap::new())),
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

    // ── Media token tests ────────────────────────────────────────────────────

    // Helper: build a minimal archive + auth setup and return (state, entry_uid, session_cookie).
    async fn make_media_token_state(
        dir: &tempfile::TempDir,
    ) -> (AppState, String, std::path::PathBuf, String) {
        let store_path = dir.path().join("store");
        let paths =
            archivr_core::archive::initialize_archive(dir.path(), &store_path, "test", false)
                .unwrap();
        // Write artifact file.
        let artifact_relpath = "raw/m/e/video.mp4";
        let artifact_dir = store_path.join("raw").join("m").join("e");
        std::fs::create_dir_all(&artifact_dir).unwrap();
        std::fs::write(artifact_dir.join("video.mp4"), b"fakevideo").unwrap();
        // Populate DB.
        let conn = database::open_or_initialize(&paths.archive_path).unwrap();
        let user_id = database::ensure_default_user(&conn).unwrap();
        let sid = database::upsert_source_identity(
            &conn,
            "yt",
            "video",
            Some("media-token-test"),
            Some("https://yt.example/v"),
            "https://yt.example/v",
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
                source_kind: "yt".to_string(),
                entity_kind: "video".to_string(),
                title: Some("Test Video".to_string()),
                visibility: "private".to_string(),
                representation_kind: "video".to_string(),
                source_metadata_json: "{}".to_string(),
                display_metadata_json: None,
            },
        )
        .unwrap();
        let blob_id = database::upsert_blob(
            &conn,
            &database::BlobRecord {
                sha256: "bbbb2222cccc3333dddd4444aaaa1111bbbb2222cccc3333dddd4444aaaa1111"
                    .to_string(),
                byte_size: 9,
                mime_type: Some("video/mp4".to_string()),
                extension: Some("mp4".to_string()),
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
        let state = AppState {
            registry: Arc::new(registry),
            auth_db_path: Arc::new(auth_path),
            login_attempts: Arc::new(Mutex::new(HashMap::new())),
            media_tokens: Arc::new(Mutex::new(HashMap::new())),
        };
        (state, entry.entry_uid, paths.archive_path, session_cookie)
    }

    /// Bare artifact URL (no token, no session) must still return 401.
    #[tokio::test]
    async fn media_token_bare_artifact_without_auth_returns_401() {
        let dir = tempfile::tempdir().unwrap();
        let (state, entry_uid, _, _) = make_media_token_state(&dir).await;
        let uri = format!("/api/archives/test/entries/{}/artifacts/0", entry_uid);
        let response = app_with_state(state)
            .oneshot(Request::builder().uri(&uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// Authenticated POST to media-token, then unauthenticated GET with token → 200.
    #[tokio::test]
    async fn media_token_tokenized_artifact_succeeds_unauthenticated() {
        let dir = tempfile::tempdir().unwrap();
        let (state, entry_uid, _, session_cookie) = make_media_token_state(&dir).await;
        // Issue token (authenticated).
        let token_uri = format!(
            "/api/archives/test/entries/{}/artifacts/0/media-token",
            entry_uid
        );
        let token_resp = app_with_state(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(&token_uri)
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(token_resp.status(), StatusCode::OK);
        let body = body_json(token_resp).await;
        let signed_url = body["url"].as_str().expect("url field missing");
        assert!(body["expires_in_secs"].as_u64().unwrap() > 0);
        // Fetch artifact with signed URL — no session cookie.
        let artifact_resp = app_with_state(state)
            .oneshot(
                Request::builder()
                    .uri(signed_url)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(artifact_resp.status(), StatusCode::OK);
    }

    /// A bogus token with NO session must return 401 (no valid auth path).
    #[tokio::test]
    async fn media_token_invalid_token_returns_401() {
        let dir = tempfile::tempdir().unwrap();
        let (state, entry_uid, _, _) = make_media_token_state(&dir).await;
        let uri = format!(
            "/api/archives/test/entries/{}/artifacts/0?token=not-a-real-token",
            entry_uid
        );
        let response = app_with_state(state)
            .oneshot(Request::builder().uri(&uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// A bogus token WITH a valid session must return 200 — the session fallback
    /// keeps a logged-in browser player working after a token expires.
    #[tokio::test]
    async fn media_token_bogus_token_with_session_returns_200() {
        let dir = tempfile::tempdir().unwrap();
        let (state, entry_uid, _, session_cookie) = make_media_token_state(&dir).await;
        let uri = format!(
            "/api/archives/test/entries/{}/artifacts/0?token=not-a-real-token",
            entry_uid
        );
        let response = app_with_state(state)
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

    /// A token issued for artifact 0 must not unlock artifact 1.
    #[tokio::test]
    async fn media_token_wrong_artifact_index_returns_401() {
        let dir = tempfile::tempdir().unwrap();
        let (state, entry_uid, _, session_cookie) = make_media_token_state(&dir).await;
        // Issue token for artifact 0.
        let token_uri = format!(
            "/api/archives/test/entries/{}/artifacts/0/media-token",
            entry_uid
        );
        let token_resp = app_with_state(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(&token_uri)
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(token_resp.status(), StatusCode::OK);
        let body = body_json(token_resp).await;
        let token = body["url"]
            .as_str()
            .unwrap()
            .split("token=")
            .nth(1)
            .unwrap();
        // Try to use it for artifact 1.
        let wrong_uri = format!(
            "/api/archives/test/entries/{}/artifacts/1?token={}",
            entry_uid, token
        );
        let response = app_with_state(state)
            .oneshot(
                Request::builder()
                    .uri(&wrong_uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    // ── Public preview contract tests ──────────────────────────────────────────
    // entry_detail and serve_artifact are open to guests iff the entry lives in a
    // public collection (requires_auth=false) with visibility_bits & ROLE_GUEST (1).
    // list_entry_children is open to guests iff the parent passes the same check.
    // Children inherit public accessibility from their parent.

    /// Build a collection via API; return its uid.
    async fn api_make_collection(
        registry: ServerRegistry, auth_path: std::path::PathBuf,
        session: &str, name: &str, slug: &str, vis: u32, requires_auth: bool,
    ) -> String {
        let r = app(registry, auth_path)
            .oneshot(Request::builder().method("POST")
                .uri("/api/archives/test/collections")
                .header("content-type", "application/json").header("cookie", session)
                .body(json_body(&serde_json::json!({
                    "name": name, "slug": slug,
                    "default_visibility_bits": vis, "requires_auth": requires_auth
                }))).unwrap()).await.unwrap();
        assert_eq!(r.status(), StatusCode::CREATED);
        body_json(r).await["collection_uid"].as_str().unwrap().to_string()
    }

    /// Add an entry to a collection via API.
    async fn api_add_to_coll(
        registry: ServerRegistry, auth_path: std::path::PathBuf,
        session: &str, coll_uid: &str, entry_uid: &str, vis: u32,
    ) {
        let r = app(registry, auth_path)
            .oneshot(Request::builder().method("POST")
                .uri(format!("/api/archives/test/collections/{coll_uid}/entries"))
                .header("content-type", "application/json").header("cookie", session)
                .body(json_body(&serde_json::json!({
                    "entry_uid": entry_uid, "visibility_bits": vis
                }))).unwrap()).await.unwrap();
        assert_eq!(r.status(), StatusCode::NO_CONTENT);
    }

    /// Create an entry with one artifact file; returns (entry, artifact uri component).
    fn make_entry_with_artifact(
        archive_path: &std::path::Path,
        store_path: &std::path::Path,
    ) -> archivr_core::database::ArchivedEntry {
        let conn = database::open_or_initialize(archive_path).unwrap();
        let user_id = database::ensure_default_user(&conn).unwrap();
        let run = database::create_archive_run(&conn, user_id, 1).unwrap();
        let si = database::upsert_source_identity(
            &conn, "web", "page", None,
            Some("https://example.com/artitest"), "https://example.com/artitest",
        ).unwrap();
        let entry = database::create_archived_entry(&conn, &database::NewEntry {
            source_identity_id: si, archive_run_id: run.id,
            parent_entry_id: None, root_entry_id: None,
            created_by_user_id: user_id, owned_by_user_id: user_id,
            source_kind: "web".to_string(), entity_kind: "page".to_string(),
            title: Some("Artifact Test".to_string()), visibility: "private".to_string(),
            representation_kind: "html".to_string(),
            source_metadata_json: "{}".to_string(), display_metadata_json: None,
        }).unwrap();
        let relpath = "raw/pp/qq/test.html";
        let file_dir = store_path.join("raw").join("pp").join("qq");
        std::fs::create_dir_all(&file_dir).unwrap();
        std::fs::write(file_dir.join("test.html"), b"<html>pub</html>").unwrap();
        let blob_id = database::upsert_blob(&conn, &database::BlobRecord {
            sha256: "cccc3333dddd4444eeee5555ffff6666cccc3333dddd4444eeee5555ffff6666".to_string(),
            byte_size: 16, mime_type: Some("text/html".to_string()),
            extension: Some("html".to_string()), raw_relpath: relpath.to_string(),
        }).unwrap();
        database::add_entry_artifact(&conn, &database::NewArtifact {
            entry_id: entry.id, artifact_role: "primary_media".to_string(),
            storage_area: "raw".to_string(), relpath: relpath.to_string(),
            blob_id: Some(blob_id), logical_path: None, metadata_json: None,
        }).unwrap();
        entry
    }

    #[tokio::test]
    async fn guest_entry_detail_public_entry_succeeds() {
        // Entry in a requires_auth=false collection with vis=3 (ROLE_GUEST) → 200 without cookie.
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let session = make_test_session(&auth_path);
        let entry = make_test_entry(&archive_path);
        let coll = api_make_collection(
            registry.clone(), auth_path.clone(), &session,
            "PubD", "pub-d", 3, false,
        ).await;
        api_add_to_coll(registry.clone(), auth_path.clone(), &session, &coll, &entry.entry_uid, 3).await;
        let resp = app(registry, auth_path)
            .oneshot(Request::builder()
                .uri(format!("/api/archives/test/entries/{}", entry.entry_uid))
                .body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn guest_entry_detail_users_only_entry_blocked() {
        // Entry in requires_auth=false collection but visibility_bits=2 (no ROLE_GUEST) → 401.
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let session = make_test_session(&auth_path);
        let entry = make_test_entry(&archive_path);
        let coll = api_make_collection(
            registry.clone(), auth_path.clone(), &session,
            "SemiPub", "semi-pub", 3, false,
        ).await;
        // Add with visibility_bits=2: users-only, ROLE_GUEST bit not set.
        api_add_to_coll(registry.clone(), auth_path.clone(), &session, &coll, &entry.entry_uid, 2).await;
        let resp = app(registry, auth_path)
            .oneshot(Request::builder()
                .uri(format!("/api/archives/test/entries/{}", entry.entry_uid))
                .body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn guest_entry_detail_auth_required_collection_blocked() {
        // Entry in requires_auth=true collection even with guest visibility bits → 401.
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let session = make_test_session(&auth_path);
        let entry = make_test_entry(&archive_path);
        let coll = api_make_collection(
            registry.clone(), auth_path.clone(), &session,
            "AuthColl", "auth-coll", 3, true,
        ).await;
        api_add_to_coll(registry.clone(), auth_path.clone(), &session, &coll, &entry.entry_uid, 3).await;
        let resp = app(registry, auth_path)
            .oneshot(Request::builder()
                .uri(format!("/api/archives/test/entries/{}", entry.entry_uid))
                .body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn guest_serve_artifact_public_entry_succeeds() {
        // Artifact of an entry in a public collection is accessible without cookie.
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().join("store");
        let paths = archivr_core::archive::initialize_archive(
            dir.path(), &store_path, "test", false,
        ).unwrap();
        let auth_path = dir.path().join("auth.sqlite");
        {
            let conn = archivr_core::database::open_auth_db(&auth_path).unwrap();
            archivr_core::database::create_owner(&conn, "testowner", "dummy").unwrap();
        }
        let registry = ServerRegistry {
            archives: vec![MountedArchive {
                id: "test".to_string(), label: "Test".to_string(),
                archive_path: paths.archive_path.clone(),
            }],
            bind: None, auth_db_path: None,
        };
        let session = make_test_session(&auth_path);
        let entry = make_entry_with_artifact(&paths.archive_path, &store_path);
        let coll = api_make_collection(
            registry.clone(), auth_path.clone(), &session,
            "PubArt", "pub-art", 3, false,
        ).await;
        api_add_to_coll(registry.clone(), auth_path.clone(), &session, &coll, &entry.entry_uid, 3).await;
        let resp = app(registry, auth_path)
            .oneshot(Request::builder()
                .uri(format!("/api/archives/test/entries/{}/artifacts/0", entry.entry_uid))
                .body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn guest_serve_artifact_users_only_entry_blocked() {
        // Artifact of a users-only entry is blocked for guests even in a public collection.
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().join("store");
        let paths = archivr_core::archive::initialize_archive(
            dir.path(), &store_path, "test", false,
        ).unwrap();
        let auth_path = dir.path().join("auth.sqlite");
        {
            let conn = archivr_core::database::open_auth_db(&auth_path).unwrap();
            archivr_core::database::create_owner(&conn, "testowner", "dummy").unwrap();
        }
        let registry = ServerRegistry {
            archives: vec![MountedArchive {
                id: "test".to_string(), label: "Test".to_string(),
                archive_path: paths.archive_path.clone(),
            }],
            bind: None, auth_db_path: None,
        };
        let session = make_test_session(&auth_path);
        let entry = make_entry_with_artifact(&paths.archive_path, &store_path);
        let coll = api_make_collection(
            registry.clone(), auth_path.clone(), &session,
            "PubArt2", "pub-art2", 3, false,
        ).await;
        // visibility_bits=2: users-only, guest cannot see.
        api_add_to_coll(registry.clone(), auth_path.clone(), &session, &coll, &entry.entry_uid, 2).await;
        let resp = app(registry, auth_path)
            .oneshot(Request::builder()
                .uri(format!("/api/archives/test/entries/{}/artifacts/0", entry.entry_uid))
                .body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn guest_list_children_public_parent_succeeds() {
        // Children of a public parent are accessible to guests.
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let session = make_test_session(&auth_path);
        // Create parent entry.
        let parent = make_test_entry(&archive_path);
        // Create child entry referencing parent.
        let child = make_test_child(&archive_path, parent.id, "Child Entry", "https://example.com/child");
        // Put parent in a public collection with guest visibility.
        let coll = api_make_collection(
            registry.clone(), auth_path.clone(), &session, "PubParent", "pub-parent", 3, false,
        ).await;
        api_add_to_coll(registry.clone(), auth_path.clone(), &session, &coll, &parent.entry_uid, 3).await;
        // Guest requests children of the public parent — must see the child, not just 200.
        let resp = app(registry, auth_path)
            .oneshot(Request::builder()
                .uri(format!("/api/archives/test/entries/{}/children", parent.entry_uid))
                .body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        let uids: Vec<&str> = body.as_array().unwrap()
            .iter().map(|e| e["entry_uid"].as_str().unwrap()).collect();
        assert!(uids.contains(&child.entry_uid.as_str()),
            "guest must see child uid in children response; got {:?}", uids);
    }

    #[tokio::test]
    async fn guest_list_children_private_parent_blocked() {
        // Children of a non-public parent are blocked for guests.
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        // Parent only in _default_ (requires_auth=true by default); not in any public collection.
        let parent = make_test_entry(&archive_path);
        let resp = app(registry, auth_path)
            .oneshot(Request::builder()
                .uri(format!("/api/archives/test/entries/{}/children", parent.entry_uid))
                .body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn guest_child_entry_detail_via_public_parent_succeeds() {
        // A child entry inherits public accessibility from its parent's collection membership.
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let session = make_test_session(&auth_path);
        let parent = make_test_entry(&archive_path);
        let child = {
            let conn = database::open_or_initialize(&archive_path).unwrap();
            let user_id = database::ensure_default_user(&conn).unwrap();
            let run = database::create_archive_run(&conn, user_id, 1).unwrap();
            let si = database::upsert_source_identity(
                &conn, "web", "page", None,
                Some("https://example.com/child2"), "https://example.com/child2",
            ).unwrap();
            database::create_archived_entry(&conn, &database::NewEntry {
                source_identity_id: si, archive_run_id: run.id,
                parent_entry_id: Some(parent.id), root_entry_id: Some(parent.id),
                created_by_user_id: user_id, owned_by_user_id: user_id,
                source_kind: "web".to_string(), entity_kind: "page".to_string(),
                title: Some("Child Detail Test".to_string()), visibility: "private".to_string(),
                representation_kind: "html".to_string(),
                source_metadata_json: "{}".to_string(), display_metadata_json: None,
            }).unwrap()
        };
        // Parent in a public collection with guest visibility.
        let coll = api_make_collection(
            registry.clone(), auth_path.clone(), &session, "PubParent2", "pub-parent2", 3, false,
        ).await;
        api_add_to_coll(registry.clone(), auth_path.clone(), &session, &coll, &parent.entry_uid, 3).await;
        // Child detail accessible to guest via parent's public membership.
        let resp = app(registry, auth_path)
            .oneshot(Request::builder()
                .uri(format!("/api/archives/test/entries/{}", child.entry_uid))
                .body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn guest_list_collections_returns_only_public() {
        // GET /api/archives/:id/collections for a guest must omit auth-required collections
        // so their names are never leaked to unsigned users.
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let session = make_test_session(&auth_path);
        // Create one public and one auth-required collection.
        let _ = api_make_collection(
            registry.clone(), auth_path.clone(), &session,
            "PublicColl", "public-coll", 3, false,
        ).await;
        let auth_coll_name = "SecretColl";
        let _ = api_make_collection(
            registry.clone(), auth_path.clone(), &session,
            auth_coll_name, "secret-coll", 2, true,
        ).await;
        // Guest fetches the collection list.
        let resp = app(registry, auth_path)
            .oneshot(Request::builder()
                .uri("/api/archives/test/collections")
                .body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        let names: Vec<&str> = body.as_array().unwrap()
            .iter().map(|c| c["name"].as_str().unwrap()).collect();
        assert!(!names.contains(&auth_coll_name),
            "auth-required collection name must not be returned to guests; got {:?}", names);
        assert!(names.contains(&"PublicColl"),
            "public collection must be returned to guests; got {:?}", names);
    }

    // ── Local transcription fallback ───────────────────────────────────────

    /// Serializes tests that set transcription env vars (std Mutex: the
    /// workspace tokio features don't include `sync`).
    static TRANSCRIBE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    const TRANSCRIBE_ENV_VARS: [&str; 13] = [
        "ARCHIVR_TRANSCRIBE_ENGINES",
        "ARCHIVR_WHISPER_CLI",
        "ARCHIVR_WHISPER_MODEL",
        "ARCHIVR_WHISPER_BACKEND",
        "ARCHIVR_WHISPER_LANGUAGES",
        "ARCHIVR_PARAKEET_CLI",
        "ARCHIVR_PARAKEET_MODEL",
        "ARCHIVR_PARAKEET_LANGUAGES",
        "ARCHIVR_PHONON2_CLI",
        "ARCHIVR_PHONON2_MODEL",
        "ARCHIVR_TRANSCRIBE_TIMEOUT",
        "ARCHIVR_FFMPEG",
        "ARCHIVR_CODEX_CLI",
    ];

    /// Holds the lock, clears every transcription var, sets `vars`, and
    /// restores the previous values on drop.
    struct TranscribeEnv {
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for TranscribeEnv {
        fn drop(&mut self) {
            for (key, value) in &self.saved {
                match value {
                    Some(v) => unsafe { std::env::set_var(key, v) },
                    None => unsafe { std::env::remove_var(key) },
                }
            }
        }
    }

    fn transcribe_env(vars: &[(&str, &str)]) -> TranscribeEnv {
        let guard = TRANSCRIBE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = TRANSCRIBE_ENV_VARS
            .iter()
            .map(|k| (*k, std::env::var_os(k)))
            .collect();
        for k in TRANSCRIBE_ENV_VARS {
            unsafe { std::env::remove_var(k) };
        }
        for (k, v) in vars {
            unsafe { std::env::set_var(k, v) };
        }
        TranscribeEnv {
            saved,
            _guard: guard,
        }
    }

    /// Writes an executable `#!/bin/sh` stub, then waits until it can be exec'd
    /// (same ETXTBSY retry as core's `downloader::write_script`: a child forked by
    /// a parallel test may still hold our write fd, rust-lang/rust#114554). A guard
    /// line after the shebang makes the `--version` probe a no-op so stub bodies
    /// don't write files during it.
    #[cfg(unix)]
    fn write_transcribe_stub(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        let (shebang, rest) = body.split_once('\n').unwrap_or((body, ""));
        std::fs::write(&path, format!("{shebang}\n[ \"$1\" = --version ] && exit 0\n{rest}")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        for _ in 0..200 {
            match std::process::Command::new(&path).arg("--version").output() {
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                _ => return path,
            }
        }
        panic!("{} stayed busy (ETXTBSY)", path.display());
    }

    async fn post_summary_body(
        registry: ServerRegistry,
        auth_path: std::path::PathBuf,
        entry_uid: &str,
        payload: serde_json::Value,
    ) -> axum::response::Response {
        let session_cookie = make_test_session(&auth_path);
        app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/archives/test/entries/{entry_uid}/summary"))
                    .header("content-type", "application/json")
                    .header("cookie", &session_cookie)
                    .body(json_body(&payload))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn transcription_engines_endpoint_requires_user() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _archive_path, auth_path) = make_test_registry(&dir);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/summary/transcription-engines")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn transcription_engines_endpoint_lists_enabled_engines() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _archive_path, auth_path) = make_test_registry(&dir);
        let session_cookie = make_test_session(&auth_path);
        let _env = transcribe_env(&[
            ("ARCHIVR_TRANSCRIBE_ENGINES", "phonon2"),
            ("ARCHIVR_PHONON2_CLI", "/usr/bin/false"),
        ]);
        let response = app(registry, auth_path)
            .oneshot(
                Request::builder()
                    .uri("/api/summary/transcription-engines")
                    .header("cookie", &session_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(
            body,
            serde_json::json!([{
                "kind": "phonon2",
                "label": "Phonon-2",
                "english_only": true,
                "languages": ["en"],
            }])
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn summary_post_rejects_unconfigured_transcribe_engine_with_400() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let entry = make_test_youtube_entry(&archive_path, "youtube-test:offline");
        let _env = transcribe_env(&[
            ("ARCHIVR_TRANSCRIBE_ENGINES", "whisper"),
            ("ARCHIVR_CODEX_CLI", "/usr/bin/false"),
        ]);
        let response = post_summary_body(
            registry,
            auth_path,
            &entry.entry_uid,
            serde_json::json!({ "provider": "codex_cli", "transcribe_engine": "whisper" }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_json(response).await.to_string();
        assert!(body.contains("ARCHIVR_WHISPER_MODEL"), "{body}");
        let conn = database::open_or_initialize(&archive_path).unwrap();
        assert!(database::latest_entry_summary_attempt(&conn, entry.id)
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn summary_post_rejects_engine_not_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        let entry = make_test_youtube_entry(&archive_path, "youtube-test:offline");
        let _env = transcribe_env(&[
            ("ARCHIVR_PHONON2_CLI", "/usr/bin/false"),
            ("ARCHIVR_CODEX_CLI", "/usr/bin/false"),
        ]);
        let response = post_summary_body(
            registry,
            auth_path,
            &entry.entry_uid,
            serde_json::json!({ "provider": "codex_cli", "transcribe_engine": "phonon2" }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_json(response).await.to_string();
        assert!(body.contains("ARCHIVR_TRANSCRIBE_ENGINES"), "{body}");
        let conn = database::open_or_initialize(&archive_path).unwrap();
        assert!(database::latest_entry_summary_attempt(&conn, entry.id)
            .unwrap()
            .is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn youtube_summary_with_stub_transcription_reaches_provider() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, archive_path, auth_path) = make_test_registry(&dir);
        // Non-HTTP canonical URL: the subtitle fetch never spawns yt-dlp.
        let entry = make_test_youtube_entry(&archive_path, "youtube-test:offline");
        add_summary_test_artifact(
            &archive_path,
            entry.id,
            "raw/youtube-video.mp4",
            "primary_media",
            "video/mp4",
            b"video fixture",
        );
        let bin = tempfile::tempdir().unwrap();
        let ffmpeg = write_transcribe_stub(
            bin.path(),
            "ffmpeg",
            "#!/bin/sh\nfor a; do last=\"$a\"; done\nhead -c 3244 /dev/zero > \"$last\"\n",
        );
        let whisper = write_transcribe_stub(
            bin.path(),
            "whisper-cli",
            "#!/bin/sh\nprefix=\"\"\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = \"-of\" ]; then prefix=\"$2\"; shift; fi\n  shift\ndone\nprintf 'WEBVTT\\n\\n00:00:00.000 --> 00:00:02.000\\nhello from the stub transcript\\n' > \"$prefix.vtt\"\nprintf '{\"result\":{\"language\":\"en\"}}' > \"$prefix.json\"\n",
        );
        let _env = transcribe_env(&[
            ("ARCHIVR_TRANSCRIBE_ENGINES", "whisper"),
            ("ARCHIVR_WHISPER_CLI", whisper.to_str().unwrap()),
            ("ARCHIVR_WHISPER_MODEL", "/tmp/x/ggml-tiny.bin"),
            ("ARCHIVR_FFMPEG", ffmpeg.to_str().unwrap()),
            ("ARCHIVR_CODEX_CLI", "/usr/bin/false"),
        ]);

        let response = post_summary_body(
            registry,
            auth_path,
            &entry.entry_uid,
            serde_json::json!({ "provider": "codex_cli", "transcribe_engine": "whisper" }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let summary_uid = body_json(response).await["summary_uid"]
            .as_str()
            .unwrap()
            .to_string();

        let mut stored = None;
        for _ in 0..200 {
            let conn = database::open_or_initialize(&archive_path).unwrap();
            let row = database::get_entry_summary_by_uid(&conn, &summary_uid)
                .unwrap()
                .unwrap();
            if row.status == "failed" {
                stored = Some(row);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let stored = stored.expect("summary row should fail at the /usr/bin/false provider");
        let error_text = stored.error_text.unwrap_or_default();
        // Neither the no-subtitles copy nor a transcription copy: transcription
        // succeeded and the (failing) provider ran.
        assert_ne!(error_text, summarizer::NO_SUBTITLES_SUMMARY_MESSAGE);
        assert_ne!(error_text, summarizer::NO_SUBTITLES_AFTER_TRANSCRIPTION_MESSAGE);
        assert!(!error_text.starts_with("Local transcription"), "{error_text}");
        assert!(!error_text.contains("can’t be transcribed"), "{error_text}");
        assert_ne!(stored.input_sha256, summarizer::SUBTITLE_FETCH_PENDING_INPUT_SHA256);

        let conn = database::open_or_initialize(&archive_path).unwrap();
        let subtitles = database::list_entry_artifacts_by_role(&conn, entry.id, "subtitle").unwrap();
        assert_eq!(subtitles.len(), 1);
        let meta: serde_json::Value =
            serde_json::from_str(subtitles[0].metadata_json.as_deref().unwrap()).unwrap();
        assert_eq!(meta["kind"], "transcribed");
        assert_eq!(meta["origin"], "transcription");
        assert_eq!(meta["engine"], "whisper");
        assert_eq!(meta["model"], "ggml-tiny.bin");
    }

    #[test]
    fn summary_failure_error_text_prefers_transcription_copy() {
        let copy = "Local transcription with Whisper failed: the transcription engine exited with an error. Check the server log for details.";
        let error = anyhow::anyhow!("whisper-cli exited with 1: /secret/path")
            .context(archivr_core::transcriber::TranscriptionUserMessage(copy.to_string()))
            .context("outer");
        assert_eq!(summary_failure_error_text(&error), copy);
    }
}
