//! Read-scope API token enforcement: Bearer requests from `scope = 'read'`
//! tokens get 403 on anything but GET/HEAD. Stub: the credentials stream
//! implements `enforce_read_scope` and wires it as a layer in `app_with_state`.
#![allow(dead_code)] // not wired yet
use axum::{
    Router,
    extract::{Request, State},
    middleware::Next,
    response::Response,
};

use crate::routes::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
}

/// Middleware stub: currently a pass-through.
pub async fn enforce_read_scope(
    State(_state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    next.run(req).await
}
