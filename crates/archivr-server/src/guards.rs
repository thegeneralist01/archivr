//! Shared authorization guards for user/role/token management endpoints.
//!
//! Callers still do their own coarse role check (`require_role(ROLE_ADMIN)`);
//! these guards add the target-aware rules on top.
use rusqlite::Connection;

use crate::auth::{ROLE_ADMIN, ROLE_OWNER};
use crate::routes::ApiError;

/// Strict management rule: only an OWNER may act on a target that holds the
/// OWNER **or** ADMIN bit. Admins may manage everyone else. 403 otherwise.
pub fn ensure_can_manage(caller_bits: u32, target_bits: u32) -> Result<(), ApiError> {
    if target_bits & (ROLE_OWNER | ROLE_ADMIN) != 0 && caller_bits & ROLE_OWNER == 0 {
        return Err(ApiError::forbidden(
            "only an owner can manage owners and admins",
        ));
    }
    Ok(())
}

/// Rejects an action aimed at the caller's own account (409).
pub fn ensure_not_self(caller_id: i64, target_id: i64) -> Result<(), ApiError> {
    if caller_id == target_id {
        return Err(ApiError::conflict("you cannot do this to your own account"));
    }
    Ok(())
}

/// Rejects an action that would leave the instance without an *active* owner
/// (409). Passes when the target is not an active owner, or when another
/// active owner remains.
pub fn ensure_not_last_owner(conn: &Connection, target_id: i64) -> Result<(), ApiError> {
    let active_owners = |exclude_target: bool| -> Result<i64, ApiError> {
        let sql = format!(
            "SELECT COUNT(*) FROM user_roles ur
             JOIN roles r ON r.id = ur.role_id
             JOIN users u ON u.id = ur.user_id
             WHERE r.slug = 'owner' AND u.status = 'active'{}",
            if exclude_target { " AND u.id != ?1" } else { " AND u.id = ?1" }
        );
        Ok(conn.query_row(&sql, [target_id], |row| row.get(0))?)
    };
    let target_is_active_owner = active_owners(false)? > 0;
    if target_is_active_owner && active_owners(true)? == 0 {
        return Err(ApiError::conflict("cannot remove the last active owner"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{ROLE_GUEST, ROLE_USER};
    use archivr_core::database;
    use axum::http::StatusCode;

    fn auth_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        database::initialize_auth_schema(&conn).unwrap();
        conn
    }

    fn add_owner(conn: &Connection, username: &str, creator: i64) -> i64 {
        let uid = database::create_user(conn, username, None, "dummy", creator).unwrap();
        let id = database::get_user_id_by_uid(conn, &uid).unwrap().unwrap();
        database::assign_role(conn, id, "owner", creator).unwrap();
        id
    }

    #[test]
    fn manage_rule_requires_owner_for_owner_and_admin_targets() {
        let user = ROLE_USER;
        let admin = ROLE_USER | ROLE_ADMIN;
        let owner = ROLE_USER | ROLE_ADMIN | ROLE_OWNER;
        // Admin vs plain user / guest / custom-role user: allowed.
        assert!(ensure_can_manage(admin, user).is_ok());
        assert!(ensure_can_manage(admin, ROLE_GUEST).is_ok());
        assert!(ensure_can_manage(admin, user | 1 << 4).is_ok());
        // Admin vs admin and admin vs owner: 403.
        for target in [admin, owner, ROLE_ADMIN, ROLE_OWNER] {
            let err = ensure_can_manage(admin, target).unwrap_err();
            assert_eq!(err.status, StatusCode::FORBIDDEN);
        }
        // Owner may manage everyone.
        for target in [user, admin, owner] {
            assert!(ensure_can_manage(owner, target).is_ok());
        }
    }

    #[test]
    fn not_self_conflicts_only_on_same_id() {
        assert!(ensure_not_self(1, 2).is_ok());
        let err = ensure_not_self(3, 3).unwrap_err();
        assert_eq!(err.status, StatusCode::CONFLICT);
    }

    #[test]
    fn last_owner_guard_counts_only_active_owners() {
        let conn = auth_conn();
        let first = database::create_owner(&conn, "first", "pw").unwrap();
        // Sole owner: blocked.
        assert_eq!(
            ensure_not_last_owner(&conn, first).unwrap_err().status,
            StatusCode::CONFLICT
        );
        // A plain user is never a "last owner".
        let uid = database::create_user(&conn, "plain", None, "dummy", first).unwrap();
        let plain = database::get_user_id_by_uid(&conn, &uid).unwrap().unwrap();
        assert!(ensure_not_last_owner(&conn, plain).is_ok());

        // Second owner makes either removable.
        let second = add_owner(&conn, "second", first);
        assert!(ensure_not_last_owner(&conn, first).is_ok());
        assert!(ensure_not_last_owner(&conn, second).is_ok());

        // Disable the second owner: it no longer counts, so the first is last again.
        let second_uid = database::get_user_uid(&conn, second).unwrap().unwrap();
        assert!(database::set_user_status(&conn, &second_uid, "disabled").unwrap());
        assert_eq!(
            ensure_not_last_owner(&conn, first).unwrap_err().status,
            StatusCode::CONFLICT
        );
        // A disabled owner is not an active owner: touching it is fine.
        assert!(ensure_not_last_owner(&conn, second).is_ok());
    }
}
