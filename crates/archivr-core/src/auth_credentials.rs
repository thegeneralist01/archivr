//! Auth-DB credential helpers: session listing/revocation by handle, API token
//! revocation, password resets, role slugs and the throttled token touch.
//!
//! `session_uid` is the value of the `session` cookie, so nothing in this
//! module hands it back to callers; sessions are addressed by a stable,
//! non-reversible `session_handle` (first 16 hex chars of its SHA3-256 hash).
use anyhow::Result;
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};

use crate::hash::hash_bytes;

/// Length of a `session_handle` in hex characters.
pub const SESSION_HANDLE_LEN: usize = 16;

/// Public view of a session. Never carries `session_uid`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionInfo {
    pub session_handle: String,
    pub created_at: String,
    pub last_seen_at: String,
    pub expires_at: String,
    pub user_agent: Option<String>,
}

/// Stable handle for a session: first 16 hex chars of `hash(session_uid)`.
pub fn session_handle(session_uid: &str) -> String {
    hash_bytes(session_uid.as_bytes())[..SESSION_HANDLE_LEN].to_string()
}

/// Lists a user's unexpired sessions, newest `last_seen_at` first.
pub fn list_user_sessions(conn: &Connection, user_id: i64) -> Result<Vec<SessionInfo>> {
    let now = Utc::now().to_rfc3339();
    let mut stmt = conn.prepare(
        "SELECT session_uid, created_at, last_seen_at, expires_at, user_agent
         FROM sessions WHERE user_id = ?1 AND expires_at > ?2
         ORDER BY last_seen_at DESC, id DESC",
    )?;
    let rows = stmt
        .query_map(params![user_id, now], |row| {
            let session_uid: String = row.get(0)?;
            Ok(SessionInfo {
                session_handle: session_handle(&session_uid),
                created_at: row.get(1)?,
                last_seen_at: row.get(2)?,
                expires_at: row.get(3)?,
                user_agent: row.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Deletes the user's session with the given handle. Returns false when the
/// handle matches none of that user's sessions.
pub fn delete_user_session_by_handle(
    conn: &Connection,
    user_id: i64,
    handle: &str,
) -> Result<bool> {
    let mut stmt = conn.prepare("SELECT session_uid FROM sessions WHERE user_id = ?1")?;
    let uids = stmt
        .query_map([user_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for uid in uids {
        if session_handle(&uid) == handle {
            conn.execute("DELETE FROM sessions WHERE session_uid = ?1", [&uid])?;
            return Ok(true);
        }
    }
    Ok(false)
}

/// Deletes every session of the user except `keep_session_uid` (all of them
/// when `None`). Returns the number of sessions deleted.
pub fn delete_other_sessions(
    conn: &Connection,
    user_id: i64,
    keep_session_uid: Option<&str>,
) -> Result<usize> {
    let n = match keep_session_uid {
        Some(keep) => conn.execute(
            "DELETE FROM sessions WHERE user_id = ?1 AND session_uid != ?2",
            params![user_id, keep],
        )?,
        None => conn.execute("DELETE FROM sessions WHERE user_id = ?1", [user_id])?,
    };
    Ok(n)
}

/// Deletes all API tokens of a user. Returns the number deleted.
pub fn delete_user_tokens(conn: &Connection, user_id: i64) -> Result<usize> {
    Ok(conn.execute("DELETE FROM api_tokens WHERE user_id = ?1", [user_id])?)
}

/// Admin password reset: sets the new hash, deletes all of the user's sessions
/// and, if asked, all their API tokens, in one transaction.
/// Returns `(sessions_revoked, tokens_revoked)`.
pub fn reset_user_password(
    conn: &Connection,
    user_id: i64,
    new_hash: &str,
    revoke_tokens: bool,
) -> Result<(usize, usize)> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE users SET password_hash = ?1 WHERE id = ?2",
        params![new_hash, user_id],
    )?;
    let sessions = delete_other_sessions(&tx, user_id, None)?;
    let tokens = if revoke_tokens {
        delete_user_tokens(&tx, user_id)?
    } else {
        0
    };
    tx.commit()?;
    Ok((sessions, tokens))
}

/// Role slugs held by the user, lowest bit first. A user with no role rows is
/// reported as `["guest"]` (the guest bit is always part of their role bits).
pub fn list_user_role_slugs(conn: &Connection, user_id: i64) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT r.slug FROM user_roles ur JOIN roles r ON r.id = ur.role_id
         WHERE ur.user_id = ?1 ORDER BY r.bit_position ASC",
    )?;
    let mut slugs = stmt
        .query_map([user_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if slugs.is_empty() {
        slugs.push("guest".to_string());
    }
    Ok(slugs)
}

/// Scope (`full` | `read`) of a valid token: unexpired and its user active.
pub fn token_scope_for_hash(conn: &Connection, token_hash: &str) -> Result<Option<String>> {
    let now = Utc::now().to_rfc3339();
    conn.query_row(
        "SELECT t.scope FROM api_tokens t
         JOIN users u ON u.id = t.user_id
         WHERE t.token_hash = ?1
           AND u.status = 'active'
           AND (t.expires_at IS NULL OR t.expires_at > ?2)",
        params![token_hash, now],
        |row| row.get(0),
    )
    .optional()
    .map_err(Into::into)
}

/// Sets `last_used_at` for a token unless it was already set within the last
/// `min_interval_secs`. Returns true when a write happened.
pub fn touch_token_throttled(
    conn: &Connection,
    token_uid: &str,
    min_interval_secs: i64,
) -> Result<bool> {
    let now = Utc::now();
    let cutoff = (now - chrono::Duration::seconds(min_interval_secs)).to_rfc3339();
    let n = conn.execute(
        "UPDATE api_tokens SET last_used_at = ?1
         WHERE token_uid = ?2
           AND (last_used_at IS NULL OR julianday(last_used_at) <= julianday(?3))",
        params![now.to_rfc3339(), token_uid, cutoff],
    )?;
    Ok(n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database;

    fn setup() -> (Connection, i64, i64) {
        let conn = Connection::open_in_memory().unwrap();
        database::initialize_auth_schema(&conn).unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        let owner = database::create_owner(&conn, "owner", "pw").unwrap();
        let uid = database::create_user(&conn, "bob", None, "pw", owner).unwrap();
        let bob = database::get_user_id_by_uid(&conn, &uid).unwrap().unwrap();
        (conn, owner, bob)
    }

    #[test]
    fn handle_is_sixteen_hex_and_stable_and_not_the_uid() {
        let h = session_handle("sess_abc");
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(h, session_handle("sess_abc"));
        assert_ne!(h, session_handle("sess_abd"));
    }

    #[test]
    fn list_and_delete_sessions_by_handle_are_scoped_to_user() {
        let (conn, owner, bob) = setup();
        let s1 = database::create_session(&conn, bob, 2, Some("ua")).unwrap();
        let s2 = database::create_session(&conn, bob, 2, None).unwrap();
        let other = database::create_session(&conn, owner, 8, None).unwrap();

        let listed = list_user_sessions(&conn, bob).unwrap();
        assert_eq!(listed.len(), 2);
        let json = serde_json::to_string(&listed).unwrap();
        assert!(!json.contains(&s1) && !json.contains(&s2));

        // Another user's handle is not deletable through bob.
        assert!(!delete_user_session_by_handle(&conn, bob, &session_handle(&other)).unwrap());
        assert!(database::get_session(&conn, &other).unwrap().is_some());

        assert!(delete_user_session_by_handle(&conn, bob, &session_handle(&s1)).unwrap());
        assert!(database::get_session(&conn, &s1).unwrap().is_none());
        assert!(database::get_session(&conn, &s2).unwrap().is_some());
    }

    #[test]
    fn delete_other_sessions_keeps_only_the_given_one() {
        let (conn, _, bob) = setup();
        let keep = database::create_session(&conn, bob, 2, None).unwrap();
        database::create_session(&conn, bob, 2, None).unwrap();
        database::create_session(&conn, bob, 2, None).unwrap();
        assert_eq!(delete_other_sessions(&conn, bob, Some(&keep)).unwrap(), 2);
        assert_eq!(list_user_sessions(&conn, bob).unwrap().len(), 1);
        assert_eq!(delete_other_sessions(&conn, bob, None).unwrap(), 1);
        assert!(list_user_sessions(&conn, bob).unwrap().is_empty());
    }

    #[test]
    fn reset_password_clears_sessions_and_optionally_tokens() {
        let (conn, _, bob) = setup();
        database::create_session(&conn, bob, 2, None).unwrap();
        database::create_api_token(&conn, bob, "h1", "t", None, "full").unwrap();
        let (s, t) = reset_user_password(&conn, bob, "newhash", false).unwrap();
        assert_eq!((s, t), (1, 0));
        assert_eq!(
            database::get_user_password_hash(&conn, bob).unwrap().as_deref(),
            Some("newhash")
        );
        database::create_session(&conn, bob, 2, None).unwrap();
        let (s, t) = reset_user_password(&conn, bob, "newhash2", true).unwrap();
        assert_eq!((s, t), (1, 1));
        assert!(database::list_user_tokens(&conn, bob).unwrap().is_empty());
    }

    #[test]
    fn touch_token_is_throttled() {
        let (conn, _, bob) = setup();
        let tok = database::create_api_token(&conn, bob, "h", "t", None, "full").unwrap();
        assert!(touch_token_throttled(&conn, &tok, 60).unwrap());
        assert!(!touch_token_throttled(&conn, &tok, 60).unwrap());
        // Back-date last use beyond the interval: touched again.
        let old = (Utc::now() - chrono::Duration::seconds(120)).to_rfc3339();
        conn.execute(
            "UPDATE api_tokens SET last_used_at = ?1 WHERE token_uid = ?2",
            params![old, tok],
        )
        .unwrap();
        assert!(touch_token_throttled(&conn, &tok, 60).unwrap());
    }

    #[test]
    fn scope_lookup_honours_expiry() {
        let (conn, _, bob) = setup();
        database::create_api_token(&conn, bob, "hr", "t", None, "read").unwrap();
        let past = (Utc::now() - chrono::Duration::days(1)).to_rfc3339();
        database::create_api_token(&conn, bob, "hx", "t", Some(&past), "read").unwrap();
        assert_eq!(token_scope_for_hash(&conn, "hr").unwrap().as_deref(), Some("read"));
        assert_eq!(token_scope_for_hash(&conn, "hx").unwrap(), None);
        assert_eq!(token_scope_for_hash(&conn, "nope").unwrap(), None);
    }

    #[test]
    fn role_slugs_follow_bit_order() {
        let (conn, owner, bob) = setup();
        assert_eq!(list_user_role_slugs(&conn, owner).unwrap(), ["user", "admin", "owner"]);
        assert_eq!(list_user_role_slugs(&conn, bob).unwrap(), ["user"]);
        conn.execute("DELETE FROM user_roles WHERE user_id = ?1", [bob]).unwrap();
        assert_eq!(list_user_role_slugs(&conn, bob).unwrap(), ["guest"]);
    }
}
