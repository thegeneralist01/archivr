//! Auth-DB user management (delete, password reset, per-user session/token
//! revocation) and custom-role administration.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

use crate::database::{self, RoleRecord};

/// Result of [`delete_custom_role`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleDeletion {
    /// Users that held the role (their sessions were invalidated).
    pub users_affected: usize,
    /// True when the role's bit was set in `reorder_children_role_bits` and got cleared.
    pub reorder_mask_cleared: bool,
}

/// Deletes a user. `user_roles.assigned_by_user_id` has no ON DELETE action, so rows
/// the user granted to others are detached first; sessions, tokens and the user's own
/// `user_roles` rows cascade. Returns false when the user does not exist.
pub fn delete_user(conn: &mut Connection, user_id: i64) -> Result<bool> {
    let tx = conn.transaction()?;
    let deleted = delete_user_in(&tx, user_id)?;
    tx.commit()?;
    Ok(deleted)
}

/// [`delete_user`] without its own transaction, for callers that already hold one
/// (e.g. a last-owner check that must run under the same write lock).
pub fn delete_user_in(conn: &Connection, user_id: i64) -> Result<bool> {
    conn.execute(
        "UPDATE user_roles SET assigned_by_user_id = NULL WHERE assigned_by_user_id = ?1",
        [user_id],
    )?;
    let deleted = conn.execute("DELETE FROM users WHERE id = ?1", [user_id])?;
    Ok(deleted > 0)
}

/// Looks a role up by slug.
pub fn get_role_by_slug(conn: &Connection, slug: &str) -> Result<Option<RoleRecord>> {
    Ok(database::list_roles(conn)?
        .into_iter()
        .find(|role| role.slug == slug))
}

/// True when `user_id` currently holds the role `slug`.
pub fn user_has_role(conn: &Connection, user_id: i64, slug: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM user_roles ur JOIN roles r ON r.id = ur.role_id
         WHERE ur.user_id = ?1 AND r.slug = ?2",
        params![user_id, slug],
        |row| row.get(0),
    )?;
    Ok(n > 0)
}

/// Number of users (active or not) holding the role `slug`.
pub fn count_role_holders(conn: &Connection, slug: &str) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM user_roles ur JOIN roles r ON r.id = ur.role_id
         WHERE r.slug = ?1",
        [slug],
        |row| row.get(0),
    )?)
}

/// Renames a custom role (the slug is immutable). Returns None for an unknown slug;
/// callers reject built-in roles before calling.
pub fn rename_role(conn: &Connection, slug: &str, name: &str) -> Result<Option<RoleRecord>> {
    let n = conn.execute(
        "UPDATE roles SET name = ?1 WHERE slug = ?2 AND is_builtin = 0",
        params![name, slug],
    )?;
    if n == 0 {
        return Ok(None);
    }
    get_role_by_slug(conn, slug)
}

/// Deletes a custom role in one transaction: drops its `user_roles` rows, invalidates
/// the sessions of every holder (their cached `role_bits` are stale), clears its bit
/// from `instance_settings.reorder_children_role_bits`, then deletes the role.
/// Returns None for an unknown or built-in slug.
pub fn delete_custom_role(conn: &mut Connection, slug: &str) -> Result<Option<RoleDeletion>> {
    let tx = conn.transaction()?;
    let role: Option<(i64, i64)> = tx
        .query_row(
            "SELECT id, bit_position FROM roles WHERE slug = ?1 AND is_builtin = 0",
            [slug],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((role_id, bit_position)) = role else {
        return Ok(None);
    };
    let holders: Vec<i64> = tx
        .prepare("SELECT user_id FROM user_roles WHERE role_id = ?1")?
        .query_map([role_id], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    for user_id in &holders {
        database::invalidate_user_sessions(&tx, *user_id)?;
    }
    tx.execute("DELETE FROM user_roles WHERE role_id = ?1", [role_id])?;

    let bit = 1i64 << bit_position;
    let mask: i64 = tx
        .query_row(
            "SELECT reorder_children_role_bits FROM instance_settings WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0);
    let reorder_mask_cleared = mask & bit != 0;
    if reorder_mask_cleared {
        tx.execute(
            "UPDATE instance_settings SET reorder_children_role_bits = ?1 WHERE id = 1",
            [mask & !bit],
        )?;
    }
    tx.execute("DELETE FROM roles WHERE id = ?1", [role_id])?;
    tx.commit()?;
    Ok(Some(RoleDeletion {
        users_affected: holders.len(),
        reorder_mask_cleared,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        database::initialize_auth_schema(&conn).unwrap();
        conn
    }

    fn new_user(conn: &Connection, name: &str, creator: i64) -> i64 {
        let uid = database::create_user(conn, name, None, "dummy", creator).unwrap();
        database::get_user_id_by_uid(conn, &uid).unwrap().unwrap()
    }

    #[test]
    fn delete_user_detaches_granted_roles_and_cascades() {
        let mut conn = auth_conn();
        let owner = database::create_owner(&conn, "owner", "pw").unwrap();
        let grantor = new_user(&conn, "grantor", owner);
        let grantee = new_user(&conn, "grantee", owner);
        database::assign_role(&conn, grantor, "admin", owner).unwrap();
        database::create_custom_role(&conn, "editors", "Editors").unwrap();
        database::assign_role(&conn, grantee, "editors", grantor).unwrap();
        database::create_session(&conn, grantor, 3, None).unwrap();

        assert!(delete_user(&mut conn, grantor).unwrap());
        assert!(database::get_user_uid(&conn, grantor).unwrap().is_none());
        let sessions: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions WHERE user_id = ?1", [grantor], |r| r.get(0))
            .unwrap();
        assert_eq!(sessions, 0);
        // The grantee keeps the role; only the attribution is cleared.
        assert!(user_has_role(&conn, grantee, "editors").unwrap());
        let by: Option<i64> = conn
            .query_row(
                "SELECT assigned_by_user_id FROM user_roles ur JOIN roles r ON r.id = ur.role_id
                 WHERE ur.user_id = ?1 AND r.slug = 'editors'",
                [grantee],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(by, None);
        assert!(!delete_user(&mut conn, grantor).unwrap());
    }

    #[test]
    fn rename_role_only_touches_custom_roles() {
        let conn = auth_conn();
        database::create_custom_role(&conn, "editors", "Editors").unwrap();
        let renamed = rename_role(&conn, "editors", "Chief Editors").unwrap().unwrap();
        assert_eq!(renamed.slug, "editors");
        assert_eq!(renamed.name, "Chief Editors");
        assert!(rename_role(&conn, "admin", "Boss").unwrap().is_none());
        assert!(rename_role(&conn, "nope", "x").unwrap().is_none());
        assert_eq!(get_role_by_slug(&conn, "admin").unwrap().unwrap().name, "Admin");
    }

    #[test]
    fn delete_custom_role_cleans_holders_sessions_and_mask() {
        let mut conn = auth_conn();
        let owner = database::create_owner(&conn, "owner", "pw").unwrap();
        let role = database::create_custom_role(&conn, "editors", "Editors").unwrap();
        let holder = new_user(&conn, "holder", owner);
        database::assign_role(&conn, holder, "editors", owner).unwrap();
        database::create_session(&conn, holder, 2, None).unwrap();
        let bit = 1u32 << role.bit_position;
        let mut settings = database::get_instance_settings(&conn).unwrap();
        settings.reorder_children_role_bits |= bit;
        database::update_instance_settings(&conn, &settings).unwrap();

        let out = delete_custom_role(&mut conn, "editors").unwrap().unwrap();
        assert_eq!(out, RoleDeletion { users_affected: 1, reorder_mask_cleared: true });
        assert!(get_role_by_slug(&conn, "editors").unwrap().is_none());
        assert_eq!(database::compute_role_bits(&conn, holder).unwrap() & bit, 0);
        let sessions: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions WHERE user_id = ?1", [holder], |r| r.get(0))
            .unwrap();
        assert_eq!(sessions, 0);
        let mask = database::get_instance_settings(&conn).unwrap().reorder_children_role_bits;
        assert_eq!(mask & bit, 0);
        assert_eq!(mask, database::DEFAULT_REORDER_CHILDREN_ROLE_BITS);

        assert!(delete_custom_role(&mut conn, "editors").unwrap().is_none());
        assert!(delete_custom_role(&mut conn, "admin").unwrap().is_none());
    }
}
