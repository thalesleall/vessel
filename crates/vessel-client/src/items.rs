//! Reading and editing the decrypted contents of vaults. Edits are queued locally and pushed by
//! the next sync, so everything works offline.

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use vessel_crypto::envelope::{RecordContext, open_record, seal_record};
use vessel_crypto::wrap::{WrapPurpose, open_bytes, seal_bytes};
use vessel_model::{Entity, EntityKind, FieldConflict};
use vessel_protocol::RecordDto;

use crate::store::PendingRow;
use crate::{ClientError, Result, Vessel};

/// A queued edit, sealed with the account key. `base` is the version the edit started from.
#[derive(Serialize, Deserialize)]
pub(crate) struct PendingPayload {
    pub entity: Option<Entity>,
    pub base: Option<Entity>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub vault_id: Uuid,
    pub id: Uuid,
    /// Server version this item is based on (0 = never pushed).
    pub version: u64,
    /// Has local changes not yet pushed.
    pub pending: bool,
    pub entity: Entity,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConflictView {
    pub id: i64,
    pub vault_id: Uuid,
    pub record_id: Uuid,
    pub at: i64,
    pub label: String,
    pub conflicts: Vec<FieldConflict>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct ConflictPayload {
    pub label: String,
    pub conflicts: Vec<FieldConflict>,
}

fn ctx(vault: &Uuid, id: &Uuid, key_version: u32) -> RecordContext {
    RecordContext {
        vault_id: *vault.as_bytes(),
        record_id: *id.as_bytes(),
        key_version,
    }
}

impl Vessel {
    pub(crate) fn open_entity(&self, vault: &Uuid, record: &RecordDto) -> Result<Option<Entity>> {
        if record.deleted {
            return Ok(None);
        }
        let key = self.vault_key(vault, record.key_version)?;
        let plain = open_record(
            key,
            &ctx(vault, &record.id, record.key_version),
            record.blob.as_slice(),
        )?;
        if plain.as_slice() == b"null" {
            return Ok(None);
        }
        Ok(Some(Entity::from_json(&plain)?))
    }

    pub(crate) fn seal_entity(
        &self,
        vault: &Uuid,
        id: &Uuid,
        entity: Option<&Entity>,
    ) -> Result<(u32, Vec<u8>)> {
        let summary = self
            .store
            .vault(vault)?
            .ok_or_else(|| ClientError::invalid("unknown vault"))?;
        let kv = summary.current_key_version;
        let json = entity.map_or_else(|| b"null".to_vec(), Entity::to_json);
        Ok((
            kv,
            seal_record(self.vault_key(vault, kv)?, &ctx(vault, id, kv), &json)?,
        ))
    }

    pub(crate) fn seal_local<T: Serialize>(&self, value: &T) -> Result<Vec<u8>> {
        let ak = &self.unlocked()?.account.account_key;
        Ok(seal_bytes(
            ak,
            WrapPurpose::LocalData,
            &serde_json::to_vec(value)?,
        )?)
    }

    pub(crate) fn open_local<T: for<'de> Deserialize<'de>>(&self, sealed: &[u8]) -> Result<T> {
        let ak = &self.unlocked()?.account.account_key;
        Ok(serde_json::from_slice(&open_bytes(
            ak,
            WrapPurpose::LocalData,
            sealed,
        )?)?)
    }

    /// Every item the user can see, optionally of one kind, with local edits applied.
    pub fn items(&self, kind: Option<EntityKind>) -> Result<Vec<Item>> {
        self.unlocked()?;
        let mut out = Vec::new();
        for summary in self.store.vaults()? {
            let vault = summary.vault_id;
            let pending: std::collections::HashMap<Uuid, PendingRow> = self
                .store
                .pending(Some(&vault))?
                .into_iter()
                .map(|p| (p.id, p))
                .collect();
            for record in self.store.records(&vault)? {
                if pending.contains_key(&record.id) {
                    continue;
                }
                match self.open_entity(&vault, &record) {
                    Ok(Some(entity)) => out.push(Item {
                        vault_id: vault,
                        id: record.id,
                        version: record.version,
                        pending: false,
                        entity,
                    }),
                    Ok(None) => {}
                    Err(e) => tracing::warn!(%vault, id = %record.id, "cannot open record: {e}"),
                }
            }
            for p in pending.values() {
                let payload: PendingPayload = self.open_local(&p.payload)?;
                if let Some(entity) = payload.entity {
                    out.push(Item {
                        vault_id: vault,
                        id: p.id,
                        version: p.base_version,
                        pending: true,
                        entity,
                    });
                }
            }
        }
        if let Some(kind) = kind {
            out.retain(|i| i.entity.kind() == kind);
        }
        out.sort_by(|a, b| {
            a.entity
                .label()
                .to_lowercase()
                .cmp(&b.entity.label().to_lowercase())
        });
        Ok(out)
    }

    pub fn item(&self, vault: Uuid, id: Uuid) -> Result<Option<Item>> {
        Ok(self
            .items(None)?
            .into_iter()
            .find(|i| i.vault_id == vault && i.id == id))
    }

    /// Looks an item up by id in any vault.
    pub fn find(&self, id: Uuid) -> Result<Option<Item>> {
        Ok(self.items(None)?.into_iter().find(|i| i.id == id))
    }

    fn check_writable(&self, vault: &Uuid) -> Result<()> {
        let summary = self
            .store
            .vault(vault)?
            .ok_or_else(|| ClientError::invalid("unknown vault"))?;
        if !summary.role.can_write() {
            return Err(ClientError::invalid(
                "you have read-only access to this vault",
            ));
        }
        Ok(())
    }

    /// The server version and decrypted content an edit of `id` starts from.
    fn edit_base(&self, vault: &Uuid, id: &Uuid) -> Result<(u64, Option<Entity>)> {
        if let Some(p) = self.store.pending_one(vault, id)? {
            let payload: PendingPayload = self.open_local(&p.payload)?;
            return Ok((p.base_version, payload.base));
        }
        match self.store.record(vault, id)? {
            Some(record) => Ok((record.version, self.open_entity(vault, &record)?)),
            None => Ok((0, None)),
        }
    }

    /// Creates or replaces an item. Returns its id. Pushed on the next [`Vessel::sync`].
    pub fn save(&mut self, vault: Uuid, id: Option<Uuid>, entity: Entity) -> Result<Uuid> {
        self.unlocked()?;
        if entity.kind() == EntityKind::VaultMeta {
            return Err(ClientError::invalid(
                "vault metadata is edited with update_vault",
            ));
        }
        self.check_writable(&vault)?;
        let id = id.unwrap_or_else(vessel_model::new_id);
        let (base_version, base) = self.edit_base(&vault, &id)?;
        let payload = self.seal_local(&PendingPayload {
            entity: Some(entity),
            base,
        })?;
        self.store.put_pending(&PendingRow {
            vault_id: vault,
            id,
            base_version,
            deleted: false,
            payload,
        })?;
        Ok(id)
    }

    pub fn delete(&mut self, vault: Uuid, id: Uuid) -> Result<()> {
        self.unlocked()?;
        self.check_writable(&vault)?;
        let (base_version, base) = self.edit_base(&vault, &id)?;
        if base_version == 0 {
            // Never reached the server: just drop the local draft.
            return self.store.remove_pending(&vault, &id);
        }
        let payload = self.seal_local(&PendingPayload { entity: None, base })?;
        self.store.put_pending(&PendingRow {
            vault_id: vault,
            id,
            base_version,
            deleted: true,
            payload,
        })
    }

    /// Moves an item to another vault (a copy in the target, a deletion in the source).
    pub fn move_item(&mut self, from: Uuid, to: Uuid, id: Uuid) -> Result<Uuid> {
        let item = self
            .item(from, id)?
            .ok_or_else(|| ClientError::invalid("item not found"))?;
        self.check_writable(&to)?;
        let new_id = self.save(to, None, item.entity)?;
        self.delete(from, id)?;
        Ok(new_id)
    }

    pub fn pending_count(&self) -> Result<usize> {
        Ok(self.store.pending(None)?.len())
    }

    pub fn conflicts(&self) -> Result<Vec<ConflictView>> {
        self.unlocked()?;
        self.store
            .conflicts()?
            .into_iter()
            .map(|(id, vault_id, record_id, at, payload)| {
                let p: ConflictPayload = self.open_local(&payload)?;
                Ok(ConflictView {
                    id,
                    vault_id,
                    record_id,
                    at,
                    label: p.label,
                    conflicts: p.conflicts,
                })
            })
            .collect()
    }

    pub fn dismiss_conflict(&self, id: i64) -> Result<()> {
        self.store.remove_conflict(id)
    }
}
