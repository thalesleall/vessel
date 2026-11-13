//! Pull and push of encrypted records (`docs/ARCHITECTURE.md` §9).

use axum::Json;
use axum::extract::{Path, Query, State};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use uuid::Uuid;
use vessel_protocol::*;

use super::vaults::vault_for_member;
use crate::auth::Auth;
use crate::db::{now, uuid_from, vault_members};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct PullQuery {
    #[serde(default)]
    since: u64,
    limit: Option<usize>,
}

fn record_from_row(
    r: &rusqlite::Row<'_>,
) -> rusqlite::Result<(Vec<u8>, i64, i64, u32, bool, Vec<u8>, i64, Vec<u8>)> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
        r.get(7)?,
    ))
}

type RawRecord = (Vec<u8>, i64, i64, u32, bool, Vec<u8>, i64, Vec<u8>);

fn to_dto(raw: RawRecord) -> AppResult<RecordDto> {
    let (id, version, seq, key_version, deleted, blob, updated_at, device) = raw;
    Ok(RecordDto {
        id: uuid_from(id)?,
        version: version as u64,
        seq: seq as u64,
        key_version,
        deleted,
        blob: Bytes(blob),
        updated_at,
        updated_by_device: uuid_from(device)?,
    })
}

const RECORD_COLUMNS: &str =
    "id, version, seq, key_version, deleted, blob, updated_at, updated_by_device";

fn current_record(conn: &Connection, vault: &Uuid, id: &Uuid) -> AppResult<Option<RecordDto>> {
    conn.query_row(
        &format!("SELECT {RECORD_COLUMNS} FROM records WHERE vault_id = ?1 AND id = ?2"),
        params![vault.as_bytes().as_slice(), id.as_bytes().as_slice()],
        record_from_row,
    )
    .optional()?
    .map(to_dto)
    .transpose()
}

pub async fn pull(
    State(state): State<AppState>,
    auth: Auth,
    Path(vault): Path<Uuid>,
    Query(q): Query<PullQuery>,
) -> AppResult<Json<ChangesPage>> {
    let limit = q.limit.unwrap_or(MAX_BATCH).clamp(1, MAX_BATCH);
    let page = state
        .db_read(move |conn| {
            let (row, _) = vault_for_member(conn, &vault, &auth.user_id)?;
            let mut stmt = conn.prepare(&format!(
                "SELECT {RECORD_COLUMNS} FROM records WHERE vault_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3"
            ))?;
            let changes = stmt
                .query_map(params![vault.as_bytes().as_slice(), q.since as i64, (limit + 1) as i64], record_from_row)?
                .map(|r| to_dto(r?))
                .collect::<AppResult<Vec<_>>>()?;
            let has_more = changes.len() > limit;
            let changes: Vec<_> = changes.into_iter().take(limit).collect();
            let next_seq = if has_more { changes.last().map_or(q.since, |c| c.seq) } else { row.last_seq.max(q.since) };
            Ok(ChangesPage { changes, next_seq, has_more })
        })
        .await?;
    Ok(Json(page))
}

pub async fn push(
    State(state): State<AppState>,
    auth: Auth,
    Path(vault): Path<Uuid>,
    Json(req): Json<PushRequest>,
) -> AppResult<Json<PushResponse>> {
    if req.ops.len() > MAX_BATCH {
        return Err(AppError::new(
            axum::http::StatusCode::PAYLOAD_TOO_LARGE,
            ErrorCode::TooLarge,
            "too many operations",
        ));
    }
    // Writes per account per minute: one account cannot monopolize the database.
    state.check_rate_limit(
        crate::state::RateKey::account(&format!("push:{}", auth.user_id)),
        crate::state::PUSH_RATE_PER_MIN,
    )?;
    let default_quota = state.config.quota_bytes;
    let (results, last_seq, members) = state
        .db(move |conn| {
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let (row, role) = vault_for_member(&tx, &vault, &auth.user_id)?;
            if !role.can_write() {
                return Err(AppError::forbidden("read-only access to this vault"));
            }
            // Storage counts against the vault's owner.
            let (quota, mut used): (u64, i64) = tx.query_row(
                "SELECT u.quota_bytes, (SELECT COALESCE(SUM(bytes), 0) FROM vaults WHERE owner_id = v.owner_id)
                 FROM vaults v JOIN users u ON u.id = v.owner_id WHERE v.id = ?1",
                [vault.as_bytes().as_slice()],
                |r| Ok((r.get::<_, Option<i64>>(0)?.map_or(default_quota, |q| q.max(0) as u64), r.get(1)?)),
            )?;
            let mut added: i64 = 0;
            let mut seq = row.last_seq;
            let mut results = Vec::with_capacity(req.ops.len());
            for op in req.ops {
                if op.blob.0.len() > MAX_BLOB_LEN {
                    results.push(PushResult::Rejected { id: op.id, reason: "record too large".into() });
                    continue;
                }
                if op.key_version != row.current_key_version {
                    results.push(PushResult::Rejected {
                        id: op.id,
                        reason: format!("records must use key version {}", row.current_key_version),
                    });
                    continue;
                }
                let current = current_record(&tx, &vault, &op.id)?;
                let current_version = current.as_ref().map_or(0, |c| c.version);
                let delta = op.blob.0.len() as i64 - current.as_ref().map_or(0, |c| c.blob.0.len() as i64);
                if quota > 0 && delta > 0 && (used + delta) as u64 > quota {
                    results.push(PushResult::Rejected { id: op.id, reason: "storage quota exceeded".into() });
                    continue;
                }
                if op.base_version != current_version {
                    if let Some(current) = current {
                        results.push(PushResult::Conflict { id: op.id, current });
                    } else {
                        results.push(PushResult::Rejected { id: op.id, reason: "record does not exist".into() });
                    }
                    continue;
                }
                seq += 1;
                let version = current_version + 1;
                tx.execute(
                    "INSERT INTO records (vault_id, id, version, seq, key_version, deleted, blob, updated_at, updated_by_device)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                     ON CONFLICT (vault_id, id) DO UPDATE SET version = ?3, seq = ?4, key_version = ?5,
                        deleted = ?6, blob = ?7, updated_at = ?8, updated_by_device = ?9",
                    params![
                        vault.as_bytes().as_slice(),
                        op.id.as_bytes().as_slice(),
                        version as i64,
                        seq as i64,
                        op.key_version,
                        op.deleted,
                        op.blob.as_slice(),
                        now(),
                        auth.device_id.as_bytes().as_slice(),
                    ],
                )?;
                used += delta;
                added += delta;
                results.push(PushResult::Ok { id: op.id, version, seq });
            }
            tx.execute(
                "UPDATE vaults SET bytes = MAX(0, bytes + ?1) WHERE id = ?2",
                params![added, vault.as_bytes().as_slice()],
            )?;
            tx.execute("UPDATE vaults SET last_seq = ?1 WHERE id = ?2", params![seq as i64, vault.as_bytes().as_slice()])?;
            let changed = seq != row.last_seq;
            let members = if changed { vault_members(&tx, &vault)? } else { Vec::new() };
            tx.commit()?;
            Ok((results, seq, members))
        })
        .await?;
    if !members.is_empty() {
        state.notify(
            members.into_iter().map(|(u, _)| u).collect(),
            ServerEvent::VaultChanged {
                vault_id: vault,
                seq: last_seq,
            },
        );
    }
    Ok(Json(PushResponse { results }))
}
