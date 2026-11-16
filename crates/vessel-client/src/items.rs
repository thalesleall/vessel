//! Reading and editing the decrypted contents of vaults. Edits are queued locally and pushed by
//! the next sync, so everything works offline.

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use vessel_crypto::envelope::{
    FORMAT_V1, RecordContext, Revision, format_version, open_record, seal_record,
};
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

fn ctx(vault: &Uuid, id: &Uuid, key_version: u32, version: u64, deleted: bool) -> RecordContext {
    RecordContext {
        vault_id: *vault.as_bytes(),
        record_id: *id.as_bytes(),
        key_version,
        revision: Some(Revision { version, deleted }),
    }
}

/// Local key: vaults whose records this device has seen entirely in envelope format 2. From
/// then on a format 1 envelope there is a server replaying an old copy, and is refused.
pub(crate) const V2_VAULTS_KEY: &str = "v2_vaults";

impl Vessel {
    pub(crate) fn v2_vaults(&self) -> Result<std::collections::BTreeSet<Uuid>> {
        Ok(self.store.get(V2_VAULTS_KEY)?.unwrap_or_default())
    }

    /// Whether a record from the server is one this device cannot check: it is sealed under a
    /// key version this device does not hold, and it would replace a copy this device has.
    /// Legitimately that only happens while a key rotation is under way and the new grant has
    /// not been fetched yet, so the caller fetches grants and tries again later.
    pub(crate) fn unverifiable_replacement(&self, vault: &Uuid, record: &RecordDto) -> Result<bool> {
        Ok(self.vault_key(vault, record.key_version).is_err()
            && self.store.record(vault, &record.id)?.is_some())
    }

    /// Opens a record that just came from the server, before it may replace what this device
    /// has. Stricter than [`Self::open_entity`], which also reads records already stored:
    ///
    /// - A record under a key version this device does not hold has nothing to be checked
    ///   against. It never replaces a local copy, and a deletion is never taken on faith:
    ///   otherwise a server could hide any record by relabeling it with a made-up key version.
    /// - A format 1 envelope (which authenticates neither the record version nor the deletion
    ///   flag) never replaces a format 2 copy: every app writes format 2, so that is a replay.
    pub(crate) fn open_remote(&self, vault: &Uuid, record: &RecordDto) -> Result<Option<Entity>> {
        let refuse = |why: &'static str| Err(vessel_crypto::CryptoError::Malformed(why).into());
        let legacy = format_version(record.blob.as_slice()) == Some(FORMAT_V1);
        let local = self.store.record(vault, &record.id)?;
        if self.vault_key(vault, record.key_version).is_err() && (local.is_some() || record.deleted) {
            return refuse("a record under a key version this device does not hold");
        }
        if legacy
            && local.is_some_and(|l| format_version(l.blob.as_slice()) != Some(FORMAT_V1))
        {
            return refuse("an old-format copy of a record this device has in the current format");
        }
        self.open_entity(vault, record)
    }

    pub(crate) fn open_entity(&self, vault: &Uuid, record: &RecordDto) -> Result<Option<Entity>> {
        let legacy = format_version(record.blob.as_slice()) == Some(FORMAT_V1);
        if legacy && self.v2_vaults()?.contains(vault) {
            return Err(vessel_crypto::CryptoError::Malformed(
                "an old-format copy of a record in a vault already upgraded (a replayed copy?)",
            )
            .into());
        }
        if record.deleted {
            // Format 2 deletions are authenticated when the key is held. Records from the server
            // go through `open_remote` first, which refuses a deletion whose key is not held.
            if !legacy && let Ok(key) = self.vault_key(vault, record.key_version) {
                open_record(
                    key,
                    &ctx(vault, &record.id, record.key_version, record.version, true),
                    record.blob.as_slice(),
                )?;
            }
            return Ok(None);
        }
        let key = self.vault_key(vault, record.key_version)?;
        let plain = open_record(
            key,
            &ctx(vault, &record.id, record.key_version, record.version, false),
            record.blob.as_slice(),
        )?;
        if plain.as_slice() == b"null" {
            return Ok(None);
        }
        Ok(Some(Entity::from_json(&plain)?))
    }

    /// Seals the next version of a record: `base_version + 1`, the version the server gives a
    /// write based on `base_version`.
    pub(crate) fn seal_entity(
        &self,
        vault: &Uuid,
        id: &Uuid,
        entity: Option<&Entity>,
        base_version: u64,
        deleted: bool,
    ) -> Result<(u32, Vec<u8>)> {
        let kv = self.write_key_version(vault)?;
        let json = entity.map_or_else(|| b"null".to_vec(), Entity::to_json);
        Ok((
            kv,
            seal_record(
                self.vault_key(vault, kv)?,
                &ctx(vault, id, kv, base_version + 1, deleted),
                &json,
            )?,
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
        match kind {
            Some(kind) => out.retain(|i| i.entity.kind() == kind),
            // The trust record is the app's own bookkeeping: only listed when asked for.
            None => out.retain(|i| i.entity.kind() != EntityKind::Trust),
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

    /// Looks an item up by id in any vault. Ids are chosen by clients and unique only per
    /// vault, so an id present in two vaults is refused instead of resolving to whichever copy
    /// comes first (another member could otherwise shadow one of the user's records).
    pub fn find(&self, id: Uuid) -> Result<Option<Item>> {
        let mut matches = self.items(None)?.into_iter().filter(|i| i.id == id);
        let first = matches.next();
        if first.is_some() && matches.next().is_some() {
            return Err(ClientError::invalid(
                "this id exists in more than one vault; refusing an ambiguous reference",
            ));
        }
        Ok(first)
    }

    /// The key version new writes to `vault` must use: the server's current version, but never
    /// lower than the newest verified key held for the vault (a server cannot roll writes back
    /// to a key a removed member still holds).
    pub(crate) fn write_key_version(&self, vault: &Uuid) -> Result<u32> {
        let summary = self
            .store
            .vault(vault)?
            .ok_or_else(|| ClientError::invalid("unknown vault"))?;
        let held = self
            .unlocked()?
            .vault_keys
            .keys()
            .filter(|(v, _)| v == vault)
            .map(|(_, kv)| *kv)
            .max()
            .unwrap_or(0);
        Ok(summary.current_key_version.max(held))
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
