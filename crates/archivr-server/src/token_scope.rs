//! Read-scope API token enforcement: Bearer requests authenticated by a token
//! with `scope = 'read'` get 403 on anything but GET/HEAD/OPTIONS. Cookie
//! sessions are unaffected. Wired as a layer in `app_with_state`.
use axum::{
    extract::{Request, State},
    http::Method,
    middleware::Next,
    response::{IntoResponse, Response},
};
use axum_extra::extract::CookieJar;

use crate::auth::hash_token;
use crate::routes::{ApiError, AppState};
use archivr_core::{auth_credentials, database};

/// Rejects mutating requests authenticated by a read-scope Bearer token.
///
/// The token is only resolved for non-safe methods, so reads cost nothing
/// extra. A valid session cookie wins over a Bearer header (same precedence
/// as the `AuthUser` extractor), so cookie requests are never affected.
pub async fn enforce_read_scope(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return next.run(req).await;
    }
    let raw_token = req
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if let Some(raw_token) = raw_token
        && let Ok(conn) = database::open_auth_db(&state.auth_db_path)
    {
        let jar = CookieJar::from_headers(req.headers());
        let cookie_session_valid = jar
            .get("session")
            .map(|c| matches!(database::get_session(&conn, c.value()), Ok(Some(_))))
            .unwrap_or(false);
        if !cookie_session_valid
            && let Ok(Some(scope)) =
                auth_credentials::token_scope_for_hash(&conn, &hash_token(raw_token))
            && scope == "read"
        {
            return ApiError::forbidden("read-only token").into_response();
        }
    }
    next.run(req).await
}
