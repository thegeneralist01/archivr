//! Shared helpers for server tests (`#[cfg(test)]` only). Copied from
//! `routes.rs`'s `mod tests` so new stream modules can reuse them without
//! touching that file; the originals there are unchanged.
#![allow(dead_code)] // helpers are consumed by the stream modules' tests
use std::path::{Path, PathBuf};

use archivr_core::database;
use axum::body::Body;

use crate::auth;
use crate::registry::{MountedArchive, ServerRegistry};

/// One archive ("test") + auth DB seeded with owner `testowner`.
/// Returns `(registry, archive_path, auth_db_path)`.
pub(crate) fn make_test_registry(
    dir: &tempfile::TempDir,
) -> (ServerRegistry, PathBuf, PathBuf) {
    let paths = archivr_core::archive::initialize_archive(
        dir.path(),
        &dir.path().join("store"),
        "test",
        false,
    )
    .unwrap();
    let auth_path = dir.path().join("auth.sqlite");
    {
        let conn = database::open_auth_db(&auth_path).unwrap();
        database::create_owner(&conn, "testowner", "dummy").unwrap();
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
pub(crate) fn make_test_session(auth_path: &Path) -> String {
    let conn = database::open_auth_db(auth_path).unwrap();
    let user_id: i64 = conn
        .query_row(
            "SELECT id FROM users WHERE username = 'testowner'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let role_bits = database::compute_role_bits(&conn, user_id).unwrap();
    let sess_uid = database::create_session(&conn, user_id, role_bits, None).unwrap();
    format!("session={}", sess_uid)
}

/// Creates an active user holding `roles` (assign_role adds the cumulative ones:
/// user for any non-guest, admin for owner) and returns a session cookie.
pub(crate) fn make_role_session(auth_path: &Path, username: &str, roles: &[&str]) -> String {
    let conn = database::open_auth_db(auth_path).unwrap();
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

/// Cookie for the seeded owner `testowner`.
pub(crate) fn owner_session(auth_path: &Path) -> String {
    make_test_session(auth_path)
}

/// Creates user `test-admin` (roles: admin + cumulative user) and returns its cookie.
/// Fixed username: call at most once per auth DB (use `make_role_session` for more).
pub(crate) fn admin_session(auth_path: &Path) -> String {
    make_role_session(auth_path, "test-admin", &["admin"])
}

/// Creates user `test-user` (role: user) and returns its cookie. Once per auth DB.
pub(crate) fn user_session(auth_path: &Path) -> String {
    make_role_session(auth_path, "test-user", &["user"])
}

/// Creates user `test-guest` (role: guest only) and returns its cookie. Once per auth DB.
pub(crate) fn guest_session(auth_path: &Path) -> String {
    make_role_session(auth_path, "test-guest", &["guest"])
}

/// Creates an API token for the existing user `username` and returns the raw
/// token (send as `Authorization: Bearer <raw>`). `expires` is an RFC 3339
/// timestamp (None = never; a past value yields an already-expired token);
/// `scope` is `"full"` or `"read"`.
pub(crate) fn make_api_token(
    auth_path: &Path,
    username: &str,
    expires: Option<&str>,
    scope: &str,
) -> String {
    let conn = database::open_auth_db(auth_path).unwrap();
    let user_id: i64 = conn
        .query_row(
            "SELECT id FROM users WHERE username = ?1",
            [username],
            |r| r.get(0),
        )
        .unwrap();
    let raw = auth::generate_token();
    database::create_api_token(
        &conn,
        user_id,
        &auth::hash_token(&raw),
        "test token",
        expires,
        scope,
    )
    .unwrap();
    raw
}

pub(crate) async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

pub(crate) fn json_body(payload: &serde_json::Value) -> Body {
    Body::from(serde_json::to_vec(payload).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::app;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn session_helpers_and_bearer_token_authenticate() {
        let dir = tempfile::tempdir().unwrap();
        let (registry, _, auth_path) = make_test_registry(&dir);
        let owner = owner_session(&auth_path);
        let admin = admin_session(&auth_path);
        let user = user_session(&auth_path);
        let guest = guest_session(&auth_path);
        let token = make_api_token(&auth_path, "test-user", None, "full");

        let me = |cookie: Option<String>, bearer: Option<String>| {
            let registry = registry.clone();
            let auth_path = auth_path.clone();
            async move {
                let mut b = Request::builder().uri("/api/auth/me");
                if let Some(c) = cookie {
                    b = b.header("cookie", c);
                }
                if let Some(t) = bearer {
                    b = b.header("authorization", format!("Bearer {t}"));
                }
                let resp = app(registry, auth_path)
                    .oneshot(b.body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                let status = resp.status();
                (status, body_json(resp).await)
            }
        };
        for (cookie, role_bit) in [(owner, 8u64), (admin, 4), (user, 2), (guest, 1)] {
            let (status, body) = me(Some(cookie), None).await;
            assert_eq!(status, StatusCode::OK);
            assert!(body["role_bits"].as_u64().unwrap() & role_bit != 0, "{body}");
        }
        let (status, body) = me(None, Some(token)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["username"], "test-user");

        // Back-dated token expiry is rejected as unauthenticated.
        let past = (chrono::Utc::now() - chrono::Duration::days(1)).to_rfc3339();
        let expired = make_api_token(&auth_path, "test-user", Some(&past), "full");
        let (status, _) = me(None, Some(expired)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    // Axum 0.7.9 `Router::merge` semantics, pinned for the stream modules.
    async fn status_of(router: axum::Router, method: &str, uri: &str) -> StatusCode {
        router
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn merge_adds_new_methods_to_an_existing_path() {
        use axum::routing::{delete, get, post};
        let base = axum::Router::new().route("/api/auth/tokens", get(|| async { "list" }).post(|| async { "create" }));
        // A merged router adding DELETE (a new method) on the same path: fine.
        let extra = axum::Router::new().route("/api/auth/tokens", delete(|| async { "revoke-all" }));
        let merged = base.merge(extra);
        assert_eq!(status_of(merged.clone(), "GET", "/api/auth/tokens").await, StatusCode::OK);
        assert_eq!(status_of(merged.clone(), "POST", "/api/auth/tokens").await, StatusCode::OK);
        assert_eq!(status_of(merged.clone(), "DELETE", "/api/auth/tokens").await, StatusCode::OK);
        assert_eq!(status_of(merged, "PUT", "/api/auth/tokens").await, StatusCode::METHOD_NOT_ALLOWED);
        // Same for routes registered by two merged routers on one new path.
        let a = axum::Router::new().route("/x", get(|| async { "a" }));
        let b = axum::Router::new().route("/x", post(|| async { "b" }));
        let merged = a.merge(b);
        assert_eq!(status_of(merged.clone(), "GET", "/x").await, StatusCode::OK);
        assert_eq!(status_of(merged, "POST", "/x").await, StatusCode::OK);
    }

    #[test]
    #[should_panic(expected = "Overlapping method route")]
    fn merge_panics_when_the_same_method_is_registered_twice() {
        use axum::routing::get;
        let base = axum::Router::<()>::new().route("/api/auth/tokens", get(|| async { "list" }));
        let extra = axum::Router::new().route("/api/auth/tokens", get(|| async { "dup" }));
        let _ = base.merge(extra);
    }
}
