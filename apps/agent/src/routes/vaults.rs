//! Vaults, signed membership history, grants and key rotation.

use std::collections::{BTreeSet, HashMap};

use axum::Json;
use axum::extract::{Path, State};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;
use vessel_crypto::membership::{Action, Membership, replay};
use vessel_protocol::*;

use crate::auth::Auth;
use crate::db::{
    self, insert_event, insert_grant, member_role, now, role_code, uuid_from, vault_members,
};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub struct VaultRow {
    pub id: Uuid,
    pub personal: bool,
    pub current_key_version: u32,
    pub state: VaultState,
    pub meta: Vec<u8>,
    pub meta_key_version: u32,
    pub last_seq: u64,
    pub rotation_due: bool,
}

pub fn vault_row(conn: &Connection, id: &Uuid) -> AppResult<Option<VaultRow>> {
    let row = conn
        .query_row(
            "SELECT personal, current_key_version, state, meta, meta_key_version, last_seq, rotation_due FROM vaults WHERE id = ?1",
            [id.as_bytes().as_slice()],
            |r| {
                Ok((
                    r.get::<_, bool>(0)?,
                    r.get::<_, u32>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Vec<u8>>(3)?,
                    r.get::<_, u32>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, bool>(6)?,
                ))
            },
        )
        .optional()?;
    Ok(row.map(|(personal, kv, st, meta, mkv, seq, due)| VaultRow {
        id: *id,
        personal,
        current_key_version: kv,
        state: if st == "rotating" {
            VaultState::Rotating
        } else {
            VaultState::Active
        },
        meta,
        meta_key_version: mkv,
        last_seq: seq as u64,
        rotation_due: due,
    }))
}

/// Loads a vault and the caller's role in it; outsiders get `404`, not `403`.
pub fn vault_for_member(
    conn: &Connection,
    vault: &Uuid,
    user: &Uuid,
) -> AppResult<(VaultRow, Role)> {
    let role =
        member_role(conn, vault, user)?.ok_or_else(|| AppError::not_found("vault not found"))?;
    let row = vault_row(conn, vault)?.ok_or_else(|| AppError::not_found("vault not found"))?;
    Ok((row, role))
}

fn summary(row: VaultRow, role: Role) -> VaultSummary {
    VaultSummary {
        vault_id: row.id,
        personal: row.personal,
        role,
        current_key_version: row.current_key_version,
        state: row.state,
        meta: Bytes(row.meta),
        meta_key_version: row.meta_key_version,
        last_seq: row.last_seq,
        rotation_due: row.rotation_due,
    }
}

fn public_keys_lookup(conn: &Connection) -> impl FnMut(&[u8; 16]) -> Option<UserPublicKeys> + '_ {
    let mut cache: HashMap<[u8; 16], Option<UserPublicKeys>> = HashMap::new();
    move |id| {
        *cache.entry(*id).or_insert_with(|| {
            db::user_public(conn, &Uuid::from_bytes(*id))
                .ok()
                .flatten()
                .map(|u| u.public_keys)
        })
    }
}

/// Replays the stored history of a vault.
pub fn current_membership(conn: &Connection, vault: &Uuid) -> AppResult<Membership> {
    let events = db::membership_events(conn, vault)?;
    replay(vault.as_bytes(), &events, public_keys_lookup(conn))
        .map_err(|e| AppError::internal(format!("stored membership history is invalid: {e}")))
}

/// Checks that a grant is issued by `granter` to `recipient` for this vault and key version.
fn check_grant(
    conn: &Connection,
    vault: &Uuid,
    grant: &Grant,
    granter: &Uuid,
    recipient: &Uuid,
    key_version: u32,
) -> AppResult<()> {
    let bad = |m: &str| AppError::bad_request(format!("invalid grant: {m}"));
    if grant.vault_id != *vault.as_bytes() || grant.key_version != key_version {
        return Err(bad("wrong vault or key version"));
    }
    if grant.recipient_id != *recipient.as_bytes() || grant.granter_id != *granter.as_bytes() {
        return Err(bad("wrong parties"));
    }
    let recipient_keys =
        db::user_public(conn, recipient)?.ok_or_else(|| bad("unknown recipient"))?;
    if recipient_keys.public_keys.fingerprint().0 != grant.recipient_fingerprint {
        return Err(bad("recipient keys do not match"));
    }
    let granter_keys = db::user_public(conn, granter)?.ok_or_else(|| bad("unknown granter"))?;
    grant
        .verify(&granter_keys.public_keys)
        .map_err(|_| bad("bad signature"))
}

/// Creates a vault from a [`CreateVaultRequest`] made by `owner`. Shared by sign-up.
pub fn insert_new_vault(
    conn: &Connection,
    owner: &Uuid,
    req: &CreateVaultRequest,
) -> AppResult<VaultRow> {
    if vault_row(conn, &req.vault_id)?.is_some() {
        return Err(AppError::conflict("vault id already exists"));
    }
    let event = &req.creation_event;
    if event.body.action != Action::Create || event.body.actor != *owner.as_bytes() {
        return Err(AppError::bad_request(
            "first event must create the vault as the caller",
        ));
    }
    let owner_keys = db::user_public(conn, owner)?
        .ok_or_else(AppError::unauthorized)?
        .public_keys;
    replay(req.vault_id.as_bytes(), [event], |_| Some(owner_keys))
        .map_err(|e| AppError::bad_request(format!("invalid creation event: {e}")))?;
    check_grant(conn, &req.vault_id, &req.grant, owner, owner, 1)?;
    if req.meta.0.len() > MAX_BLOB_LEN {
        return Err(AppError::bad_request("vault metadata too large"));
    }
    if req.personal {
        let existing: i64 = conn.query_row(
            "SELECT COUNT(*) FROM vaults WHERE owner_id = ?1 AND personal = 1",
            [owner.as_bytes().as_slice()],
            |r| r.get(0),
        )?;
        if existing > 0 {
            return Err(AppError::conflict("personal vault already exists"));
        }
    }

    conn.execute(
        "INSERT INTO vaults (id, personal, owner_id, current_key_version, state, meta, meta_key_version, created_at)
         VALUES (?1, ?2, ?3, 1, 'active', ?4, 1, ?5)",
        params![req.vault_id.as_bytes().as_slice(), req.personal, owner.as_bytes().as_slice(), req.meta.as_slice(), now()],
    )?;
    conn.execute(
        "INSERT INTO vault_members (vault_id, user_id, role) VALUES (?1, ?2, ?3)",
        params![
            req.vault_id.as_bytes().as_slice(),
            owner.as_bytes().as_slice(),
            role_code(Role::Owner)
        ],
    )?;
    insert_event(conn, &req.vault_id, event)?;
    insert_grant(conn, &req.vault_id, &req.grant)?;
    vault_row(conn, &req.vault_id)?.ok_or_else(|| AppError::internal("vault vanished"))
}

pub async fn list(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Vec<VaultSummary>>> {
    let vaults = state
        .db_read(move |conn| {
            let mut stmt =
                conn.prepare("SELECT vault_id, role FROM vault_members WHERE user_id = ?1")?;
            let rows: Vec<(Vec<u8>, i64)> = stmt
                .query_map([auth.user_id.as_bytes().as_slice()], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?
                .collect::<Result<_, _>>()?;
            let mut out = Vec::new();
            for (id, role) in rows {
                let id = uuid_from(id)?;
                if let Some(row) = vault_row(conn, &id)? {
                    out.push(summary(row, db::role_from(role)?));
                }
            }
            Ok(out)
        })
        .await?;
    Ok(Json(vaults))
}

pub async fn create(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<CreateVaultRequest>,
) -> AppResult<Json<VaultSummary>> {
    if req.personal {
        return Err(AppError::bad_request(
            "personal vaults are created at sign-up",
        ));
    }
    let max_vaults = state.config.max_vaults;
    let row = state
        .db(move |conn| {
            let tx = conn.transaction()?;
            if max_vaults > 0 {
                let owned: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM vaults WHERE owner_id = ?1 AND personal = 0",
                    [auth.user_id.as_bytes().as_slice()],
                    |r| r.get(0),
                )?;
                if owned >= i64::from(max_vaults) {
                    return Err(AppError::new(
                        axum::http::StatusCode::FORBIDDEN,
                        ErrorCode::QuotaExceeded,
                        "this account reached its limit of shared vaults",
                    ));
                }
            }
            let row = insert_new_vault(&tx, &auth.user_id, &req)?;
            tx.commit()?;
            Ok(row)
        })
        .await?;
    let vault_id = row.id;
    state.notify(vec![auth.user_id], ServerEvent::VaultUpdated { vault_id });
    Ok(Json(summary(row, Role::Owner)))
}

pub async fn get(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> AppResult<Json<VaultSummary>> {
    let (row, role) = state
        .db_read(move |conn| vault_for_member(conn, &id, &auth.user_id))
        .await?;
    Ok(Json(summary(row, role)))
}

pub async fn delete_vault(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> AppResult<Json<()>> {
    let members = state
        .db(move |conn| {
            let (row, role) = vault_for_member(conn, &id, &auth.user_id)?;
            if role != Role::Owner || row.personal {
                return Err(AppError::forbidden(
                    "only the owner can delete a shared vault",
                ));
            }
            let members = vault_members(conn, &id)?;
            // Invites have no foreign key to vaults: drop them explicitly so a deleted vault's
            // invite can neither admit sign-ups nor be accepted.
            conn.execute(
                "DELETE FROM invites WHERE vault_id = ?1",
                [id.as_bytes().as_slice()],
            )?;
            conn.execute(
                "DELETE FROM vaults WHERE id = ?1",
                [id.as_bytes().as_slice()],
            )?;
            Ok(members)
        })
        .await?;
    state.notify(
        members.into_iter().map(|(u, _)| u).collect(),
        ServerEvent::VaultRemoved { vault_id: id },
    );
    Ok(Json(()))
}

pub async fn update_meta(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateVaultMetaRequest>,
) -> AppResult<Json<()>> {
    let members = state
        .db(move |conn| {
            let (row, role) = vault_for_member(conn, &id, &auth.user_id)?;
            if !role.can_manage() {
                return Err(AppError::forbidden("admins only"));
            }
            if req.meta_key_version != row.current_key_version || req.meta.0.len() > MAX_BLOB_LEN {
                return Err(AppError::bad_request(
                    "metadata must use the current key version",
                ));
            }
            conn.execute(
                "UPDATE vaults SET meta = ?1, meta_key_version = ?2 WHERE id = ?3",
                params![
                    req.meta.as_slice(),
                    req.meta_key_version,
                    id.as_bytes().as_slice()
                ],
            )?;
            vault_members(conn, &id)
        })
        .await?;
    state.notify(
        members.into_iter().map(|(u, _)| u).collect(),
        ServerEvent::VaultUpdated { vault_id: id },
    );
    Ok(Json(()))
}

pub async fn membership(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> AppResult<Json<MembershipResponse>> {
    let response = state
        .db_read(move |conn| {
            vault_for_member(conn, &id, &auth.user_id)?;
            let events = db::membership_events(conn, &id)?;
            let mut stmt = conn.prepare(
                "SELECT user_id, role, invited_by, created_at FROM pending_members WHERE vault_id = ?1 ORDER BY created_at",
            )?;
            let pending_rows: Vec<(Vec<u8>, i64, Vec<u8>, i64)> = stmt
                .query_map([id.as_bytes().as_slice()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<Result<_, _>>()?;

            let mut user_ids: BTreeSet<Uuid> = BTreeSet::new();
            for e in &events {
                user_ids.insert(Uuid::from_bytes(e.body.actor));
                user_ids.insert(Uuid::from_bytes(e.body.subject));
            }
            let mut pending = Vec::new();
            for (user, role, invited_by, created_at) in pending_rows {
                let user = uuid_from(user)?;
                if let Some(public) = db::user_public(conn, &user)? {
                    pending.push(PendingMember {
                        user: public,
                        role: db::role_from(role)?,
                        invited_by: uuid_from(invited_by)?,
                        created_at,
                    });
                }
            }
            let users = user_ids
                .iter()
                .filter_map(|u| db::user_public(conn, u).transpose())
                .collect::<AppResult<Vec<_>>>()?;
            Ok(MembershipResponse { events, users, pending })
        })
        .await?;
    Ok(Json(response))
}

pub async fn change_membership(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(req): Json<MembershipChangeRequest>,
) -> AppResult<Json<()>> {
    let (members, subject, action) = state
        .db(move |conn| {
            let tx = conn.transaction()?;
            let (row, _) = vault_for_member(&tx, &id, &auth.user_id)?;
            if row.personal {
                return Err(AppError::forbidden("the personal vault cannot be shared"));
            }
            if row.state == VaultState::Rotating && req.event.body.action != Action::Remove {
                return Err(AppError::conflict("finish the key rotation first"));
            }
            let events: i64 = tx.query_row(
                "SELECT COUNT(*) FROM membership_events WHERE vault_id = ?1",
                [id.as_bytes().as_slice()],
                |r| r.get(0),
            )?;
            if events >= crate::state::MAX_MEMBERSHIP_EVENTS {
                return Err(AppError::new(
                    axum::http::StatusCode::FORBIDDEN,
                    ErrorCode::QuotaExceeded,
                    "this vault's membership history is full",
                ));
            }
            let body = &req.event.body;
            if body.actor != *auth.user_id.as_bytes() {
                return Err(AppError::forbidden("events must be signed by the caller"));
            }
            let subject = Uuid::from_bytes(body.subject);
            let actor_keys = db::user_public(&tx, &auth.user_id)?
                .ok_or_else(AppError::unauthorized)?
                .public_keys;

            let mut membership = current_membership(&tx, &id)?;
            membership
                .apply(id.as_bytes(), &req.event, &actor_keys)
                .map_err(|e| AppError::forbidden(format!("membership change rejected: {e}")))?;

            match body.action {
                Action::Add => {
                    let role = body
                        .role
                        .ok_or_else(|| AppError::bad_request("missing role"))?;
                    let pending: Option<i64> = tx
                        .query_row(
                            "SELECT role FROM pending_members WHERE vault_id = ?1 AND user_id = ?2",
                            params![id.as_bytes().as_slice(), subject.as_bytes().as_slice()],
                            |r| r.get(0),
                        )
                        .optional()?;
                    let Some(invited_role) = pending else {
                        return Err(AppError::bad_request(
                            "the user has not accepted an invite to this vault",
                        ));
                    };
                    // The approval may lower the role chosen when the invite was created, never
                    // raise it (only the owner can invite admins).
                    if role > crate::db::role_from(invited_role)? {
                        return Err(AppError::forbidden(
                            "the role is higher than the one in the invite",
                        ));
                    }
                    let grant = req
                        .grants
                        .iter()
                        .find(|g| g.key_version == row.current_key_version)
                        .ok_or_else(|| {
                            AppError::bad_request("missing grant for the current key version")
                        })?;
                    check_grant(
                        &tx,
                        &id,
                        grant,
                        &auth.user_id,
                        &subject,
                        row.current_key_version,
                    )?;
                    insert_grant(&tx, &id, grant)?;
                    tx.execute(
                        "INSERT INTO vault_members (vault_id, user_id, role) VALUES (?1, ?2, ?3)",
                        params![
                            id.as_bytes().as_slice(),
                            subject.as_bytes().as_slice(),
                            role_code(role)
                        ],
                    )?;
                    tx.execute(
                        "DELETE FROM pending_members WHERE vault_id = ?1 AND user_id = ?2",
                        params![id.as_bytes().as_slice(), subject.as_bytes().as_slice()],
                    )?;
                }
                Action::ChangeRole => {
                    let role = body
                        .role
                        .ok_or_else(|| AppError::bad_request("missing role"))?;
                    let before: Option<i64> = tx
                        .query_row(
                            "SELECT role FROM vault_members WHERE vault_id = ?1 AND user_id = ?2",
                            params![id.as_bytes().as_slice(), subject.as_bytes().as_slice()],
                            |r| r.get(0),
                        )
                        .optional()?;
                    // A demoted manager signed the current key's grants: clients accept new key
                    // versions only from current managers, so the vault rotates to one.
                    if before
                        .and_then(|r| crate::db::role_from(r).ok())
                        .is_some_and(|r| r.can_manage() && !role.can_manage())
                    {
                        tx.execute(
                            "UPDATE vaults SET rotation_due = 1 WHERE id = ?1",
                            [id.as_bytes().as_slice()],
                        )?;
                    }
                    tx.execute(
                        "UPDATE vault_members SET role = ?1 WHERE vault_id = ?2 AND user_id = ?3",
                        params![
                            role_code(role),
                            id.as_bytes().as_slice(),
                            subject.as_bytes().as_slice()
                        ],
                    )?;
                }
                Action::Remove => {
                    tx.execute(
                        "DELETE FROM vault_members WHERE vault_id = ?1 AND user_id = ?2",
                        params![id.as_bytes().as_slice(), subject.as_bytes().as_slice()],
                    )?;
                    tx.execute(
                        "DELETE FROM grants WHERE vault_id = ?1 AND recipient_id = ?2",
                        params![id.as_bytes().as_slice(), subject.as_bytes().as_slice()],
                    )?;
                    // The former member may hold the current key, including one handed out by
                    // a rotation still in progress: the vault needs a fresh key.
                    tx.execute(
                        "UPDATE vaults SET rotation_due = 1 WHERE id = ?1",
                        [id.as_bytes().as_slice()],
                    )?;
                }
                Action::Create => return Err(AppError::bad_request("vault already exists")),
            }
            insert_event(&tx, &id, &req.event)?;
            let members = vault_members(&tx, &id)?;
            tx.commit()?;
            Ok((members, subject, body.action))
        })
        .await?;

    state.notify(
        members.into_iter().map(|(u, _)| u).collect(),
        ServerEvent::VaultUpdated { vault_id: id },
    );
    if action == Action::Remove {
        state.notify(vec![subject], ServerEvent::VaultRemoved { vault_id: id });
    }
    Ok(Json(()))
}

pub async fn my_grants(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> AppResult<Json<GrantsResponse>> {
    let response = state
        .db_read(move |conn| {
            vault_for_member(conn, &id, &auth.user_id)?;
            let mut stmt = conn.prepare(
                "SELECT grant_json FROM grants WHERE vault_id = ?1 AND recipient_id = ?2 ORDER BY key_version",
            )?;
            let grants: Vec<Grant> = stmt
                .query_map(params![id.as_bytes().as_slice(), auth.user_id.as_bytes().as_slice()], |r| {
                    r.get::<_, String>(0)
                })?
                .map(|row| Ok(serde_json::from_str(&row?)?))
                .collect::<AppResult<_>>()?;
            let granter_ids: BTreeSet<Uuid> = grants.iter().map(|g| Uuid::from_bytes(g.granter_id)).collect();
            let granters = granter_ids
                .iter()
                .filter_map(|u| db::user_public(conn, u).transpose())
                .collect::<AppResult<Vec<_>>>()?;
            Ok(GrantsResponse { grants, granters })
        })
        .await?;
    Ok(Json(response))
}

pub async fn rotate_start(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(req): Json<RotateStartRequest>,
) -> AppResult<Json<VaultSummary>> {
    let (row, role, members) = state
        .db(move |conn| {
            let tx = conn.transaction()?;
            let (row, role) = vault_for_member(&tx, &id, &auth.user_id)?;
            if !role.can_manage() {
                return Err(AppError::forbidden("admins only"));
            }
            if row.state == VaultState::Rotating {
                return Err(AppError::conflict("a rotation is already in progress"));
            }
            if req.meta.0.len() > MAX_BLOB_LEN {
                return Err(AppError::bad_request("vault metadata is too large"));
            }
            if req.new_key_version != row.current_key_version + 1 {
                return Err(AppError::bad_request("new key version must be current + 1"));
            }
            let members = vault_members(&tx, &id)?;
            let covered: BTreeSet<[u8; 16]> = req.grants.iter().map(|g| g.recipient_id).collect();
            let expected: BTreeSet<[u8; 16]> = members.iter().map(|(u, _)| *u.as_bytes()).collect();
            if covered != expected || req.grants.len() != expected.len() {
                return Err(AppError::bad_request("rotation needs exactly one grant per member"));
            }
            for grant in &req.grants {
                check_grant(&tx, &id, grant, &auth.user_id, &Uuid::from_bytes(grant.recipient_id), req.new_key_version)?;
            }
            for grant in &req.grants {
                insert_grant(&tx, &id, grant)?;
            }
            tx.execute(
                "UPDATE vaults SET current_key_version = ?1, state = 'rotating', meta = ?2, meta_key_version = ?1, rotation_due = 0 WHERE id = ?3",
                params![req.new_key_version, req.meta.as_slice(), id.as_bytes().as_slice()],
            )?;
            let row = vault_row(&tx, &id)?.ok_or_else(|| AppError::internal("vault vanished"))?;
            tx.commit()?;
            Ok((row, role, members))
        })
        .await?;
    state.notify(
        members.into_iter().map(|(u, _)| u).collect(),
        ServerEvent::VaultUpdated { vault_id: id },
    );
    Ok(Json(summary(row, role)))
}

pub async fn rotate_finish(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
) -> AppResult<Json<VaultSummary>> {
    let (row, role, members) = state
        .db(move |conn| {
            let tx = conn.transaction()?;
            let (row, role) = vault_for_member(&tx, &id, &auth.user_id)?;
            if !role.can_manage() {
                return Err(AppError::forbidden("admins only"));
            }
            if row.state != VaultState::Rotating {
                return Err(AppError::conflict("no rotation in progress"));
            }
            let stale: i64 = tx.query_row(
                "SELECT COUNT(*) FROM records WHERE vault_id = ?1 AND key_version < ?2",
                params![id.as_bytes().as_slice(), row.current_key_version],
                |r| r.get(0),
            )?;
            if stale > 0 {
                return Err(AppError::conflict(format!(
                    "{stale} records still use the old key"
                )));
            }
            tx.execute(
                "DELETE FROM grants WHERE vault_id = ?1 AND key_version < ?2",
                params![id.as_bytes().as_slice(), row.current_key_version],
            )?;
            tx.execute(
                "UPDATE vaults SET state = 'active' WHERE id = ?1",
                [id.as_bytes().as_slice()],
            )?;
            let row = vault_row(&tx, &id)?.ok_or_else(|| AppError::internal("vault vanished"))?;
            let members = vault_members(&tx, &id)?;
            tx.commit()?;
            Ok((row, role, members))
        })
        .await?;
    state.notify(
        members.into_iter().map(|(u, _)| u).collect(),
        ServerEvent::VaultUpdated { vault_id: id },
    );
    Ok(Json(summary(row, role)))
}
