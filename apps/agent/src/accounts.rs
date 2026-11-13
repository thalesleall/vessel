//! Deleting an account, by its owner or by the server operator.
//!
//! The account row stays, pseudonymized: its public keys sign events in other people's vault
//! histories, which their apps keep verifying. Everything else goes: e-mail, password file,
//! wrapped keys, devices, sessions, invites, and the vaults the account owns (with their
//! records, grants and history).

use rusqlite::{Connection, params};
use uuid::Uuid;

use crate::db::{now, uuid_from, vault_members};
use crate::error::{AppError, AppResult};

/// Members of each deleted vault, to notify.
pub type RemovedVaults = Vec<(Uuid, Vec<Uuid>)>;

/// Deletes `user`. With `require_left`, refuses while the account is still a member of vaults
/// it does not own (its app leaves them first, with signed events).
pub fn delete_account(conn: &Connection, user: &Uuid, require_left: bool) -> AppResult<RemovedVaults> {
    let id = user.as_bytes().as_slice();
    if require_left {
        let foreign: i64 = conn.query_row(
            "SELECT COUNT(*) FROM vault_members m JOIN vaults v ON v.id = m.vault_id
             WHERE m.user_id = ?1 AND v.owner_id != ?1",
            [id],
            |r| r.get(0),
        )?;
        if foreign > 0 {
            return Err(AppError::conflict("leave the shared vaults you do not own first"));
        }
    }
    let (is_admin, others, other_admins): (bool, i64, i64) = conn.query_row(
        "SELECT (SELECT is_admin FROM users WHERE id = ?1),
                (SELECT COUNT(*) FROM users WHERE id != ?1 AND deleted_at IS NULL),
                (SELECT COUNT(*) FROM users WHERE id != ?1 AND deleted_at IS NULL AND is_admin = 1)",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if is_admin && others > 0 && other_admins == 0 {
        return Err(AppError::conflict("make another account server admin first"));
    }

    let mut removed = Vec::new();
    let owned: Vec<Vec<u8>> = conn
        .prepare("SELECT id FROM vaults WHERE owner_id = ?1")?
        .query_map([id], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for vault in owned {
        let vault_id = uuid_from(vault.clone())?;
        let members = vault_members(conn, &vault_id)?
            .into_iter()
            .map(|(u, _)| u)
            .filter(|u| u != user)
            .collect();
        conn.execute("DELETE FROM invites WHERE vault_id = ?1", [&vault])?;
        conn.execute("DELETE FROM vaults WHERE id = ?1", [&vault])?;
        removed.push((vault_id, members));
    }
    conn.execute("DELETE FROM invites WHERE created_by = ?1", [id])?;
    conn.execute("DELETE FROM pending_members WHERE user_id = ?1", [id])?;
    conn.execute("DELETE FROM sessions WHERE user_id = ?1", [id])?;
    conn.execute("DELETE FROM devices WHERE user_id = ?1", [id])?;
    conn.execute(
        "UPDATE users SET email = 'deleted-' || lower(hex(id)) || '@deleted.invalid',
            password_file = x'', wrapped_ak_master = x'', wrapped_ak_recovery = x'',
            encrypted_private_keys = x'', recovery_auth_hash = randomblob(32),
            is_admin = 0, suspended_at = NULL, quota_bytes = NULL, deleted_at = ?2
         WHERE id = ?1",
        params![id, now()],
    )?;
    tracing::info!(user = %user, "account deleted");
    Ok(removed)
}
