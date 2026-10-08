//! Admin user/role management: delete user (U1), rename role (R1), delete role (R2).
//! The status/roles handlers that predate this module live in `routes.rs`.
use archivr_core::{auth_users, database};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, patch},
};

use crate::auth::{AuthUser, ROLE_ADMIN, ROLE_OWNER};
use crate::guards;
use crate::routes::{ApiError, AppState};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/admin/users/:uid", delete(admin_delete_user))
        .route(
            "/api/admin/roles/:slug",
            patch(admin_rename_role).delete(admin_delete_role),
        )
}

/// U1: guards are role -> target 404 -> self 409 -> can-manage 403 -> last-owner 409.
async fn admin_delete_user(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(uid): Path<String>,
) -> Result<StatusCode, ApiError> {
    auth_user.require_role(ROLE_ADMIN)?;
    let (caller_id, caller_bits) = auth_user.require_auth()?;
    let mut conn = database::open_auth_db(&state.auth_db_path)?;
    let target_id = database::get_user_id_by_uid(&conn, &uid)?
        .ok_or_else(|| ApiError::not_found("user not found"))?;
    guards::ensure_not_self(caller_id, target_id)?;
    guards::ensure_can_manage(caller_bits, database::compute_role_bits(&conn, target_id)?)?;
    guards::ensure_not_last_owner(&conn, target_id)?;
    if !auth_users::delete_user(&mut conn, target_id)? {
        return Err(ApiError::not_found("user not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, serde::Deserialize)]
struct AdminRenameRoleBody {
    name: String,
}

/// R1: custom roles only; the slug never changes.
async fn admin_rename_role(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(slug): Path<String>,
    Json(body): Json<AdminRenameRoleBody>,
) -> Result<Json<database::RoleRecord>, ApiError> {
    auth_user.require_role(ROLE_ADMIN)?;
    let conn = database::open_auth_db(&state.auth_db_path)?;
    let role = auth_users::get_role_by_slug(&conn, &slug)?
        .ok_or_else(|| ApiError::not_found("role not found"))?;
    if role.is_builtin {
        return Err(ApiError::bad_request("built-in roles cannot be renamed"));
    }
    let name = body.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request("role name is required"));
    }
    let updated = auth_users::rename_role(&conn, &slug, name)?
        .ok_or_else(|| ApiError::not_found("role not found"))?;
    Ok(Json(updated))
}

/// R2: OWNER only; custom roles only.
async fn admin_delete_role(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(slug): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    auth_user.require_role(ROLE_OWNER)?;
    let mut conn = database::open_auth_db(&state.auth_db_path)?;
    let role = auth_users::get_role_by_slug(&conn, &slug)?
        .ok_or_else(|| ApiError::not_found("role not found"))?;
    if role.is_builtin {
        return Err(ApiError::bad_request("built-in roles cannot be deleted"));
    }
    let outcome = auth_users::delete_custom_role(&mut conn, &slug)?
        .ok_or_else(|| ApiError::not_found("role not found"))?;
    Ok(Json(serde_json::json!({
        "slug": slug,
        "users_affected": outcome.users_affected,
        "reorder_mask_cleared": outcome.reorder_mask_cleared,
    })))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use archivr_core::{auth_users, database};

    use crate::registry::ServerRegistry;
    use crate::routes::app;
    use crate::test_support::*;

    struct Env {
        _dir: tempfile::TempDir,
        registry: ServerRegistry,
        auth_path: std::path::PathBuf,
    }

    impl Env {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let (registry, _, auth_path) = make_test_registry(&dir);
            Env { _dir: dir, registry, auth_path }
        }

        async fn call(
            &self,
            cookie: Option<&str>,
            method: &str,
            uri: &str,
            body: Option<Value>,
        ) -> (StatusCode, Value) {
            let mut b = Request::builder().method(method).uri(uri);
            if let Some(c) = cookie {
                b = b.header("cookie", c);
            }
            let req = match body {
                Some(v) => b
                    .header("content-type", "application/json")
                    .body(json_body(&v))
                    .unwrap(),
                None => b.body(Body::empty()).unwrap(),
            };
            let resp = app(self.registry.clone(), self.auth_path.clone())
                .oneshot(req)
                .await
                .unwrap();
            let status = resp.status();
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
            let value = if bytes.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&bytes).unwrap()
            };
            (status, value)
        }

        /// Creates an (unused-session) user with `roles` and returns its `user_uid`.
        fn user_uid(&self, username: &str, roles: &[&str]) -> String {
            let _ = make_role_session(&self.auth_path, username, roles);
            let conn = database::open_auth_db(&self.auth_path).unwrap();
            conn.query_row(
                "SELECT user_uid FROM users WHERE username = ?1",
                [username],
                |r| r.get(0),
            )
            .unwrap()
        }

        fn user_uid_by_name(&self, username: &str) -> String {
            let conn = database::open_auth_db(&self.auth_path).unwrap();
            conn.query_row(
                "SELECT user_uid FROM users WHERE username = ?1",
                [username],
                |r| r.get(0),
            )
            .unwrap()
        }

        fn user_id(&self, user_uid: &str) -> i64 {
            let conn = database::open_auth_db(&self.auth_path).unwrap();
            database::get_user_id_by_uid(&conn, user_uid).unwrap().unwrap()
        }

        fn exists(&self, user_uid: &str) -> bool {
            let conn = database::open_auth_db(&self.auth_path).unwrap();
            database::get_user_id_by_uid(&conn, user_uid).unwrap().is_some()
        }
    }

    fn count(auth_path: &Path, sql: &str, id: i64) -> i64 {
        let conn = database::open_auth_db(auth_path).unwrap();
        conn.query_row(sql, [id], |r| r.get(0)).unwrap()
    }

    // ----- U1: DELETE /api/admin/users/:uid -----

    #[tokio::test]
    async fn delete_user_role_gates_and_target_rules() {
        let env = Env::new();
        let victim = env.user_uid("victim", &["user"]);
        let uri = format!("/api/admin/users/{victim}");

        let (s, _) = env.call(None, "DELETE", &uri, None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let guest = guest_session(&env.auth_path);
        let (s, _) = env.call(Some(&guest), "DELETE", &uri, None).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let user = user_session(&env.auth_path);
        let (s, _) = env.call(Some(&user), "DELETE", &uri, None).await;
        assert_eq!(s, StatusCode::FORBIDDEN);

        let admin = admin_session(&env.auth_path);
        let (s, b) = env.call(Some(&admin), "DELETE", "/api/admin/users/usr_nope", None).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{b}");

        // Admin vs owner / admin: 403. Admin vs self: 409.
        let owner_uid = env.user_uid_by_name("testowner");
        let other_admin = env.user_uid("other-admin", &["admin"]);
        for target in [&owner_uid, &other_admin] {
            let (s, _) = env
                .call(Some(&admin), "DELETE", &format!("/api/admin/users/{target}"), None)
                .await;
            assert_eq!(s, StatusCode::FORBIDDEN);
            assert!(env.exists(target));
        }
        let admin_uid = env.user_uid_by_name("test-admin");
        let (s, _) = env
            .call(Some(&admin), "DELETE", &format!("/api/admin/users/{admin_uid}"), None)
            .await;
        assert_eq!(s, StatusCode::CONFLICT);

        // Admin deletes a plain user.
        let (s, _) = env.call(Some(&admin), "DELETE", &uri, None).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        assert!(!env.exists(&victim));
    }

    #[tokio::test]
    async fn delete_user_owner_rules() {
        let env = Env::new();
        let owner = owner_session(&env.auth_path);
        let second_owner_cookie = make_role_session(&env.auth_path, "owner2", &["owner"]);
        let owner2_uid = env.user_uid_by_name("owner2");
        let owner1_uid = env.user_uid_by_name("testowner");
        let admin_uid = env.user_uid("an-admin", &["admin"]);

        // Self: 409 even for an owner.
        let (s, _) = env
            .call(Some(&owner), "DELETE", &format!("/api/admin/users/{owner1_uid}"), None)
            .await;
        assert_eq!(s, StatusCode::CONFLICT);
        // Owner may delete an admin and another owner.
        let (s, _) = env
            .call(Some(&owner), "DELETE", &format!("/api/admin/users/{admin_uid}"), None)
            .await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        let (s, _) = env
            .call(Some(&owner), "DELETE", &format!("/api/admin/users/{owner2_uid}"), None)
            .await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        // Its session died with it.
        let (s, _) = env.call(Some(&second_owner_cookie), "GET", "/api/admin/users", None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn delete_user_removes_sessions_tokens_and_survives_granted_roles() {
        let env = Env::new();
        let owner = owner_session(&env.auth_path);
        // `grantor` is an admin who assigned a role to `grantee` (FK on assigned_by).
        let grantor_cookie = make_role_session(&env.auth_path, "grantor", &["admin"]);
        let grantor_uid = env.user_uid_by_name("grantor");
        let grantor_id = env.user_id(&grantor_uid);
        let grantee_uid = env.user_uid("grantee", &[]);
        let grantee_id = env.user_id(&grantee_uid);
        let (s, b) = env
            .call(
                Some(&grantor_cookie),
                "POST",
                &format!("/api/admin/users/{grantee_uid}/roles"),
                Some(json!({"role_slug": "user"})),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{b}");
        let token = make_api_token(&env.auth_path, "grantor", None, "full");
        assert_eq!(count(&env.auth_path, "SELECT COUNT(*) FROM api_tokens WHERE user_id = ?1", grantor_id), 1);

        let (s, b) = env
            .call(Some(&owner), "DELETE", &format!("/api/admin/users/{grantor_uid}"), None)
            .await;
        assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
        assert!(!env.exists(&grantor_uid));
        assert_eq!(count(&env.auth_path, "SELECT COUNT(*) FROM sessions WHERE user_id = ?1", grantor_id), 0);
        assert_eq!(count(&env.auth_path, "SELECT COUNT(*) FROM api_tokens WHERE user_id = ?1", grantor_id), 0);
        assert_eq!(count(&env.auth_path, "SELECT COUNT(*) FROM user_roles WHERE user_id = ?1", grantor_id), 0);
        // Grantee keeps the role it was given.
        assert!(
            auth_users::user_has_role(
                &database::open_auth_db(&env.auth_path).unwrap(),
                grantee_id,
                "user"
            )
            .unwrap()
        );
        // The cookie and Bearer token are dead.
        let (s, _) = env.call(Some(&grantor_cookie), "GET", "/api/admin/users", None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let resp = app(env.registry.clone(), env.auth_path.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/admin/users")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // ----- R1: PATCH /api/admin/roles/:slug -----

    #[tokio::test]
    async fn rename_role_gates_validation_and_slug_stability() {
        let env = Env::new();
        let owner = owner_session(&env.auth_path);
        let (s, _) = env
            .call(Some(&owner), "POST", "/api/admin/roles", Some(json!({"slug": "editors", "name": "Editors"})))
            .await;
        assert_eq!(s, StatusCode::CREATED);
        let uri = "/api/admin/roles/editors";
        let body = || Some(json!({"name": "  Chief Editors "}));

        let (s, _) = env.call(None, "PATCH", uri, body()).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let guest = guest_session(&env.auth_path);
        let (s, _) = env.call(Some(&guest), "PATCH", uri, body()).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let user = user_session(&env.auth_path);
        let (s, _) = env.call(Some(&user), "PATCH", uri, body()).await;
        assert_eq!(s, StatusCode::FORBIDDEN);

        let admin = admin_session(&env.auth_path);
        let (s, b) = env.call(Some(&admin), "PATCH", uri, Some(json!({"name": "   "}))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{b}");
        for slug in ["guest", "user", "admin", "owner"] {
            let (s, _) = env
                .call(Some(&admin), "PATCH", &format!("/api/admin/roles/{slug}"), body())
                .await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "{slug}");
        }
        let (s, _) = env.call(Some(&admin), "PATCH", "/api/admin/roles/nope", body()).await;
        assert_eq!(s, StatusCode::NOT_FOUND);

        let (s, b) = env.call(Some(&admin), "PATCH", uri, body()).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(b["slug"], "editors");
        assert_eq!(b["name"], "Chief Editors");
        assert_eq!(b["is_builtin"], false);
        assert!(b["role_uid"].is_string() && b["bit_position"].is_number());
        let (_, roles) = env.call(Some(&owner), "GET", "/api/admin/roles", None).await;
        let listed = roles.as_array().unwrap().iter().find(|r| r["slug"] == "editors").unwrap();
        assert_eq!(listed["name"], "Chief Editors");
    }

    // ----- R2: DELETE /api/admin/roles/:slug -----

    #[tokio::test]
    async fn delete_role_gates_and_builtin_rejection() {
        let env = Env::new();
        let owner = owner_session(&env.auth_path);
        env.call(Some(&owner), "POST", "/api/admin/roles", Some(json!({"slug": "editors", "name": "Editors"})))
            .await;
        let uri = "/api/admin/roles/editors";

        let (s, _) = env.call(None, "DELETE", uri, None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let guest = guest_session(&env.auth_path);
        let (s, _) = env.call(Some(&guest), "DELETE", uri, None).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let user = user_session(&env.auth_path);
        let (s, _) = env.call(Some(&user), "DELETE", uri, None).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        // OWNER only: admin is not enough.
        let admin = admin_session(&env.auth_path);
        let (s, _) = env.call(Some(&admin), "DELETE", uri, None).await;
        assert_eq!(s, StatusCode::FORBIDDEN);

        for slug in ["guest", "user", "admin", "owner"] {
            let (s, _) = env
                .call(Some(&owner), "DELETE", &format!("/api/admin/roles/{slug}"), None)
                .await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "{slug}");
        }
        let (s, _) = env.call(Some(&owner), "DELETE", "/api/admin/roles/nope", None).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_role_strips_holders_sessions_and_reorder_bit() {
        let env = Env::new();
        let owner = owner_session(&env.auth_path);
        let (_, role) = env
            .call(Some(&owner), "POST", "/api/admin/roles", Some(json!({"slug": "editors", "name": "Editors"})))
            .await;
        let bit = 1u32 << role["bit_position"].as_u64().unwrap();
        let holder_cookie = make_role_session(&env.auth_path, "holder", &["editors"]);
        let holder_id = env.user_id(&env.user_uid_by_name("holder"));
        // Grant the custom role the reorder permission.
        let (s, b) = env
            .call(
                Some(&owner),
                "PATCH",
                "/api/admin/instance-settings",
                Some(json!({"reorder_children_role_bits": 12 | bit})),
            )
            .await;
        assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
        let (_, me) = env.call(Some(&holder_cookie), "GET", "/api/auth/me", None).await;
        assert!(me["role_bits"].as_u64().unwrap() as u32 & bit != 0);

        let (s, b) = env.call(Some(&owner), "DELETE", "/api/admin/roles/editors", None).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(b, json!({"slug": "editors", "users_affected": 1, "reorder_mask_cleared": true}));

        // Holder lost the bit (rows gone), its session is invalid, mask no longer has the bit.
        let conn = database::open_auth_db(&env.auth_path).unwrap();
        assert_eq!(database::compute_role_bits(&conn, holder_id).unwrap() & bit, 0);
        assert_eq!(count(&env.auth_path, "SELECT COUNT(*) FROM sessions WHERE user_id = ?1", holder_id), 0);
        let (s, _) = env.call(Some(&holder_cookie), "GET", "/api/auth/me", None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let (_, settings) = env.call(Some(&owner), "GET", "/api/admin/instance-settings", None).await;
        assert_eq!(settings["reorder_children_role_bits"], 12);
        let (s, _) = env.call(Some(&owner), "DELETE", "/api/admin/roles/editors", None).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        // Unaffected users keep their sessions.
        let (s, _) = env.call(Some(&owner), "GET", "/api/auth/me", None).await;
        assert_eq!(s, StatusCode::OK);
    }

    // ----- Hardened: PATCH /api/admin/users/:uid/status -----

    #[tokio::test]
    async fn set_status_guards() {
        let env = Env::new();
        let owner = owner_session(&env.auth_path);
        let admin = admin_session(&env.auth_path);
        let user = user_session(&env.auth_path);
        let plain = env.user_uid("plain", &["user"]);
        let other_admin = env.user_uid("other-admin", &["admin"]);
        let owner_uid = env.user_uid_by_name("testowner");
        let admin_uid = env.user_uid_by_name("test-admin");
        let patch = |uid: &str| format!("/api/admin/users/{uid}/status");
        let disable = || Some(json!({"status": "disabled"}));

        let (s, _) = env.call(None, "PATCH", &patch(&plain), disable()).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let guest = guest_session(&env.auth_path);
        let (s, _) = env.call(Some(&guest), "PATCH", &patch(&plain), disable()).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let (s, _) = env.call(Some(&user), "PATCH", &patch(&plain), disable()).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let (s, _) = env.call(Some(&admin), "PATCH", &patch("usr_nope"), disable()).await;
        assert_eq!(s, StatusCode::NOT_FOUND);

        // Self (admin and owner): 409. Admin vs owner/admin: 403.
        let (s, _) = env.call(Some(&admin), "PATCH", &patch(&admin_uid), disable()).await;
        assert_eq!(s, StatusCode::CONFLICT);
        let (s, _) = env.call(Some(&owner), "PATCH", &patch(&owner_uid), disable()).await;
        assert_eq!(s, StatusCode::CONFLICT);
        for target in [&owner_uid, &other_admin] {
            let (s, _) = env.call(Some(&admin), "PATCH", &patch(target), disable()).await;
            assert_eq!(s, StatusCode::FORBIDDEN);
            let (s, _) = env
                .call(Some(&admin), "PATCH", &patch(target), Some(json!({"status": "active"})))
                .await;
            assert_eq!(s, StatusCode::FORBIDDEN);
        }

        // Admin disables/re-enables a plain user; owner may disable an admin.
        let (s, b) = env.call(Some(&admin), "PATCH", &patch(&plain), disable()).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(b["status"], "disabled");
        let (s, _) = env
            .call(Some(&admin), "PATCH", &patch(&plain), Some(json!({"status": "active"})))
            .await;
        assert_eq!(s, StatusCode::OK);
        let (s, _) = env.call(Some(&owner), "PATCH", &patch(&other_admin), disable()).await;
        assert_eq!(s, StatusCode::OK);
        let (s, _) = env
            .call(Some(&owner), "PATCH", &patch(&plain), Some(json!({"status": "bogus"})))
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }

    // ----- Hardened: POST/DELETE /api/admin/users/:uid/roles -----

    #[tokio::test]
    async fn assign_role_guards() {
        let env = Env::new();
        let owner = owner_session(&env.auth_path);
        let admin = admin_session(&env.auth_path);
        let user = user_session(&env.auth_path);
        let plain = env.user_uid("plain", &["user"]);
        let other_admin = env.user_uid("other-admin", &["admin"]);
        let owner_uid = env.user_uid_by_name("testowner");
        env.call(Some(&owner), "POST", "/api/admin/roles", Some(json!({"slug": "editors", "name": "Editors"})))
            .await;
        let uri = |uid: &str| format!("/api/admin/users/{uid}/roles");
        let grant = |slug: &str| Some(json!({"role_slug": slug}));

        let (s, _) = env.call(None, "POST", &uri(&plain), grant("editors")).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let guest = guest_session(&env.auth_path);
        let (s, _) = env.call(Some(&guest), "POST", &uri(&plain), grant("editors")).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let (s, _) = env.call(Some(&user), "POST", &uri(&plain), grant("editors")).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let (s, _) = env.call(Some(&admin), "POST", &uri("usr_nope"), grant("editors")).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        // Unknown slug: 404 (was 500).
        let (s, _) = env.call(Some(&admin), "POST", &uri(&plain), grant("nope")).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (s, _) = env.call(Some(&owner), "POST", &uri(&plain), grant("nope")).await;
        assert_eq!(s, StatusCode::NOT_FOUND);

        // Admin may grant custom/user roles to plain users but not owner/admin.
        let (s, _) = env.call(Some(&admin), "POST", &uri(&plain), grant("editors")).await;
        assert_eq!(s, StatusCode::OK);
        for slug in ["owner", "admin"] {
            let (s, _) = env.call(Some(&admin), "POST", &uri(&plain), grant(slug)).await;
            assert_eq!(s, StatusCode::FORBIDDEN, "{slug}");
        }
        // Admin vs admin / owner targets: 403.
        for target in [&other_admin, &owner_uid] {
            let (s, _) = env.call(Some(&admin), "POST", &uri(target), grant("editors")).await;
            assert_eq!(s, StatusCode::FORBIDDEN);
        }
        // Owner can grant admin, and manage an admin.
        let (s, _) = env.call(Some(&owner), "POST", &uri(&plain), grant("admin")).await;
        assert_eq!(s, StatusCode::OK);
        let (s, _) = env.call(Some(&owner), "POST", &uri(&other_admin), grant("editors")).await;
        assert_eq!(s, StatusCode::OK);
    }

    #[tokio::test]
    async fn remove_role_guards() {
        let env = Env::new();
        let owner = owner_session(&env.auth_path);
        let admin = admin_session(&env.auth_path);
        let user = user_session(&env.auth_path);
        env.call(Some(&owner), "POST", "/api/admin/roles", Some(json!({"slug": "editors", "name": "Editors"})))
            .await;
        let plain = env.user_uid("plain", &["editors"]);
        let other_admin = env.user_uid("other-admin", &["admin"]);
        let owner_uid = env.user_uid_by_name("testowner");
        let uri = |uid: &str, slug: &str| format!("/api/admin/users/{uid}/roles/{slug}");

        let (s, _) = env.call(None, "DELETE", &uri(&plain, "editors"), None).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let guest = guest_session(&env.auth_path);
        let (s, _) = env.call(Some(&guest), "DELETE", &uri(&plain, "editors"), None).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let (s, _) = env.call(Some(&user), "DELETE", &uri(&plain, "editors"), None).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let (s, _) = env.call(Some(&admin), "DELETE", &uri("usr_nope", "editors"), None).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        // Unknown slug: 404 (was 500).
        let (s, _) = env.call(Some(&admin), "DELETE", &uri(&plain, "nope"), None).await;
        assert_eq!(s, StatusCode::NOT_FOUND);

        // Admin vs admin / owner target: 403; admin cannot remove owner/admin from anyone.
        for target in [&other_admin, &owner_uid] {
            let (s, _) = env.call(Some(&admin), "DELETE", &uri(target, "editors"), None).await;
            assert_eq!(s, StatusCode::FORBIDDEN);
        }
        let (s, _) = env.call(Some(&admin), "DELETE", &uri(&plain, "admin"), None).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let (s, _) = env.call(Some(&admin), "DELETE", &uri(&plain, "owner"), None).await;
        assert_eq!(s, StatusCode::FORBIDDEN);

        // Admin removes a custom role from a plain user.
        let (s, _) = env.call(Some(&admin), "DELETE", &uri(&plain, "editors"), None).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
        // Owner removes admin from an admin.
        let (s, _) = env.call(Some(&owner), "DELETE", &uri(&other_admin, "admin"), None).await;
        assert_eq!(s, StatusCode::NO_CONTENT);

        // Owner removing owner from the last owner: 409 (was 500).
        let (s, b) = env.call(Some(&owner), "DELETE", &uri(&owner_uid, "owner"), None).await;
        assert_eq!(s, StatusCode::CONFLICT, "{b}");
        // Removing owner from a non-owner is a harmless no-op, not a 500.
        let (s, _) = env.call(Some(&owner), "DELETE", &uri(&plain, "owner"), None).await;
        assert_eq!(s, StatusCode::NO_CONTENT);

        // With a second owner, removal is allowed; and a *disabled* second owner does not count.
        let second = env.user_uid("owner2", &["owner"]);
        let (s, _) = env
            .call(Some(&owner), "PATCH", &format!("/api/admin/users/{second}/status"), Some(json!({"status": "disabled"})))
            .await;
        assert_eq!(s, StatusCode::OK);
        let (s, _) = env.call(Some(&owner), "DELETE", &uri(&owner_uid, "owner"), None).await;
        assert_eq!(s, StatusCode::CONFLICT);
        let (s, _) = env
            .call(Some(&owner), "PATCH", &format!("/api/admin/users/{second}/status"), Some(json!({"status": "active"})))
            .await;
        assert_eq!(s, StatusCode::OK);
        let (s, _) = env.call(Some(&owner), "DELETE", &uri(&second, "owner"), None).await;
        assert_eq!(s, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn remove_owner_from_a_disabled_second_owner_is_allowed() {
        let env = Env::new();
        let owner = owner_session(&env.auth_path);
        let second = env.user_uid("owner2", &["owner"]);
        let (s, _) = env
            .call(Some(&owner), "PATCH", &format!("/api/admin/users/{second}/status"), Some(json!({"status": "disabled"})))
            .await;
        assert_eq!(s, StatusCode::OK);
        let (s, b) = env
            .call(Some(&owner), "DELETE", &format!("/api/admin/users/{second}/roles/owner"), None)
            .await;
        assert_eq!(s, StatusCode::NO_CONTENT, "{b}");
    }
}
