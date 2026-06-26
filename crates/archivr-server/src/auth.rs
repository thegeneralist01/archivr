use anyhow::Result;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use argon2::password_hash::{SaltString, rand_core::OsRng};
use axum::{async_trait, extract::FromRequestParts, http::request::Parts};
use axum_extra::extract::CookieJar;
use rand::RngCore;

use crate::routes::{ApiError, AppState};
use archivr_core::database;

// Role bit constants
pub const ROLE_GUEST: u32 = 1;  // bit 0
pub const ROLE_USER:  u32 = 2;  // bit 1
pub const ROLE_ADMIN: u32 = 4;  // bit 2
pub const ROLE_OWNER: u32 = 8;  // bit 3

#[derive(Clone, Debug)]
pub enum AuthUser {
    Guest,
    Authenticated { user_id: i64, role_bits: u32 },
}

impl AuthUser {
    pub fn require_auth(&self) -> Result<(i64, u32), ApiError> {
        match self {
            AuthUser::Authenticated { user_id, role_bits } => Ok((*user_id, *role_bits)),
            AuthUser::Guest => Err(ApiError::unauthorized("login required")),
        }
    }

    pub fn require_role(&self, bit: u32) -> Result<(), ApiError> {
        match self {
            AuthUser::Authenticated { role_bits, .. } if role_bits & bit != 0 => Ok(()),
            AuthUser::Authenticated { .. } => Err(ApiError::forbidden("insufficient permissions")),
            AuthUser::Guest => Err(ApiError::unauthorized("login required")),
        }
    }

    pub fn has_role(&self, bit: u32) -> bool {
        matches!(self, AuthUser::Authenticated { role_bits, .. } if role_bits & bit != 0)
    }
}

#[async_trait]
impl FromRequestParts<AppState> for AuthUser {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, std::convert::Infallible> {
        let auth_db_path = state.auth_db_path.as_ref();

        // 1. Try session cookie
        let jar = CookieJar::from_headers(&parts.headers);
        if let Some(cookie) = jar.get("session") {
            let session_uid = cookie.value().to_string();
            if let Ok(conn) = database::open_auth_db(auth_db_path) {
                if let Ok(Some(session)) = database::get_session(&conn, &session_uid) {
                    // Conditional touch: only update if >60s since last_seen_at
                    let should_touch = chrono::DateTime::parse_from_rfc3339(&session.last_seen_at)
                        .map(|last| {
                            chrono::Utc::now() - last.with_timezone(&chrono::Utc)
                                > chrono::Duration::seconds(60)
                        })
                        .unwrap_or(true);
                    if should_touch {
                        let _ = database::touch_session(&conn, &session_uid);
                    }
                    return Ok(AuthUser::Authenticated {
                        user_id: session.user_id,
                        role_bits: session.role_bits,
                    });
                }
            }
        }

        // 2. Try Bearer token
        if let Some(auth_header) = parts.headers.get("Authorization") {
            if let Ok(header_str) = auth_header.to_str() {
                if let Some(raw_token) = header_str.strip_prefix("Bearer ") {
                    let token_hash = hash_token(raw_token);
                    if let Ok(conn) = database::open_auth_db(auth_db_path) {
                        if let Ok(Some(user_id)) = database::get_user_for_token(&conn, &token_hash) {
                            if let Ok(role_bits) = database::compute_role_bits(&conn, user_id) {
                                return Ok(AuthUser::Authenticated { user_id, role_bits });
                            }
                        }
                    }
                }
            }
        }

        Ok(AuthUser::Guest)
    }
}

// Password helpers

pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("password hashing failed: {e}"))?
        .to_string();
    Ok(hash)
}

pub fn verify_password(password: &str, hash: &str) -> Result<bool> {
    let parsed = PasswordHash::new(hash)
        .map_err(|e| anyhow::anyhow!("invalid password hash: {e}"))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

// Token helpers

pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, bytes)
}

pub fn hash_token(raw: &str) -> String {
    archivr_core::hash::hash_bytes(raw.as_bytes())
}
