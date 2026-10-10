//! Credential management endpoints: own sessions (S1-S3) and the admin
//! password / session / API token operations on other users (U2-U5).
//! T1/T2/M1 live in `routes.rs` because they extend existing handlers.
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get, post},
};
use axum_extra::extract::CookieJar;
use rusqlite::Connection;

use crate::auth::{self, AuthUser, ROLE_ADMIN};
use crate::guards::{ensure_can_manage, ensure_not_self};
use crate::routes::{ApiError, AppState};
use archivr_core::{auth_credentials, database};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/auth/sessions",
            get(list_sessions).delete(revoke_other_sessions),
        )
        .route("/api/auth/sessions/:handle", delete(revoke_session))
        .route("/api/admin/users/:uid/password", post(admin_reset_password))
        .route("/api/admin/users/:uid/sessions", delete(admin_revoke_sessions))
        .route("/api/admin/users/:uid/tokens", get(admin_list_tokens))
        .route(
            "/api/admin/users/:uid/tokens/:token_uid",
            delete(admin_revoke_token),
        )
}

/// Value of the caller's `session` cookie, if any.
fn cookie_session_uid(jar: &CookieJar) -> Option<&str> {
    jar.get("session").map(|c| c.value())
}

async fn list_sessions(
    State(state): State<AppState>,
    auth_user: AuthUser,
    jar: CookieJar,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (user_id, _) = auth_user.require_auth()?;
    let conn = database::open_auth_db(&state.auth_db_path)?;
    let current = cookie_session_uid(&jar).map(auth_credentials::session_handle);
    let rows: Vec<serde_json::Value> = auth_credentials::list_user_sessions(&conn, user_id)?
        .into_iter()
        .map(|s| {
            let is_current = current.as_deref() == Some(s.session_handle.as_str());
            serde_json::json!({
                "session_handle": s.session_handle,
                "created_at": s.created_at,
                "last_seen_at": s.last_seen_at,
                "expires_at": s.expires_at,
                "user_agent": s.user_agent,
                "current": is_current,
            })
        })
        .collect();
    Ok(Json(serde_json::Value::Array(rows)))
}

async fn revoke_session(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(handle): Path<String>,
) -> Result<StatusCode, ApiError> {
    let (user_id, _) = auth_user.require_auth()?;
    let conn = database::open_auth_db(&state.auth_db_path)?;
    if auth_credentials::delete_user_session_by_handle(&conn, user_id, &handle)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("session not found"))
    }
}

async fn revoke_other_sessions(
    State(state): State<AppState>,
    auth_user: AuthUser,
    jar: CookieJar,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (user_id, _) = auth_user.require_auth()?;
    let conn = database::open_auth_db(&state.auth_db_path)?;
    // On Bearer requests the cookie (if any) is not one of this user's live
    // sessions, so nothing is spared.
    let revoked =
        auth_credentials::delete_other_sessions(&conn, user_id, cookie_session_uid(&jar))?;
    Ok(Json(serde_json::json!({ "revoked": revoked })))
}

/// Shared prefix of the admin target endpoints: role check, target lookup
/// (404), optional self-guard (409), can-manage (403), in the spec's order.
/// Returns the target's integer id.
fn resolve_target(
    conn: &Connection,
    auth_user: &AuthUser,
    target_uid: &str,
    self_guard: bool,
) -> Result<i64, ApiError> {
    auth_user.require_role(ROLE_ADMIN)?;
    let (caller_id, caller_bits) = auth_user.require_auth()?;
    let target_id = database::get_user_id_by_uid(conn, target_uid)?
        .ok_or_else(|| ApiError::not_found("user not found"))?;
    if self_guard {
        ensure_not_self(caller_id, target_id)?;
    }
    ensure_can_manage(caller_bits, database::compute_role_bits(conn, target_id)?)?;
    Ok(target_id)
}

#[derive(Debug, serde::Deserialize)]
struct ResetPasswordBody {
    new_password: String,
    #[serde(default)]
    revoke_tokens: bool,
}

async fn admin_reset_password(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(uid): Path<String>,
    Json(body): Json<ResetPasswordBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let conn = database::open_auth_db(&state.auth_db_path)?;
    let target_id = resolve_target(&conn, &auth_user, &uid, true)?;
    if body.new_password.trim().is_empty() {
        return Err(ApiError::bad_request("new_password must not be blank"));
    }
    if body.new_password.chars().count() < 8 {
        return Err(ApiError::bad_request(
            "new_password must be at least 8 characters",
        ));
    }
    let new_hash = auth::hash_password(&body.new_password).map_err(ApiError::from)?;
    let (sessions_revoked, tokens_revoked) =
        auth_credentials::reset_user_password(&conn, target_id, &new_hash, body.revoke_tokens)?;
    Ok(Json(serde_json::json!({
        "user_uid": uid,
        "sessions_revoked": sessions_revoked,
        "tokens_revoked": tokens_revoked,
    })))
}

async fn admin_revoke_sessions(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(uid): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let conn = database::open_auth_db(&state.auth_db_path)?;
    let target_id = resolve_target(&conn, &auth_user, &uid, true)?;
    let revoked = database::invalidate_user_sessions(&conn, target_id)?;
    Ok(Json(serde_json::json!({ "revoked": revoked })))
}

async fn admin_list_tokens(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(uid): Path<String>,
) -> Result<Json<Vec<database::ApiTokenRecord>>, ApiError> {
    let conn = database::open_auth_db(&state.auth_db_path)?;
    let target_id = resolve_target(&conn, &auth_user, &uid, false)?;
    Ok(Json(database::list_user_tokens(&conn, target_id)?))
}

async fn admin_revoke_token(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path((uid, token_uid)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let conn = database::open_auth_db(&state.auth_db_path)?;
    let target_id = resolve_target(&conn, &auth_user, &uid, false)?;
    if database::delete_api_token(&conn, &token_uid, target_id)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("token not found"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::ServerRegistry;
    use crate::routes::app;
    use crate::test_support::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::{Value, json};
    use std::path::PathBuf;
    use tower::ServiceExt;

    struct Env {
        _dir: tempfile::TempDir,
        registry: ServerRegistry,
        auth: PathBuf,
    }

    impl Env {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let (registry, _, auth) = make_test_registry(&dir);
            Env { _dir: dir, registry, auth }
        }

        /// `who`: a `session=...` cookie string, or `Bearer:<raw>` for a token.
        async fn call(
            &self,
            method: &str,
            uri: &str,
            who: Option<&str>,
            body: Option<Value>,
        ) -> (StatusCode, Value) {
            let mut b = Request::builder().method(method).uri(uri);
            if let Some(w) = who {
                b = match w.strip_prefix("Bearer:") {
                    Some(tok) => b.header("authorization", format!("Bearer {tok}")),
                    None => b.header("cookie", w),
                };
            }
            let req = match body {
                Some(v) => b.header("content-type", "application/json").body(json_body(&v)),
                None => b.body(Body::empty()),
            }
            .unwrap();
            let resp = app(self.registry.clone(), self.auth.clone())
                .oneshot(req)
                .await
                .unwrap();
            let status = resp.status();
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
            let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            (status, value)
        }

        fn conn(&self) -> rusqlite::Connection {
            database::open_auth_db(&self.auth).unwrap()
        }

        fn user_id(&self, username: &str) -> i64 {
            self.conn()
                .query_row("SELECT id FROM users WHERE username = ?1", [username], |r| r.get(0))
                .unwrap()
        }

        fn user_uid(&self, username: &str) -> String {
            database::get_user_uid(&self.conn(), self.user_id(username)).unwrap().unwrap()
        }

        /// A user with a real argon2 password hash; returns their user_uid.
        fn real_user(&self, username: &str, password: &str, roles: &[&str]) -> String {
            let conn = self.conn();
            let owner = self.user_id("testowner");
            let hash = auth::hash_password(password).unwrap();
            let uid = database::create_user(&conn, username, None, &hash, owner).unwrap();
            let id = database::get_user_id_by_uid(&conn, &uid).unwrap().unwrap();
            for r in roles {
                database::assign_role(&conn, id, r, owner).unwrap();
            }
            uid
        }

        fn new_session(&self, username: &str) -> String {
            let conn = self.conn();
            let id = self.user_id(username);
            let bits = database::compute_role_bits(&conn, id).unwrap();
            format!(
                "session={}",
                database::create_session(&conn, id, bits, Some("test-agent")).unwrap()
            )
        }

        fn session_alive(&self, cookie: &str) -> bool {
            let uid = cookie.strip_prefix("session=").unwrap();
            database::get_session(&self.conn(), uid).unwrap().is_some()
        }
    }

    fn token_uid_of(env: &Env, username: &str) -> String {
        let id = env.user_id(username);
        database::list_user_tokens(&env.conn(), id).unwrap()[0].token_uid.clone()
    }

    fn handle_of(cookie: &str) -> String {
        auth_credentials::session_handle(cookie.strip_prefix("session=").unwrap())
    }

    // ---- S1-S3 ----

    #[tokio::test]
    async fn sessions_list_flags_current_and_never_leaks_session_uid() {
        let env = Env::new();
        let owner = owner_session(&env.auth);
        let second = env.new_session("testowner");
        let (st, _) = env.call("GET", "/api/auth/sessions", None, None).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);

        let (st, body) = env.call("GET", "/api/auth/sessions", Some(&owner), None).await;
        assert_eq!(st, StatusCode::OK);
        let rows = body.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        let currents: Vec<_> = rows.iter().filter(|r| r["current"] == true).collect();
        assert_eq!(currents.len(), 1);
        assert_eq!(currents[0]["session_handle"], handle_of(&owner));
        let other = rows.iter().find(|r| r["current"] == false).unwrap();
        assert_eq!(other["session_handle"], handle_of(&second));
        assert_eq!(other["user_agent"], "test-agent");
        for r in rows {
            let keys: std::collections::BTreeSet<_> =
                r.as_object().unwrap().keys().map(String::as_str).collect();
            assert_eq!(
                keys,
                [
                    "created_at",
                    "current",
                    "expires_at",
                    "last_seen_at",
                    "session_handle",
                    "user_agent"
                ]
                .into_iter()
                .collect()
            );
            assert_eq!(r["session_handle"].as_str().unwrap().len(), 16);
        }
        // No raw session_uid anywhere in the response text.
        let text = body.to_string();
        for cookie in [&owner, &second] {
            assert!(!text.contains(cookie.strip_prefix("session=").unwrap()));
        }

        // Bearer request: no row is "current".
        let tok = make_api_token(&env.auth, "testowner", None, "full");
        let (st, body) = env
            .call("GET", "/api/auth/sessions", Some(&format!("Bearer:{tok}")), None)
            .await;
        assert_eq!(st, StatusCode::OK);
        assert!(body.as_array().unwrap().iter().all(|r| r["current"] == false));
        for cookie in [&owner, &second] {
            assert!(!body.to_string().contains(cookie.strip_prefix("session=").unwrap()));
        }
    }

    #[tokio::test]
    async fn sessions_list_only_the_callers_own_sessions() {
        let env = Env::new();
        let owner = owner_session(&env.auth);
        let user = user_session(&env.auth);
        let (_, body) = env.call("GET", "/api/auth/sessions", Some(&user), None).await;
        assert_eq!(body.as_array().unwrap().len(), 1);
        assert_eq!(body[0]["session_handle"], handle_of(&user));
        assert!(!body.to_string().contains(&handle_of(&owner)));
    }

    #[tokio::test]
    async fn revoke_session_by_handle_kills_that_cookie() {
        let env = Env::new();
        let owner = owner_session(&env.auth);
        let other = env.new_session("testowner");
        let user = user_session(&env.auth);

        let uri = |c: &str| format!("/api/auth/sessions/{}", handle_of(c));
        let (st, _) = env.call("DELETE", &uri(&other), None, None).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        // Another user's handle, and an unknown one: 404, nothing deleted.
        for u in [uri(&user), "/api/auth/sessions/0000000000000000".to_string()] {
            let (st, _) = env.call("DELETE", &u, Some(&owner), None).await;
            assert_eq!(st, StatusCode::NOT_FOUND);
        }
        assert!(env.session_alive(&user));

        let (st, _) = env.call("DELETE", &uri(&other), Some(&owner), None).await;
        assert_eq!(st, StatusCode::NO_CONTENT);
        assert!(!env.session_alive(&other));
        let (st, _) = env.call("GET", "/api/auth/me", Some(&other), None).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        assert!(env.session_alive(&owner));

        // Deleting the current session is allowed (logout equivalent).
        let (st, _) = env.call("DELETE", &uri(&owner), Some(&owner), None).await;
        assert_eq!(st, StatusCode::NO_CONTENT);
        assert!(!env.session_alive(&owner));
    }

    #[tokio::test]
    async fn revoke_other_sessions_spares_current_cookie_but_not_for_bearer() {
        let env = Env::new();
        let owner = owner_session(&env.auth);
        let a = env.new_session("testowner");
        let b = env.new_session("testowner");
        let user = user_session(&env.auth);

        let (st, _) = env.call("DELETE", "/api/auth/sessions", None, None).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        let (st, body) = env.call("DELETE", "/api/auth/sessions", Some(&owner), None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(body, json!({"revoked": 2}));
        assert!(env.session_alive(&owner) && !env.session_alive(&a) && !env.session_alive(&b));
        assert!(env.session_alive(&user));

        let tok = make_api_token(&env.auth, "testowner", None, "full");
        let (st, body) = env
            .call("DELETE", "/api/auth/sessions", Some(&format!("Bearer:{tok}")), None)
            .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(body, json!({"revoked": 1}));
        assert!(!env.session_alive(&owner));
    }

    // ---- U2-U5 ----

    /// Runs `method /api/admin/users/<uid><tail>` as every caller class and
    /// checks the guard ladder against a plain-user target. Returns the env
    /// (users `test-admin`, `admin2`, `test-user`, `test-guest`, owner exist).
    async fn check_guards(method: &str, tail: &str, body: Option<Value>, self_guard: bool) -> Env {
        let env = Env::new();
        let owner = owner_session(&env.auth);
        let admin = admin_session(&env.auth);
        let user = user_session(&env.auth);
        let guest = guest_session(&env.auth);
        make_role_session(&env.auth, "admin2", &["admin"]);
        let target = env.user_uid("test-user");
        let uri = |uid: &str| format!("/api/admin/users/{uid}{tail}");

        let (st, _) = env.call(method, &uri(&target), None, body.clone()).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED, "guest");
        for (who, name) in [(&user, "user"), (&guest, "guest-role")] {
            let (st, _) = env.call(method, &uri(&target), Some(who), body.clone()).await;
            assert_eq!(st, StatusCode::FORBIDDEN, "{name}");
        }
        let (st, _) = env.call(method, &uri("usr_nope"), Some(&admin), body.clone()).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "unknown target");
        // Admin vs owner and admin vs another admin: 403.
        for t in ["testowner", "admin2"] {
            let (st, v) = env
                .call(method, &uri(&env.user_uid(t)), Some(&admin), body.clone())
                .await;
            assert_eq!(st, StatusCode::FORBIDDEN, "admin vs {t}: {v}");
        }
        // Self: 409 (for an admin that beats the 403 can-manage check).
        if self_guard {
            for (who, name) in [(&admin, "test-admin"), (&owner, "testowner")] {
                let (st, v) = env
                    .call(method, &uri(&env.user_uid(name)), Some(who), body.clone())
                    .await;
                assert_eq!(st, StatusCode::CONFLICT, "self {name}: {v}");
            }
        }
        env
    }

    #[tokio::test]
    async fn admin_password_guards_and_validation() {
        let body = json!({"new_password": "longenough"});
        let env = check_guards("POST", "/password", Some(body.clone()), true).await;
        let admin = env.new_session("test-admin");
        let target = env.user_uid("test-user");
        let uri = format!("/api/admin/users/{target}/password");
        for bad in [json!({"new_password": "short"}), json!({"new_password": "        "})] {
            let (st, _) = env.call("POST", &uri, Some(&admin), Some(bad)).await;
            assert_eq!(st, StatusCode::BAD_REQUEST);
        }
        // Owner may reset an admin's password.
        let owner = owner_session(&env.auth);
        let admin_uid = env.user_uid("admin2");
        let (st, v) = env
            .call(
                "POST",
                &format!("/api/admin/users/{admin_uid}/password"),
                Some(&owner),
                Some(body),
            )
            .await;
        assert_eq!(st, StatusCode::OK, "{v}");
    }

    #[tokio::test]
    async fn admin_password_reset_logs_in_with_new_password_and_kills_sessions() {
        let env = Env::new();
        let admin = admin_session(&env.auth);
        let uid = env.real_user("victim", "old-password", &["user"]);
        let old_session = env.new_session("victim");
        let tok = make_api_token(&env.auth, "victim", None, "full");
        let pw_uri = format!("/api/admin/users/{uid}/password");

        // Without revoke_tokens: sessions die, tokens survive.
        let (st, v) = env
            .call("POST", &pw_uri, Some(&admin), Some(json!({"new_password": "brand-new-pw"})))
            .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v, json!({"user_uid": uid, "sessions_revoked": 1, "tokens_revoked": 0}));
        assert!(!env.session_alive(&old_session));
        let (st, _) = env.call("GET", "/api/auth/me", Some(&old_session), None).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        let bearer = format!("Bearer:{tok}");
        let (st, _) = env.call("GET", "/api/auth/me", Some(&bearer), None).await;
        assert_eq!(st, StatusCode::OK);

        let login = |pw: &str| json!({"username": "victim", "password": pw});
        let (st, _) = env
            .call("POST", "/api/auth/login", None, Some(login("old-password")))
            .await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        let (st, _) = env
            .call("POST", "/api/auth/login", None, Some(login("brand-new-pw")))
            .await;
        assert_eq!(st, StatusCode::OK);

        // With revoke_tokens: the token dies too.
        let (st, v) = env
            .call(
                "POST",
                &pw_uri,
                Some(&admin),
                Some(json!({"new_password": "another-pw-1", "revoke_tokens": true})),
            )
            .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["tokens_revoked"], 1);
        assert_eq!(v["sessions_revoked"], 1, "the login above made one session");
        let (st, _) = env.call("GET", "/api/auth/me", Some(&bearer), None).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn admin_revoke_sessions_guards_and_ok() {
        let env = check_guards("DELETE", "/sessions", None, true).await;
        let admin = env.new_session("test-admin");
        let target = env.user_uid("test-user");
        let s1 = env.new_session("test-user");
        let (st, v) = env
            .call("DELETE", &format!("/api/admin/users/{target}/sessions"), Some(&admin), None)
            .await;
        assert_eq!(st, StatusCode::OK);
        assert!(v["revoked"].as_u64().unwrap() >= 2, "{v}");
        assert!(!env.session_alive(&s1));
    }

    #[tokio::test]
    async fn admin_list_tokens_guards_and_ok() {
        let env = check_guards("GET", "/tokens", None, false).await;
        let admin = env.new_session("test-admin");
        make_api_token(&env.auth, "test-user", None, "read");
        let target = env.user_uid("test-user");
        let (st, v) = env
            .call("GET", &format!("/api/admin/users/{target}/tokens"), Some(&admin), None)
            .await;
        assert_eq!(st, StatusCode::OK);
        let rows = v.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        let keys: std::collections::BTreeSet<_> =
            rows[0].as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["created_at", "expires_at", "last_used_at", "name", "scope", "token_uid"]
                .into_iter()
                .collect()
        );
        assert_eq!(rows[0]["scope"], "read");
        assert!(!v.to_string().contains("token_hash"));
        // No self-guard here (spec: can-manage only): an owner may list their own.
        let owner = owner_session(&env.auth);
        let me = env.user_uid("testowner");
        let (st, _) = env
            .call("GET", &format!("/api/admin/users/{me}/tokens"), Some(&owner), None)
            .await;
        assert_eq!(st, StatusCode::OK);
    }

    #[tokio::test]
    async fn admin_revoke_token_guards_ok_and_ownership() {
        let env = Env::new();
        let admin = admin_session(&env.auth);
        let user = user_session(&env.auth);
        make_role_session(&env.auth, "other", &["user"]);
        let raw = make_api_token(&env.auth, "test-user", None, "full");
        make_api_token(&env.auth, "other", None, "full");
        let target = env.user_uid("test-user");
        let tok_uid = token_uid_of(&env, "test-user");
        let other_tok = token_uid_of(&env, "other");
        let uri = |u: &str, t: &str| format!("/api/admin/users/{u}/tokens/{t}");

        let (st, _) = env.call("DELETE", &uri(&target, &tok_uid), None, None).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        let (st, _) = env.call("DELETE", &uri(&target, &tok_uid), Some(&user), None).await;
        assert_eq!(st, StatusCode::FORBIDDEN);
        let (st, _) = env.call("DELETE", &uri("usr_nope", &tok_uid), Some(&admin), None).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        // Admin vs owner target: 403.
        let owner_uid = env.user_uid("testowner");
        let (st, _) = env.call("DELETE", &uri(&owner_uid, &tok_uid), Some(&admin), None).await;
        assert_eq!(st, StatusCode::FORBIDDEN);
        // A token owned by someone else, or unknown: 404, and it survives.
        for t in [other_tok.as_str(), "tok_nope"] {
            let (st, _) = env.call("DELETE", &uri(&target, t), Some(&admin), None).await;
            assert_eq!(st, StatusCode::NOT_FOUND);
        }
        assert_eq!(database::list_user_tokens(&env.conn(), env.user_id("other")).unwrap().len(), 1);

        let bearer = format!("Bearer:{raw}");
        let (st, _) = env.call("GET", "/api/auth/me", Some(&bearer), None).await;
        assert_eq!(st, StatusCode::OK);
        let (st, _) = env.call("DELETE", &uri(&target, &tok_uid), Some(&admin), None).await;
        assert_eq!(st, StatusCode::NO_CONTENT);
        let (st, _) = env.call("GET", "/api/auth/me", Some(&bearer), None).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
    }

    // ---- T1/T2/M1, touch, scope ----

    #[tokio::test]
    async fn create_token_options_validation_and_listing() {
        let env = Env::new();
        let owner = owner_session(&env.auth);
        let (st, _) = env
            .call("POST", "/api/auth/tokens", None, Some(json!({"name": "x"})))
            .await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);

        // Omitted options keep the old behaviour (full, never expires).
        let (st, v) = env
            .call("POST", "/api/auth/tokens", Some(&owner), Some(json!({"name": "plain"})))
            .await;
        assert_eq!(st, StatusCode::CREATED);
        assert_eq!(v["scope"], "full");
        assert_eq!(v["name"], "plain");
        assert_eq!(v["expires_at"], Value::Null);
        assert!(v["raw_token"].as_str().is_some() && v["token_uid"].as_str().is_some());

        for bad in [
            json!({"name": "  "}),
            json!({"name": "x", "expires_in_days": 0}),
            json!({"name": "x", "expires_in_days": 3651}),
            json!({"name": "x", "expires_in_days": -1}),
            json!({"name": "x", "scope": "admin"}),
        ] {
            let (st, _) = env
                .call("POST", "/api/auth/tokens", Some(&owner), Some(bad.clone()))
                .await;
            assert_eq!(st, StatusCode::BAD_REQUEST, "{bad}");
        }

        let (st, v) = env
            .call(
                "POST",
                "/api/auth/tokens",
                Some(&owner),
                Some(json!({"name": "ro", "expires_in_days": 30, "scope": "read"})),
            )
            .await;
        assert_eq!(st, StatusCode::CREATED);
        assert_eq!(v["scope"], "read");
        let exp = chrono::DateTime::parse_from_rfc3339(v["expires_at"].as_str().unwrap()).unwrap();
        let days = (exp.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_days();
        assert!((29..=30).contains(&days), "{days}");
        let raw = v["raw_token"].as_str().unwrap().to_string();
        let (st, _) = env
            .call("GET", "/api/auth/me", Some(&format!("Bearer:{raw}")), None)
            .await;
        assert_eq!(st, StatusCode::OK);

        let (st, list) = env.call("GET", "/api/auth/tokens", Some(&owner), None).await;
        assert_eq!(st, StatusCode::OK);
        let rows = list.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        for r in rows {
            for k in ["token_uid", "name", "created_at", "last_used_at", "expires_at", "scope"] {
                assert!(r.get(k).is_some(), "missing {k}");
            }
        }
        assert!(!list.to_string().contains(&raw));
    }

    #[tokio::test]
    async fn backdated_token_expiry_is_401_through_the_router() {
        let env = Env::new();
        let past = (chrono::Utc::now() - chrono::Duration::days(1)).to_rfc3339();
        let expired = make_api_token(&env.auth, "testowner", Some(&past), "full");
        let (st, _) = env
            .call("GET", "/api/auth/me", Some(&format!("Bearer:{expired}")), None)
            .await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        let future = (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339();
        let ok = make_api_token(&env.auth, "testowner", Some(&future), "full");
        let (st, _) = env
            .call("GET", "/api/auth/me", Some(&format!("Bearer:{ok}")), None)
            .await;
        assert_eq!(st, StatusCode::OK);
    }

    #[tokio::test]
    async fn bearer_use_sets_last_used_at_throttled() {
        let env = Env::new();
        let raw = make_api_token(&env.auth, "testowner", None, "full");
        let tok_uid = token_uid_of(&env, "testowner");
        let last_used = |env: &Env| -> Option<String> {
            env.conn()
                .query_row(
                    "SELECT last_used_at FROM api_tokens WHERE token_uid = ?1",
                    [&tok_uid],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert_eq!(last_used(&env), None);
        let bearer = format!("Bearer:{raw}");
        env.call("GET", "/api/auth/me", Some(&bearer), None).await;
        let first = last_used(&env).expect("last_used_at set after a Bearer call");
        env.call("GET", "/api/auth/me", Some(&bearer), None).await;
        assert_eq!(last_used(&env).unwrap(), first, "second call within 60s is throttled");
        // After 60s it is refreshed.
        let old = (chrono::Utc::now() - chrono::Duration::seconds(120)).to_rfc3339();
        env.conn()
            .execute("UPDATE api_tokens SET last_used_at = ?1", [&old])
            .unwrap();
        env.call("GET", "/api/auth/me", Some(&bearer), None).await;
        assert_ne!(last_used(&env).unwrap(), old);
        // Visible via T2.
        let owner = owner_session(&env.auth);
        let (_, list) = env.call("GET", "/api/auth/tokens", Some(&owner), None).await;
        assert!(list[0]["last_used_at"].is_string());
    }

    #[tokio::test]
    async fn read_scope_token_cannot_mutate_but_can_read() {
        let env = Env::new();
        let owner = owner_session(&env.auth);
        let ro = format!("Bearer:{}", make_api_token(&env.auth, "testowner", None, "read"));
        let full = format!("Bearer:{}", make_api_token(&env.auth, "testowner", None, "full"));

        let (st, _) = env.call("GET", "/api/auth/me", Some(&ro), None).await;
        assert_eq!(st, StatusCode::OK);
        let (st, _) = env.call("GET", "/api/auth/tokens", Some(&ro), None).await;
        assert_eq!(st, StatusCode::OK);

        let attempts = [
            ("POST", "/api/auth/tokens", Some(json!({"name": "escalate"}))),
            ("DELETE", "/api/auth/tokens/tok_x", None),
            ("PATCH", "/api/auth/me", Some(json!({"display_name": "x"}))),
            ("DELETE", "/api/auth/sessions", None),
            (
                "POST",
                "/api/archives/test/captures",
                Some(json!({"locator": "local:/x"})),
            ),
        ];
        for (m, uri, body) in attempts {
            let (st, v) = env.call(m, uri, Some(&ro), body).await;
            assert_eq!(st, StatusCode::FORBIDDEN, "{m} {uri}");
            assert_eq!(v["error"], "read-only token");
        }
        let (_, list) = env.call("GET", "/api/auth/tokens", Some(&owner), None).await;
        assert_eq!(list.as_array().unwrap().len(), 2, "no token was created");

        // Full tokens and cookie sessions are unaffected.
        let (st, _) = env
            .call("POST", "/api/auth/tokens", Some(&full), Some(json!({"name": "ok"})))
            .await;
        assert_eq!(st, StatusCode::CREATED);
        let (st, _) = env
            .call("POST", "/api/auth/tokens", Some(&owner), Some(json!({"name": "ok2"})))
            .await;
        assert_eq!(st, StatusCode::CREATED);
        // An unauthenticated mutation is still 401, not 403.
        let (st, _) = env
            .call(
                "POST",
                "/api/auth/tokens",
                Some("Bearer:garbage"),
                Some(json!({"name": "x"})),
            )
            .await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_me_reports_user_uid_and_roles() {
        let env = Env::new();
        let owner = owner_session(&env.auth);
        let admin = admin_session(&env.auth);
        let (st, v) = env.call("GET", "/api/auth/me", Some(&owner), None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["user_uid"], env.user_uid("testowner"));
        assert_eq!(v["roles"], json!(["user", "admin", "owner"]));
        assert!(v["role_bits"].is_u64() && v["username"] == "testowner");
        let (_, v) = env.call("GET", "/api/auth/me", Some(&admin), None).await;
        assert_eq!(v["roles"], json!(["user", "admin"]));
    }

    // ---- patch_me ----

    #[tokio::test]
    async fn patch_me_password_rules_and_session_invalidation() {
        let env = Env::new();
        env.real_user("pat", "old-password", &["user"]);
        let current = env.new_session("pat");
        let other = env.new_session("pat");
        let patch = |new: &str| json!({"current_password": "old-password", "new_password": new});

        let (st, _) = env
            .call("PATCH", "/api/auth/me", Some(&current), Some(patch("short")))
            .await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert!(env.session_alive(&other), "rejected change must not touch sessions");

        let (st, _) = env
            .call("PATCH", "/api/auth/me", Some(&current), Some(patch("new-password-1")))
            .await;
        assert_eq!(st, StatusCode::NO_CONTENT);
        assert!(env.session_alive(&current));
        assert!(!env.session_alive(&other));
        let (st, _) = env
            .call(
                "POST",
                "/api/auth/login",
                None,
                Some(json!({"username": "pat", "password": "new-password-1"})),
            )
            .await;
        assert_eq!(st, StatusCode::OK);
    }
}
