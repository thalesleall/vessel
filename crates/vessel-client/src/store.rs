//! Local SQLite store. Records are kept exactly as the server sends them (sealed with vault
//! keys); local-only data is sealed with the account key. Nothing user-visible is stored in clear.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;
use vessel_protocol::{Bytes, RecordDto, VaultSummary};

use crate::error::Result;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS vaults (
    vault_id BLOB PRIMARY KEY,
    summary TEXT NOT NULL,
    cursor INTEGER NOT NULL DEFAULT 0,
    grants TEXT,
    membership TEXT
);
CREATE TABLE IF NOT EXISTS records (
    vault_id BLOB NOT NULL,
    id BLOB NOT NULL,
    version INTEGER NOT NULL,
    seq INTEGER NOT NULL,
    key_version INTEGER NOT NULL,
    deleted INTEGER NOT NULL,
    blob BLOB NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (vault_id, id)
);
CREATE TABLE IF NOT EXISTS pending (
    vault_id BLOB NOT NULL,
    id BLOB NOT NULL,
    base_version INTEGER NOT NULL,
    deleted INTEGER NOT NULL,
    payload BLOB NOT NULL,
    queued_at INTEGER NOT NULL,
    PRIMARY KEY (vault_id, id)
);
CREATE TABLE IF NOT EXISTS conflicts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    vault_id BLOB NOT NULL,
    record_id BLOB NOT NULL,
    at INTEGER NOT NULL,
    payload BLOB NOT NULL
);
"#;

pub struct Store {
    conn: Connection,
}

/// A queued local change. `payload` is sealed with the account key.
pub struct PendingRow {
    pub vault_id: Uuid,
    pub id: Uuid,
    pub base_version: u64,
    pub deleted: bool,
    pub payload: Vec<u8>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        Self::init(Connection::open(path)?)
    }

    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    // -- key/value --------------------------------------------------------------------------

    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let raw: Option<String> = self
            .conn
            .query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0))
            .optional()?;
        Ok(raw.map(|v| serde_json::from_str(&v)).transpose()?)
    }

    pub fn set<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        self.conn.execute(
            "INSERT INTO kv (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = ?2",
            params![key, serde_json::to_string(value)?],
        )?;
        Ok(())
    }

    /// Forgets everything: used on logout and device revocation.
    pub fn wipe(&self) -> Result<()> {
        self.conn.execute_batch("DELETE FROM kv; DELETE FROM vaults; DELETE FROM records; DELETE FROM pending; DELETE FROM conflicts;")?;
        Ok(())
    }

    // -- vaults -----------------------------------------------------------------------------

    pub fn vaults(&self) -> Result<Vec<VaultSummary>> {
        let mut stmt = self.conn.prepare("SELECT summary FROM vaults")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }

    pub fn vault(&self, id: &Uuid) -> Result<Option<VaultSummary>> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT summary FROM vaults WHERE vault_id = ?1",
                [id.as_bytes().as_slice()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(raw.map(|v| serde_json::from_str(&v)).transpose()?)
    }

    pub fn put_vault(&self, summary: &VaultSummary) -> Result<()> {
        self.conn.execute(
            "INSERT INTO vaults (vault_id, summary) VALUES (?1, ?2) ON CONFLICT (vault_id) DO UPDATE SET summary = ?2",
            params![summary.vault_id.as_bytes().as_slice(), serde_json::to_string(summary)?],
        )?;
        Ok(())
    }

    pub fn remove_vault(&self, id: &Uuid) -> Result<()> {
        let id = id.as_bytes().as_slice();
        self.conn
            .execute("DELETE FROM vaults WHERE vault_id = ?1", [id])?;
        self.conn
            .execute("DELETE FROM records WHERE vault_id = ?1", [id])?;
        self.conn
            .execute("DELETE FROM pending WHERE vault_id = ?1", [id])?;
        Ok(())
    }

    pub fn cursor(&self, id: &Uuid) -> Result<u64> {
        let c: Option<i64> = self
            .conn
            .query_row(
                "SELECT cursor FROM vaults WHERE vault_id = ?1",
                [id.as_bytes().as_slice()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(c.unwrap_or(0) as u64)
    }

    pub fn set_cursor(&self, id: &Uuid, cursor: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE vaults SET cursor = ?1 WHERE vault_id = ?2",
            params![cursor as i64, id.as_bytes().as_slice()],
        )?;
        Ok(())
    }

    pub fn vault_blob<T: DeserializeOwned>(
        &self,
        id: &Uuid,
        column: VaultColumn,
    ) -> Result<Option<T>> {
        let raw: Option<Option<String>> = self
            .conn
            .query_row(
                &format!("SELECT {} FROM vaults WHERE vault_id = ?1", column.name()),
                [id.as_bytes().as_slice()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(raw
            .flatten()
            .map(|v| serde_json::from_str(&v))
            .transpose()?)
    }

    pub fn set_vault_blob<T: Serialize>(
        &self,
        id: &Uuid,
        column: VaultColumn,
        value: &T,
    ) -> Result<()> {
        self.conn.execute(
            &format!(
                "UPDATE vaults SET {} = ?1 WHERE vault_id = ?2",
                column.name()
            ),
            params![serde_json::to_string(value)?, id.as_bytes().as_slice()],
        )?;
        Ok(())
    }

    // -- records ----------------------------------------------------------------------------

    pub fn put_record(&self, vault: &Uuid, r: &RecordDto) -> Result<()> {
        self.conn.execute(
            "INSERT INTO records (vault_id, id, version, seq, key_version, deleted, blob, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (vault_id, id) DO UPDATE SET version = ?3, seq = ?4, key_version = ?5, deleted = ?6, blob = ?7, updated_at = ?8
             WHERE excluded.version >= records.version",
            params![
                vault.as_bytes().as_slice(),
                r.id.as_bytes().as_slice(),
                r.version as i64,
                r.seq as i64,
                r.key_version,
                r.deleted,
                r.blob.as_slice(),
                r.updated_at
            ],
        )?;
        Ok(())
    }

    pub fn record(&self, vault: &Uuid, id: &Uuid) -> Result<Option<RecordDto>> {
        self.conn
            .query_row(
                "SELECT id, version, seq, key_version, deleted, blob, updated_at FROM records WHERE vault_id = ?1 AND id = ?2",
                params![vault.as_bytes().as_slice(), id.as_bytes().as_slice()],
                record_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn records(&self, vault: &Uuid) -> Result<Vec<RecordDto>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, version, seq, key_version, deleted, blob, updated_at FROM records WHERE vault_id = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([vault.as_bytes().as_slice()], record_row)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    // -- pending ----------------------------------------------------------------------------

    pub fn put_pending(&self, p: &PendingRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO pending (vault_id, id, base_version, deleted, payload, queued_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (vault_id, id) DO UPDATE SET base_version = ?3, deleted = ?4, payload = ?5, queued_at = ?6",
            params![
                p.vault_id.as_bytes().as_slice(),
                p.id.as_bytes().as_slice(),
                p.base_version as i64,
                p.deleted,
                p.payload,
                crate::api::now()
            ],
        )?;
        Ok(())
    }

    pub fn pending(&self, vault: Option<&Uuid>) -> Result<Vec<PendingRow>> {
        let sql = "SELECT vault_id, id, base_version, deleted, payload FROM pending";
        let map = |r: &rusqlite::Row<'_>| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, bool>(3)?,
                r.get(4)?,
            ))
        };
        let rows: Vec<(Vec<u8>, Vec<u8>, i64, bool, Vec<u8>)> = match vault {
            Some(v) => {
                let mut stmt = self
                    .conn
                    .prepare(&format!("{sql} WHERE vault_id = ?1 ORDER BY queued_at"))?;
                stmt.query_map([v.as_bytes().as_slice()], map)?
                    .collect::<std::result::Result<_, _>>()?
            }
            None => {
                let mut stmt = self.conn.prepare(&format!("{sql} ORDER BY queued_at"))?;
                stmt.query_map([], map)?
                    .collect::<std::result::Result<_, _>>()?
            }
        };
        rows.into_iter()
            .map(|(v, id, base, deleted, payload)| {
                Ok(PendingRow {
                    vault_id: uuid(&v)?,
                    id: uuid(&id)?,
                    base_version: base as u64,
                    deleted,
                    payload,
                })
            })
            .collect()
    }

    pub fn pending_one(&self, vault: &Uuid, id: &Uuid) -> Result<Option<PendingRow>> {
        Ok(self.pending(Some(vault))?.into_iter().find(|p| &p.id == id))
    }

    pub fn remove_pending(&self, vault: &Uuid, id: &Uuid) -> Result<()> {
        self.conn.execute(
            "DELETE FROM pending WHERE vault_id = ?1 AND id = ?2",
            params![vault.as_bytes().as_slice(), id.as_bytes().as_slice()],
        )?;
        Ok(())
    }

    // -- conflicts --------------------------------------------------------------------------

    pub fn add_conflict(&self, vault: &Uuid, record: &Uuid, payload: &[u8]) -> Result<()> {
        self.conn.execute(
            "INSERT INTO conflicts (vault_id, record_id, at, payload) VALUES (?1, ?2, ?3, ?4)",
            params![
                vault.as_bytes().as_slice(),
                record.as_bytes().as_slice(),
                crate::api::now(),
                payload
            ],
        )?;
        Ok(())
    }

    pub fn conflicts(&self) -> Result<Vec<(i64, Uuid, Uuid, i64, Vec<u8>)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, vault_id, record_id, at, payload FROM conflicts ORDER BY id DESC",
        )?;
        let rows: Vec<(i64, Vec<u8>, Vec<u8>, i64, Vec<u8>)> = stmt
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<std::result::Result<_, _>>()?;
        rows.into_iter()
            .map(|(id, v, rec, at, p)| Ok((id, uuid(&v)?, uuid(&rec)?, at, p)))
            .collect()
    }

    pub fn remove_conflict(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM conflicts WHERE id = ?1", [id])?;
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub enum VaultColumn {
    Grants,
    Membership,
}

impl VaultColumn {
    fn name(self) -> &'static str {
        match self {
            Self::Grants => "grants",
            Self::Membership => "membership",
        }
    }
}

fn uuid(bytes: &[u8]) -> Result<Uuid> {
    Uuid::from_slice(bytes)
        .map_err(|e| crate::error::ClientError::invalid(format!("corrupt id: {e}")))
}

fn record_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RecordDto> {
    let id: Vec<u8> = r.get(0)?;
    Ok(RecordDto {
        id: Uuid::from_slice(&id).unwrap_or_default(),
        version: r.get::<_, i64>(1)? as u64,
        seq: r.get::<_, i64>(2)? as u64,
        key_version: r.get(3)?,
        deleted: r.get(4)?,
        blob: Bytes(r.get(5)?),
        updated_at: r.get(6)?,
        updated_by_device: Uuid::nil(),
    })
}
