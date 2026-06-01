use axum::{Router, routing::get};

use crate::registry::ServerRegistry;

pub fn app(_registry: ServerRegistry) -> Router {
    Router::new().route("/health", get(|| async { "ok" }))
}
