//! Server administration, for the operator (server admins only): accounts, suspension,
//! per-account quotas, admin role, usage.

use axum::Json;
use axum::extract::{Path, State};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;
use vessel_protocol::*;

use crate::auth::{Auth, require_server_admin};
use crate::db::{now, uuid_from};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub async fn list_users(
    State(state): State<AppState>,
    auth: Auth,
) -> AppResult<Json<Vec<AdminUser>>> {
    let users = state
        .db(move |conn| {
            require_server_admin(conn, &auth.user_id)?;
            let mut stmt = conn.prepare(
                "SELECT u.id, u.email, u.created_at, u.is_admin, u.suspended_at IS NOT NULL, u.quota_bytes,
                    (SELECT MAX(last_seen_at) FROM devices d WHERE d.user_id = u.id),
                    (SELECT COUNT(*) FROM devices d WHERE d.user_id = u.id AND d.revoked_at IS NULL),
                    (SELECT COUNT(*) FROM vaults v WHERE v.owner_id = u.id),
                    (SELECT COALESCE(SUM(bytes), 0) FROM vaults v WHERE v.owner_id = u.id)
                 FROM users u WHERE u.deleted_at IS NULL ORDER BY u.created_at",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    AdminUser {
                        user_id: Uuid::nil(),
                        email: r.get(1)?,
                        created_at: r.get(2)?,
                        is_admin: r.get(3)?,
                        suspended: r.get(4)?,
                        quota_bytes: r.get::<_, Option<i64>>(5)?.map(|q| q.max(0) as u64),
                        last_seen_at: r.get(6)?,
                        devices: r.get::<_, i64>(7)? as u32,
                        vaults_owned: r.get::<_, i64>(8)? as u32,
                        bytes_used: r.get::<_, i64>(9)?.max(0) as u64,
                    },
                ))
            })?;
            rows.map(|row| {
                let (id, mut user) = row?;
                user.user_id = uuid_from(id)?;
                Ok(user)
            })
            .collect::<AppResult<Vec<_>>>()
        })
        .await?;
    Ok(Json(users))
}

pub async fn update_user(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(req): Json<AdminUserUpdate>,
) -> AppResult<Json<()>> {
    state
        .db(move |conn| {
            require_server_admin(conn, &auth.user_id)?;
            let tx = conn.transaction()?;
            let key = id.as_bytes().as_slice();
            let exists: Option<bool> = tx
                .query_row("SELECT is_admin FROM users WHERE id = ?1 AND deleted_at IS NULL", [key], |r| r.get(0))
                .optional()?;
            let Some(was_admin) = exists else {
                return Err(AppError::not_found("no such account"));
            };
            if let Some(suspend) = req.suspended {
                if suspend && id == auth.user_id {
                    return Err(AppError::bad_request("you cannot suspend your own account"));
                }
                tx.execute(
                    "UPDATE users SET suspended_at = ?1 WHERE id = ?2",
                    params![suspend.then(now), key],
                )?;
                if suspend {
                    // Signed out now; open sockets close at their next session check.
                    tx.execute("DELETE FROM sessions WHERE user_id = ?1", [key])?;
                }
            }
            if let Some(admin) = req.is_admin {
                if !admin && was_admin {
                    let others: i64 = tx.query_row(
                        "SELECT COUNT(*) FROM users WHERE is_admin = 1 AND deleted_at IS NULL AND id != ?1",
                        [key],
                        |r| r.get(0),
                    )?;
                    if others == 0 {
                        return Err(AppError::conflict("the server needs at least one admin"));
                    }
                }
                tx.execute("UPDATE users SET is_admin = ?1 WHERE id = ?2", params![admin, key])?;
            }
            if let Some(quota) = req.quota_bytes {
                tx.execute(
                    "UPDATE users SET quota_bytes = ?1 WHERE id = ?2",
                    params![quota.map(|q| q.min(i64::MAX as u64) as i64), key],
                )?;
            }
            tx.commit()?;
            tracing::info!(admin = %auth.user_id, user = %id, "account updated by the operator");
            Ok(())
        })
        .await?;
    Ok(Json(()))
}

/// Deletes an account (an abuse case, or a deletion request made to the operator). Its
/// memberships in other people's vaults stay in their signed histories until a manager removes
/// the account.
pub async fn delete_user(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> AppResult<Json<()>> {
    if id == auth.user_id {
        return Err(AppError::bad_request("delete your own account from your account settings"));
    }
    let removed = state
        .db(move |conn| {
            require_server_admin(conn, &auth.user_id)?;
            let tx = conn.transaction()?;
            let found: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM users WHERE id = ?1 AND deleted_at IS NULL)",
                [id.as_bytes().as_slice()],
                |r| r.get(0),
            )?;
            if !found {
                return Err(AppError::not_found("no such account"));
            }
            let removed = crate::accounts::delete_account(&tx, &id, false)?;
            tx.commit()?;
            tracing::info!(admin = %auth.user_id, user = %id, "account deleted by the operator");
            Ok(removed)
        })
        .await?;
    for (vault_id, members) in removed {
        state.notify(members, ServerEvent::VaultRemoved { vault_id });
    }
    Ok(Json(()))
}

pub async fn stats(State(state): State<AppState>, auth: Auth) -> AppResult<Json<ServerStats>> {
    let default_quota_bytes = state.config.quota_bytes;
    let stats = state
        .db(move |conn| {
            require_server_admin(conn, &auth.user_id)?;
            let month_ago = now() - 30 * 24 * 3600;
            let count = |sql: &str| -> AppResult<u64> {
                Ok(conn.query_row(sql, [], |r| r.get::<_, i64>(0))?.max(0) as u64)
            };
            Ok(ServerStats {
                users: count("SELECT COUNT(*) FROM users WHERE deleted_at IS NULL")?,
                active_users_30d: conn.query_row(
                    "SELECT COUNT(DISTINCT user_id) FROM devices WHERE last_seen_at > ?1",
                    [month_ago],
                    |r| r.get::<_, i64>(0),
                )?.max(0) as u64,
                suspended_users: count("SELECT COUNT(*) FROM users WHERE suspended_at IS NOT NULL AND deleted_at IS NULL")?,
                vaults: count("SELECT COUNT(*) FROM vaults")?,
                records: count("SELECT COUNT(*) FROM records")?,
                bytes: count("SELECT COALESCE(SUM(bytes), 0) FROM vaults")?,
                sessions: count("SELECT COUNT(*) FROM sessions")?,
                default_quota_bytes,
            })
        })
        .await?;
    Ok(Json(stats))
}
