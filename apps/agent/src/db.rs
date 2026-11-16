//! SQLite schema and small query helpers. Ids are stored as 16-byte BLOBs, times as Unix seconds.

use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;
use vessel_protocol::{
    AccountKeys, Bytes, KdfParams, Role, SignedEvent, UserPublic, UserPublicKeys,
};

use crate::error::{AppError, AppResult};

const SCHEMA_V1: &str = r#"
CREATE TABLE server_meta (key TEXT PRIMARY KEY, value BLOB NOT NULL);

CREATE TABLE users (
    id BLOB PRIMARY KEY,
    email TEXT NOT NULL UNIQUE,
    password_file BLOB NOT NULL,
    kdf TEXT NOT NULL,
    public_kem BLOB NOT NULL,
    public_sign BLOB NOT NULL,
    wrapped_ak_master BLOB NOT NULL,
    wrapped_ak_recovery BLOB NOT NULL,
    encrypted_private_keys BLOB NOT NULL,
    recovery_auth_hash BLOB NOT NULL,
    is_admin INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);

CREATE TABLE devices (
    id BLOB PRIMARY KEY,
    user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    public_key BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    revoked_at INTEGER
);

CREATE TABLE sessions (
    id BLOB PRIMARY KEY,
    device_id BLOB NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    user_id BLOB NOT NULL,
    access_hash BLOB NOT NULL UNIQUE,
    access_expires INTEGER NOT NULL,
    refresh_hash BLOB NOT NULL UNIQUE,
    refresh_expires INTEGER NOT NULL
);

CREATE TABLE vaults (
    id BLOB PRIMARY KEY,
    personal INTEGER NOT NULL,
    owner_id BLOB NOT NULL,
    current_key_version INTEGER NOT NULL,
    state TEXT NOT NULL,
    meta BLOB NOT NULL,
    meta_key_version INTEGER NOT NULL,
    last_seq INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);

CREATE TABLE vault_members (
    vault_id BLOB NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    user_id BLOB NOT NULL,
    role INTEGER NOT NULL,
    PRIMARY KEY (vault_id, user_id)
);
CREATE INDEX vault_members_user ON vault_members(user_id);

CREATE TABLE pending_members (
    vault_id BLOB NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    user_id BLOB NOT NULL,
    role INTEGER NOT NULL,
    invited_by BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (vault_id, user_id)
);

CREATE TABLE membership_events (
    vault_id BLOB NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    event TEXT NOT NULL,
    PRIMARY KEY (vault_id, seq)
);

CREATE TABLE grants (
    vault_id BLOB NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    key_version INTEGER NOT NULL,
    recipient_id BLOB NOT NULL,
    grant_json TEXT NOT NULL,
    PRIMARY KEY (vault_id, key_version, recipient_id)
);

CREATE TABLE records (
    vault_id BLOB NOT NULL REFERENCES vaults(id) ON DELETE CASCADE,
    id BLOB NOT NULL,
    version INTEGER NOT NULL,
    seq INTEGER NOT NULL,
    key_version INTEGER NOT NULL,
    deleted INTEGER NOT NULL,
    blob BLOB NOT NULL,
    updated_at INTEGER NOT NULL,
    updated_by_device BLOB NOT NULL,
    PRIMARY KEY (vault_id, id)
);
CREATE INDEX records_seq ON records(vault_id, seq);

CREATE TABLE invites (
    token_hash BLOB PRIMARY KEY,
    vault_id BLOB,
    role INTEGER NOT NULL,
    email TEXT,
    created_by BLOB NOT NULL,
    expires_at INTEGER NOT NULL,
    accepted_by BLOB
);
"#;

pub fn open(path: &std::path::Path) -> anyhow::Result<Connection> {
    let conn = if path.as_os_str() == ":memory:" {
        Connection::open_in_memory()?
    } else {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        Connection::open(path)?
    };
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;",
    )?;
    migrate(&conn)?;
    Ok(conn)
}

/// A read-only connection to an on-disk database (the writer opened and migrated it).
pub fn open_reader(path: &std::path::Path) -> anyhow::Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000; PRAGMA query_only=ON;")?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> anyhow::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version < 1 {
        conn.execute_batch(&format!(
            "BEGIN; {SCHEMA_V1} PRAGMA user_version = 1; COMMIT;"
        ))?;
    }
    if version < 2 {
        // Set when a member leaves or is removed; cleared when a new key rotation starts.
        conn.execute_batch(
            "BEGIN; ALTER TABLE vaults ADD COLUMN rotation_due INTEGER NOT NULL DEFAULT 0; PRAGMA user_version = 2; COMMIT;",
        )?;
    }
    if version < 3 {
        // The refresh token a session had before its last rotation, to detect reuse.
        conn.execute_batch(
            "BEGIN; ALTER TABLE sessions ADD COLUMN prev_refresh_hash BLOB;
             CREATE INDEX sessions_prev_refresh ON sessions(prev_refresh_hash);
             PRAGMA user_version = 3; COMMIT;",
        )?;
    }
    if version < 4 {
        // Operator controls (suspension, per-account quota), account deletion, and a running
        // byte count per vault for quotas.
        conn.execute_batch(
            "BEGIN;
             ALTER TABLE users ADD COLUMN suspended_at INTEGER;
             ALTER TABLE users ADD COLUMN deleted_at INTEGER;
             ALTER TABLE users ADD COLUMN quota_bytes INTEGER;
             ALTER TABLE vaults ADD COLUMN bytes INTEGER NOT NULL DEFAULT 0;
             UPDATE vaults SET bytes = (SELECT COALESCE(SUM(length(blob)), 0) FROM records WHERE records.vault_id = vaults.id);
             CREATE INDEX vaults_owner ON vaults(owner_id);
             PRAGMA user_version = 4; COMMIT;",
        )?;
    }
    Ok(())
}

/// Deletes rows that can no longer be used: sessions whose refresh token expired and invites
/// past their date. Without it both tables only grow. Returns how many rows went.
pub fn purge_expired(conn: &Connection) -> rusqlite::Result<usize> {
    let t = now();
    Ok(conn.execute("DELETE FROM sessions WHERE refresh_expires < ?1", [t])?
        + conn.execute("DELETE FROM invites WHERE expires_at < ?1", [t])?)
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn uuid_from(bytes: Vec<u8>) -> AppResult<Uuid> {
    Uuid::from_slice(&bytes).map_err(AppError::internal)
}

pub fn role_from(code: i64) -> AppResult<Role> {
    Ok(match code {
        1 => Role::Viewer,
        2 => Role::Editor,
        3 => Role::Admin,
        4 => Role::Owner,
        _ => return Err(AppError::internal(format!("bad role code {code}"))),
    })
}

pub fn role_code(role: Role) -> i64 {
    role as i64
}

pub struct UserRow {
    pub id: Uuid,
    pub email: String,
    pub password_file: Vec<u8>,
    pub kdf: KdfParams,
    pub account: AccountKeys,
    pub recovery_auth_hash: Vec<u8>,
    pub is_admin: bool,
}

impl UserRow {
    pub fn public(&self) -> UserPublic {
        UserPublic {
            user_id: self.id,
            email: self.email.clone(),
            public_keys: self.account.public_keys,
        }
    }
}

const USER_COLUMNS: &str = "id, email, password_file, kdf, public_kem, public_sign, wrapped_ak_master, \
     wrapped_ak_recovery, encrypted_private_keys, recovery_auth_hash, is_admin";

fn user_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<(Vec<u8>, UserRowRaw)> {
    Ok((
        r.get(0)?,
        UserRowRaw {
            email: r.get(1)?,
            password_file: r.get(2)?,
            kdf: r.get(3)?,
            public_kem: r.get(4)?,
            public_sign: r.get(5)?,
            wrapped_ak_master: r.get(6)?,
            wrapped_ak_recovery: r.get(7)?,
            encrypted_private_keys: r.get(8)?,
            recovery_auth_hash: r.get(9)?,
            is_admin: r.get(10)?,
        },
    ))
}

struct UserRowRaw {
    email: String,
    password_file: Vec<u8>,
    kdf: String,
    public_kem: Vec<u8>,
    public_sign: Vec<u8>,
    wrapped_ak_master: Vec<u8>,
    wrapped_ak_recovery: Vec<u8>,
    encrypted_private_keys: Vec<u8>,
    recovery_auth_hash: Vec<u8>,
    is_admin: bool,
}

fn finish_user(id: Vec<u8>, raw: UserRowRaw) -> AppResult<UserRow> {
    let key = |v: Vec<u8>| -> AppResult<[u8; 32]> {
        v.try_into()
            .map_err(|_| AppError::internal("bad key length"))
    };
    Ok(UserRow {
        id: uuid_from(id)?,
        email: raw.email,
        password_file: raw.password_file,
        kdf: serde_json::from_str(&raw.kdf)?,
        account: AccountKeys {
            public_keys: UserPublicKeys {
                kem: key(raw.public_kem)?,
                sign: key(raw.public_sign)?,
            },
            wrapped_account_key_master: Bytes(raw.wrapped_ak_master),
            wrapped_account_key_recovery: Bytes(raw.wrapped_ak_recovery),
            encrypted_private_keys: Bytes(raw.encrypted_private_keys),
        },
        recovery_auth_hash: raw.recovery_auth_hash,
        is_admin: raw.is_admin,
    })
}

pub fn user_by_email(conn: &Connection, email: &str) -> AppResult<Option<UserRow>> {
    let row = conn
        .query_row(
            &format!("SELECT {USER_COLUMNS} FROM users WHERE email = ?1"),
            [email],
            user_from_row,
        )
        .optional()?;
    row.map(|(id, raw)| finish_user(id, raw)).transpose()
}

pub fn user_by_id(conn: &Connection, id: &Uuid) -> AppResult<Option<UserRow>> {
    let row = conn
        .query_row(
            &format!("SELECT {USER_COLUMNS} FROM users WHERE id = ?1"),
            [id.as_bytes().as_slice()],
            user_from_row,
        )
        .optional()?;
    row.map(|(id, raw)| finish_user(id, raw)).transpose()
}

pub fn user_public(conn: &Connection, id: &Uuid) -> AppResult<Option<UserPublic>> {
    Ok(user_by_id(conn, id)?.map(|u| u.public()))
}

/// Whether an account may act: not suspended by the operator, not deleted.
pub fn user_active(conn: &Connection, id: &Uuid) -> AppResult<bool> {
    Ok(conn
        .query_row(
            "SELECT suspended_at IS NULL AND deleted_at IS NULL FROM users WHERE id = ?1",
            [id.as_bytes().as_slice()],
            |r| r.get::<_, bool>(0),
        )
        .optional()?
        .unwrap_or(false))
}

pub fn user_count(conn: &Connection) -> AppResult<i64> {
    // Deleted (pseudonymized) accounts do not count: a server whose accounts were all deleted
    // needs a new first admin, with the installation code.
    Ok(conn.query_row("SELECT COUNT(*) FROM users WHERE deleted_at IS NULL", [], |r| r.get(0))?)
}

pub fn member_role(conn: &Connection, vault: &Uuid, user: &Uuid) -> AppResult<Option<Role>> {
    let code: Option<i64> = conn
        .query_row(
            "SELECT role FROM vault_members WHERE vault_id = ?1 AND user_id = ?2",
            params![vault.as_bytes().as_slice(), user.as_bytes().as_slice()],
            |r| r.get(0),
        )
        .optional()?;
    code.map(role_from).transpose()
}

pub fn vault_members(conn: &Connection, vault: &Uuid) -> AppResult<Vec<(Uuid, Role)>> {
    let mut stmt = conn.prepare("SELECT user_id, role FROM vault_members WHERE vault_id = ?1")?;
    let rows = stmt.query_map([vault.as_bytes().as_slice()], |r| {
        Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?))
    })?;
    rows.map(|row| {
        let (u, role) = row?;
        Ok((uuid_from(u)?, role_from(role)?))
    })
    .collect()
}

pub fn membership_events(conn: &Connection, vault: &Uuid) -> AppResult<Vec<SignedEvent>> {
    let mut stmt =
        conn.prepare("SELECT event FROM membership_events WHERE vault_id = ?1 ORDER BY seq")?;
    let rows = stmt.query_map([vault.as_bytes().as_slice()], |r| r.get::<_, String>(0))?;
    rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
}

pub fn insert_event(conn: &Connection, vault: &Uuid, event: &SignedEvent) -> AppResult<()> {
    conn.execute(
        "INSERT INTO membership_events (vault_id, seq, event) VALUES (?1, ?2, ?3)",
        params![
            vault.as_bytes().as_slice(),
            event.body.seq as i64,
            serde_json::to_string(event)?
        ],
    )?;
    Ok(())
}

pub fn insert_grant(
    conn: &Connection,
    vault: &Uuid,
    grant: &vessel_protocol::Grant,
) -> AppResult<()> {
    conn.execute(
        "INSERT OR REPLACE INTO grants (vault_id, key_version, recipient_id, grant_json) VALUES (?1, ?2, ?3, ?4)",
        params![vault.as_bytes().as_slice(), grant.key_version, grant.recipient_id.as_slice(), serde_json::to_string(grant)?],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_sessions_and_invites_are_purged() {
        let conn = open(std::path::Path::new(":memory:")).unwrap();
        let t = now();
        conn.execute(
            "INSERT INTO users (id, email, password_file, kdf, public_kem, public_sign, wrapped_ak_master,
                wrapped_ak_recovery, encrypted_private_keys, recovery_auth_hash, created_at)
             VALUES (x'01', 'a@example.com', x'', '{}', x'', x'', x'', x'', x'', x'', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO devices (id, user_id, name, public_key, created_at, last_seen_at) VALUES (x'02', x'01', 'pc', x'', 0, 0)",
            [],
        )
        .unwrap();
        for (id, expires) in [(1_u8, t - 10), (2, t + 3600)] {
            conn.execute(
                "INSERT INTO sessions (id, device_id, user_id, access_hash, access_expires, refresh_hash, refresh_expires)
                 VALUES (?1, x'02', x'01', ?1, 0, ?1, ?2)",
                params![vec![id], expires],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO invites (token_hash, role, created_by, expires_at) VALUES (?1, 1, x'01', ?2)",
                params![vec![id], expires],
            )
            .unwrap();
        }
        assert_eq!(purge_expired(&conn).unwrap(), 2);
        let left = |table: &str| -> i64 {
            conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0)).unwrap()
        };
        assert_eq!((left("sessions"), left("invites")), (1, 1));
        assert_eq!(purge_expired(&conn).unwrap(), 0);
    }
}
