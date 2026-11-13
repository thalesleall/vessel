//! Device sessions: opaque bearer tokens, stored as SHA-256 hashes, refreshed with a signature
//! from the device key.

use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use vessel_crypto::secret::random_bytes;
use vessel_protocol::NewDevice;

use crate::db::{self, now, uuid_from};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub const ACCESS_TTL_SECS: i64 = 15 * 60;
pub const REFRESH_TTL_SECS: i64 = 30 * 24 * 3600;
/// Active devices per account. Every login creates one; past the cap the least recently seen
/// are signed out, so one account cannot grow the table without bound.
pub const MAX_DEVICES: i64 = 50;

pub fn new_token() -> AppResult<String> {
    let bytes = random_bytes::<32>().map_err(AppError::internal)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

pub fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

pub struct Tokens {
    pub access: String,
    pub access_expires: i64,
    pub refresh: String,
}

pub fn create_device(conn: &Connection, user_id: &Uuid, device: &NewDevice) -> AppResult<Uuid> {
    if device.public_key.0.len() != 32 {
        return Err(AppError::bad_request("device public key must be 32 bytes"));
    }
    let name: String = device.name.trim().chars().take(80).collect();
    let id = Uuid::now_v7();
    let t = now();
    conn.execute(
        "INSERT INTO devices (id, user_id, name, public_key, created_at, last_seen_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
        params![id.as_bytes().as_slice(), user_id.as_bytes().as_slice(), name, device.public_key.as_slice(), t],
    )?;
    let user = user_id.as_bytes().as_slice();
    conn.execute(
        "UPDATE devices SET revoked_at = ?1 WHERE id IN (
             SELECT id FROM devices WHERE user_id = ?2 AND revoked_at IS NULL
             ORDER BY last_seen_at DESC, created_at DESC LIMIT -1 OFFSET ?3)",
        params![t, user, MAX_DEVICES],
    )?;
    conn.execute(
        "DELETE FROM sessions WHERE user_id = ?1 AND device_id IN (SELECT id FROM devices WHERE user_id = ?1 AND revoked_at IS NOT NULL)",
        [user],
    )?;
    Ok(id)
}

pub fn create_session(conn: &Connection, user_id: &Uuid, device_id: &Uuid) -> AppResult<Tokens> {
    let tokens = Tokens {
        access: new_token()?,
        access_expires: now() + ACCESS_TTL_SECS,
        refresh: new_token()?,
    };
    conn.execute(
        "INSERT INTO sessions (id, device_id, user_id, access_hash, access_expires, refresh_hash, refresh_expires)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            Uuid::now_v7().as_bytes().as_slice(),
            device_id.as_bytes().as_slice(),
            user_id.as_bytes().as_slice(),
            hash_token(&tokens.access),
            tokens.access_expires,
            hash_token(&tokens.refresh),
            now() + REFRESH_TTL_SECS,
        ],
    )?;
    Ok(tokens)
}

/// Replaces both tokens of an existing session (refresh-token rotation).
pub fn rotate_session(conn: &Connection, session_id: &[u8]) -> AppResult<Tokens> {
    let tokens = Tokens {
        access: new_token()?,
        access_expires: now() + ACCESS_TTL_SECS,
        refresh: new_token()?,
    };
    conn.execute(
        "UPDATE sessions SET prev_refresh_hash = refresh_hash, access_hash = ?1, access_expires = ?2, refresh_hash = ?3, refresh_expires = ?4 WHERE id = ?5",
        params![
            hash_token(&tokens.access),
            tokens.access_expires,
            hash_token(&tokens.refresh),
            now() + REFRESH_TTL_SECS,
            session_id,
        ],
    )?;
    Ok(tokens)
}

/// An authenticated request.
#[derive(Clone, Copy, Debug)]
pub struct Auth {
    pub user_id: Uuid,
    pub device_id: Uuid,
}

/// Resolves an access token to its session, if valid and not revoked.
pub fn authenticate(conn: &Connection, token: &str) -> AppResult<Option<Auth>> {
    let row: Option<(Vec<u8>, Vec<u8>, i64, Option<i64>)> = conn
        .query_row(
            "SELECT s.user_id, s.device_id, s.access_expires, d.revoked_at
             FROM sessions s JOIN devices d ON d.id = s.device_id WHERE s.access_hash = ?1",
            [hash_token(token)],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((user, device, expires, revoked)) = row else {
        return Ok(None);
    };
    if expires < now() || revoked.is_some() {
        return Ok(None);
    }
    let device_id = uuid_from(device)?;
    conn.execute(
        "UPDATE devices SET last_seen_at = ?1 WHERE id = ?2 AND last_seen_at < ?1 - 60",
        params![now(), device_id.as_bytes().as_slice()],
    )?;
    Ok(Some(Auth {
        user_id: uuid_from(user)?,
        device_id,
    }))
}

pub fn bearer(parts: &Parts) -> Option<String> {
    let value = parts.headers.get(AUTHORIZATION)?.to_str().ok()?;
    value.strip_prefix("Bearer ").map(|t| t.trim().to_string())
}

impl FromRequestParts<AppState> for Auth {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = bearer(parts).ok_or_else(AppError::unauthorized)?;
        state
            .db(move |conn| authenticate(conn, &token))
            .await?
            .ok_or_else(AppError::unauthorized)
    }
}

/// Server admins manage sign-up invites.
pub fn require_server_admin(conn: &Connection, user: &Uuid) -> AppResult<()> {
    match db::user_by_id(conn, user)? {
        Some(u) if u.is_admin => Ok(()),
        _ => Err(AppError::forbidden("server admin only")),
    }
}
