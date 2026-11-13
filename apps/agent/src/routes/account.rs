use axum::Json;
use axum::extract::{Path, State};
use rusqlite::params;
use uuid::Uuid;
use vessel_protocol::*;

use crate::auth::Auth;
use crate::db::{self, now, user_by_id, uuid_from};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub async fn server_info(State(state): State<AppState>) -> AppResult<Json<ServerInfo>> {
    let first_user = state.db(|conn| Ok(db::user_count(conn)? == 0)).await?;
    Ok(Json(ServerInfo {
        name: state.config.name.clone(),
        version: env!("CARGO_PKG_VERSION").into(),
        api_version: API_VERSION,
        server_id: state.config.server_id(),
        signups: state.config.signups,
        first_user,
    }))
}

pub async fn get_account(
    State(state): State<AppState>,
    auth: Auth,
) -> AppResult<Json<AccountResponse>> {
    let user = state
        .db(move |conn| user_by_id(conn, &auth.user_id))
        .await?
        .ok_or_else(AppError::unauthorized)?;
    Ok(Json(AccountResponse {
        user_id: user.id,
        email: user.email,
        is_server_admin: user.is_admin,
        account: user.account,
        kdf: user.kdf,
    }))
}

pub async fn list_devices(
    State(state): State<AppState>,
    auth: Auth,
) -> AppResult<Json<Vec<DeviceInfo>>> {
    let devices = state
        .db(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, name, created_at, last_seen_at FROM devices
                 WHERE user_id = ?1 AND revoked_at IS NULL ORDER BY last_seen_at DESC",
            )?;
            let rows = stmt.query_map([auth.user_id.as_bytes().as_slice()], |r| {
                Ok((r.get::<_, Vec<u8>>(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?;
            rows.map(|row| {
                let (id, name, created_at, last_seen_at) = row?;
                let id = uuid_from(id)?;
                Ok(DeviceInfo {
                    id,
                    name,
                    created_at,
                    last_seen_at,
                    current: id == auth.device_id,
                })
            })
            .collect::<AppResult<Vec<_>>>()
        })
        .await?;
    Ok(Json(devices))
}

pub async fn revoke_device(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> AppResult<Json<()>> {
    let revoked = state
        .db(move |conn| {
            // Only the caller's own devices: the sessions go only if the device was theirs.
            let tx = conn.transaction()?;
            let n = tx.execute(
                "UPDATE devices SET revoked_at = ?1 WHERE id = ?2 AND user_id = ?3 AND revoked_at IS NULL",
                params![now(), id.as_bytes().as_slice(), auth.user_id.as_bytes().as_slice()],
            )?;
            if n > 0 {
                tx.execute(
                    "DELETE FROM sessions WHERE device_id = ?1 AND user_id = ?2",
                    params![id.as_bytes().as_slice(), auth.user_id.as_bytes().as_slice()],
                )?;
            }
            tx.commit()?;
            Ok(n > 0)
        })
        .await?;
    if !revoked {
        return Err(AppError::not_found("device not found"));
    }
    state.notify(
        vec![auth.user_id],
        ServerEvent::DeviceRevoked { device_id: id },
    );
    Ok(Json(()))
}
