//! The sync engine: refresh vaults and keys, pull changes, push the local queue, merge conflicts.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use vessel_model::merge;
use vessel_protocol::*;

use crate::items::{ConflictPayload, PendingPayload};
use crate::store::PendingRow;
use crate::{ClientError, Result, Vessel};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncReport {
    pub pulled: usize,
    pub pushed: usize,
    pub conflicts: usize,
    pub vaults: usize,
    /// Unsent edits of a vault that disappeared (deleted, or access lost), kept as new items
    /// in the personal vault.
    pub rescued: usize,
}

impl Vessel {
    /// Full sync of every vault. Safe to call often; does nothing heavy when nothing changed.
    pub async fn sync(&mut self) -> Result<SyncReport> {
        self.unlocked()?;
        let mut report = SyncReport::default();
        if self.is_local() {
            // Nothing to sync with; just finish any interrupted import.
            self.resume_import()?;
            return Ok(report);
        }
        let mut remote: Vec<VaultSummary> = self.api()?.get("/v1/vaults").await?;
        // The personal vault first: its trust record vouches for the keys and histories of
        // the others. (The server's personal flag only orders; it is verified as usual.)
        remote.sort_by_key(|v| !v.personal);
        let remote_ids: BTreeSet<Uuid> = remote.iter().map(|v| v.vault_id).collect();
        for local in self.store.vaults()? {
            if !remote_ids.contains(&local.vault_id) {
                report.rescued += self.rescue_pending(local.vault_id)?;
                self.store.remove_vault(&local.vault_id)?;
            }
        }
        for summary in remote {
            let vault = summary.vault_id;
            let needs_keys = self.vault_key(&vault, summary.current_key_version).is_err()
                || self.vault_key(&vault, summary.meta_key_version).is_err();
            let changed = self.store.vault(&vault)?.as_ref() != Some(&summary);
            self.store.put_vault(&summary)?;
            if changed || needs_keys {
                match self.verify_membership(vault).await {
                    Ok(_) => self.refresh_grants(vault).await?,
                    Err(e @ ClientError::KeyChanged { .. }) => return Err(e),
                    Err(e) => {
                        tracing::error!(%vault, "cannot verify vault: {e}");
                        continue;
                    }
                }
            }
            report.vaults += 1;
            report.pulled += self.pull_vault(vault).await?;
            if self.personal_vault().is_ok_and(|p| p == vault) {
                self.absorb_trust()?;
            }
            let (pushed, conflicts) = self.push_vault(vault).await?;
            report.pushed += pushed;
            report.conflicts += conflicts;

            let summary = self.store.vault(&vault)?.unwrap_or(summary);
            if let Err(e) = self.upgrade_envelopes(vault, &summary).await {
                tracing::warn!(%vault, "envelope upgrade: {e}");
            }
            if summary.role.can_manage() {
                if summary.rotation_due {
                    // Someone left or was removed and the vault still uses a key they may hold.
                    if let Err(e) = self.rotate_vault(vault).await {
                        tracing::warn!(%vault, "key rotation after a removal failed: {e}");
                    }
                } else if summary.state == VaultState::Rotating
                    && let Err(e) = self.finish_rotation(vault).await
                {
                    tracing::warn!(%vault, "rotation still in progress: {e}");
                }
            }
        }
        // Items carried over from local mode go into the personal vault once it exists here.
        if self.resume_import()? > 0
            && let Ok(personal) = self.personal_vault()
        {
            let (pushed, conflicts) = self.push_vault(personal).await?;
            report.pushed += pushed;
            report.conflicts += conflicts;
        }
        if let Err(e) = self.publish_trust().await {
            tracing::warn!("could not save the trust record: {e}");
        }
        self.persist_session().await?;
        Ok(report)
    }

    /// Edits not yet sent to a vault that is gone would be lost with it: they become new items
    /// in the personal vault. Returns how many.
    fn rescue_pending(&mut self, vault: Uuid) -> Result<usize> {
        let Ok(personal) = self.personal_vault() else {
            return Ok(0);
        };
        if personal == vault {
            return Ok(0);
        }
        let mut rescued = 0;
        for p in self.store.pending(Some(&vault))? {
            if p.deleted {
                continue;
            }
            let payload: PendingPayload = self.open_local(&p.payload)?;
            if let Some(entity) = payload.entity {
                self.save(personal, None, entity)?;
                rescued += 1;
            }
        }
        if rescued > 0 {
            tracing::warn!(%vault, rescued, "vault gone; unsent edits kept in the personal vault");
        }
        Ok(rescued)
    }

    /// Rewrites format 1 envelopes as format 2 (writers only, a batch per sync), and marks the
    /// vault as upgraded once this device holds no format 1 record of it.
    async fn upgrade_envelopes(&mut self, vault: Uuid, summary: &VaultSummary) -> Result<()> {
        if self.v2_vaults()?.contains(&vault) {
            return Ok(());
        }
        let legacy: Vec<RecordDto> = self
            .store
            .records(&vault)?
            .into_iter()
            .filter(|r| {
                vessel_crypto::envelope::format_version(r.blob.as_slice())
                    == Some(vessel_crypto::envelope::FORMAT_V1)
            })
            .collect();
        if legacy.is_empty() {
            let mut set = self.v2_vaults()?;
            set.insert(vault);
            return self.store.set(crate::items::V2_VAULTS_KEY, &set);
        }
        if !summary.role.can_write() || summary.state == VaultState::Rotating {
            return Ok(());
        }
        let mut ops = Vec::new();
        for record in legacy.iter().take(MAX_BATCH) {
            let entity = self.open_entity(&vault, record)?;
            let (key_version, blob) =
                self.seal_entity(&vault, &record.id, entity.as_ref(), record.version, record.deleted)?;
            ops.push(PushOp {
                id: record.id,
                base_version: record.version,
                key_version,
                deleted: record.deleted,
                blob: blob.into(),
            });
        }
        let _: PushResponse = self
            .api()?
            .post(&format!("/v1/vaults/{vault}/changes"), &PushRequest { ops })
            .await?;
        // Conflicts are fine: someone else's newer write is already format 2.
        self.pull_vault(vault).await?;
        Ok(())
    }

    /// Pulls every change after the vault's cursor. Returns the number of records received.
    pub(crate) async fn pull_vault(&mut self, vault: Uuid) -> Result<usize> {
        let mut total = 0;
        let mut refreshed_keys = false;
        loop {
            let since = self.store.cursor(&vault)?;
            let page: ChangesPage = self
                .api()?
                .get(&format!("/v1/vaults/{vault}/changes?since={since}"))
                .await?;
            total += page.changes.len();
            let mut done = since;
            for record in &page.changes {
                if self.unverifiable_replacement(&vault, record)? {
                    // The vault key may have rotated since this sync fetched the grants.
                    if !refreshed_keys {
                        refreshed_keys = true;
                        if let Err(e) = self.refresh_grants(vault).await {
                            tracing::warn!(%vault, "could not refresh the vault keys: {e}");
                        }
                    }
                    if self.unverifiable_replacement(&vault, record)? {
                        // Still no key: the local copy stays, and the pull stops here so the
                        // change is fetched again once the key arrives (the next sync verifies
                        // the membership and the grants first).
                        tracing::warn!(%vault, id = %record.id, "a change uses a vault key this device does not hold yet; will retry");
                        self.store.set_cursor(&vault, done)?;
                        return Ok(total);
                    }
                }
                // A copy that fails authentication (forged, replayed, or relabeled by the
                // server) never replaces what this device has.
                if let Err(e @ ClientError::Crypto(_)) = self.open_remote(&vault, record) {
                    tracing::warn!(%vault, id = %record.id, "refusing a record from the server: {e}");
                } else {
                    self.store.put_record(&vault, record)?;
                }
                done = done.max(record.seq);
            }
            self.store.set_cursor(&vault, page.next_seq)?;
            if !page.has_more {
                return Ok(total);
            }
        }
    }

    /// Pushes queued edits of one vault, merging on conflicts. Returns (pushed, conflicts).
    pub(crate) async fn push_vault(&mut self, vault: Uuid) -> Result<(usize, usize)> {
        let mut pushed = 0;
        let mut conflicts = 0;
        let mut over_quota = false;
        let can_write = self
            .store
            .vault(&vault)?
            .is_some_and(|s| s.role.can_write());
        for _round in 0..3 {
            let queue = self.store.pending(Some(&vault))?;
            if queue.is_empty() {
                break;
            }
            if !can_write {
                // Lost write access: the edits can never be pushed.
                for p in queue {
                    self.store.remove_pending(&vault, &p.id)?;
                }
                break;
            }
            let mut ops = Vec::with_capacity(queue.len());
            for p in &queue {
                let payload: PendingPayload = self.open_local(&p.payload)?;
                let (key_version, blob) = self.seal_entity(
                    &vault,
                    &p.id,
                    payload.entity.as_ref(),
                    p.base_version,
                    p.deleted,
                )?;
                ops.push(PushOp {
                    id: p.id,
                    base_version: p.base_version,
                    key_version,
                    deleted: p.deleted,
                    blob: blob.into(),
                });
            }
            let mut retry = false;
            for chunk in ops.chunks(MAX_BATCH) {
                let resp: PushResponse = self
                    .api()?
                    .post(
                        &format!("/v1/vaults/{vault}/changes"),
                        &PushRequest {
                            ops: chunk.to_vec(),
                        },
                    )
                    .await?;
                for (result, op) in resp.results.into_iter().zip(chunk) {
                    if !answers(op, &result) {
                        tracing::warn!(%vault, id = %op.id, "the server's answer does not match the edit sent; the edit stays queued");
                        continue;
                    }
                    match result {
                        PushResult::Ok { id, version, seq } => {
                            self.store.put_record(
                                &vault,
                                &RecordDto {
                                    id,
                                    version,
                                    seq,
                                    key_version: op.key_version,
                                    deleted: op.deleted,
                                    blob: op.blob.clone(),
                                    updated_at: crate::api::now(),
                                    updated_by_device: Uuid::nil(),
                                },
                            )?;
                            self.store.remove_pending(&vault, &id)?;
                            pushed += 1;
                        }
                        PushResult::Conflict { id, current } => {
                            conflicts += self.resolve_conflict(vault, id, current)?;
                            retry = true;
                        }
                        PushResult::Rejected { id, reason } => {
                            tracing::warn!(%vault, %id, "push rejected: {reason}");
                            if reason.contains("key version") {
                                // The vault key rotated; refresh keys and push again.
                                self.refresh_grants(vault).await?;
                                let summary: VaultSummary =
                                    self.api()?.get(&format!("/v1/vaults/{vault}")).await?;
                                self.store.put_vault(&summary)?;
                                retry = true;
                            } else if reason.contains("quota") {
                                // Kept: it goes up once the owner frees space or gets more.
                                over_quota = true;
                            } else {
                                self.store.remove_pending(&vault, &id)?;
                            }
                        }
                    }
                }
            }
            if !retry {
                break;
            }
        }
        if over_quota {
            return Err(ClientError::Api {
                status: 507,
                code: ErrorCode::QuotaExceeded,
                message: "the vault owner's storage quota is used up; edits wait on this device".into(),
            });
        }
        Ok((pushed, conflicts))
    }

    /// Re-bases a rejected edit on the server's current version. Returns the number of field
    /// conflicts recorded for the user.
    fn resolve_conflict(&mut self, vault: Uuid, id: Uuid, current: RecordDto) -> Result<usize> {
        // The server's copy is checked before it may replace this device's: a forged or
        // unverifiable one (e.g. a deletion under a key version this device does not hold) is
        // refused, and the local record and the queued edit stay as they were.
        let remote = self.open_remote(&vault, &current)?;
        self.store.put_record(&vault, &current)?;
        let Some(pending) = self.store.pending_one(&vault, &id)? else {
            return Ok(0);
        };
        let payload: PendingPayload = self.open_local(&pending.payload)?;

        let (entity, field_conflicts) = match (payload.entity, remote) {
            // Both deleted, or we deleted something the other side edited: their edit wins.
            (None, None) => (None, Vec::new()),
            (None, Some(remote)) => {
                self.store.remove_pending(&vault, &id)?;
                let label = remote.label().to_string();
                let note = vec![vessel_model::FieldConflict {
                    field: "*".into(),
                    local: serde_json::Value::String("excluído neste dispositivo".into()),
                    remote: serde_json::to_value(&remote)?,
                }];
                self.record_conflict(vault, id, label, note)?;
                return Ok(1);
            }
            // They deleted something we edited: restore our edit.
            (Some(local), None) => (Some(local), Vec::new()),
            (Some(local), Some(remote)) => {
                let outcome = merge(payload.base.as_ref(), &local, &remote);
                (Some(outcome.merged), outcome.conflicts)
            }
        };
        let Some(entity) = entity else {
            self.store.remove_pending(&vault, &id)?;
            return Ok(0);
        };
        let n = field_conflicts.len();
        if n > 0 {
            self.record_conflict(vault, id, entity.label().to_string(), field_conflicts)?;
        }
        let base = self.open_entity(&vault, &current)?;
        let sealed = self.seal_local(&PendingPayload {
            entity: Some(entity),
            base,
        })?;
        self.store.put_pending(&PendingRow {
            vault_id: vault,
            id,
            base_version: current.version,
            deleted: false,
            payload: sealed,
        })?;
        Ok(n)
    }

    fn record_conflict(
        &self,
        vault: Uuid,
        id: Uuid,
        label: String,
        conflicts: Vec<vessel_model::FieldConflict>,
    ) -> Result<()> {
        let sealed = self.seal_local(&ConflictPayload { label, conflicts })?;
        self.store.add_conflict(&vault, &id, &sealed)
    }
}

/// Whether a push answer is about the operation sent in its position and stores it as sent.
/// An answer naming another record would drop or overwrite an edit the server was never sent.
/// An edit acknowledged under another version would no longer open on this device, because the
/// envelope authenticates `base_version + 1`.
fn answers(op: &PushOp, result: &PushResult) -> bool {
    match result {
        PushResult::Ok { id, version, .. } => *id == op.id && *version == op.base_version + 1,
        PushResult::Conflict { id, current } => *id == op.id && current.id == op.id,
        PushResult::Rejected { id, .. } => *id == op.id,
    }
}

#[cfg(test)]
mod server_copy_tests {
    use super::answers;
    use crate::model::{Entity, Host};
    use crate::{ClientError, Vessel};
    use uuid::Uuid;
    use vessel_crypto::envelope::{RecordContext, seal_record};
    use vessel_protocol::{Bytes, PushOp, PushResult};

    async fn with_host() -> (Vessel, Uuid, Uuid) {
        let addr = vessel_agent::spawn_for_tests(vessel_agent::Config::for_tests())
            .await
            .unwrap();
        let mut v = Vessel::in_memory().unwrap();
        v.signup(&format!("http://{addr}"), "a@example.com", "correct horse battery staple", "pc", None)
            .await
            .unwrap();
        let vault = v.vaults().unwrap()[0].id;
        let host = Host { label: "prod-db".into(), address: "10.0.0.5".into(), ..Default::default() };
        let id = v.save(vault, None, Entity::Host(host)).unwrap();
        v.sync().await.unwrap();
        (v, vault, id)
    }

    fn labels(v: &Vessel) -> Vec<String> {
        v.items(None).unwrap().into_iter().map(|i| i.entity.label().to_string()).collect()
    }

    /// The pull path: a server marks a live record deleted under a key version this device
    /// was never granted, so there is nothing to authenticate the deletion against.
    #[tokio::test]
    async fn a_deletion_under_an_unknown_key_version_is_refused() {
        let (v, vault, id) = with_host().await;
        let mut forged = v.store.record(&vault, &id).unwrap().unwrap();
        forged.deleted = true;
        forged.key_version = 999;
        forged.version += 1;
        assert!(matches!(v.open_remote(&vault, &forged), Err(ClientError::Crypto(_))));
    }

    /// The same relabeling without the deletion flag: an unreadable copy must not replace the
    /// readable one this device has (the item would vanish from the lists just the same).
    #[tokio::test]
    async fn an_unverifiable_copy_does_not_replace_the_local_record() {
        let (v, vault, id) = with_host().await;
        let mut forged = v.store.record(&vault, &id).unwrap().unwrap();
        forged.key_version = 999;
        forged.version += 1;
        assert!(v.unverifiable_replacement(&vault, &forged).unwrap());
        assert!(matches!(v.open_remote(&vault, &forged), Err(ClientError::Crypto(_))));
        // A record this device has no copy of is kept for when its key arrives.
        forged.id = Uuid::now_v7();
        assert!(!v.unverifiable_replacement(&vault, &forged).unwrap());
        assert!(!matches!(v.open_remote(&vault, &forged), Err(ClientError::Crypto(_))));
    }

    /// Format 1 envelopes authenticate neither the record version nor the deletion flag, so the
    /// server could replay one (sealed with the right key) over a current-format record.
    #[tokio::test]
    async fn an_old_format_copy_does_not_replace_a_current_one() {
        let (v, vault, id) = with_host().await;
        let current = v.store.record(&vault, &id).unwrap().unwrap();
        let key = v.vault_key(&vault, current.key_version).unwrap();
        let context = RecordContext {
            vault_id: *vault.as_bytes(),
            record_id: *id.as_bytes(),
            key_version: current.key_version,
            revision: None,
        };
        let old_host = Host { label: "old".into(), address: "10.9.9.9".into(), ..Default::default() };
        let mut replayed = current.clone();
        replayed.version += 1;
        replayed.blob = Bytes(seal_record(key, &context, &Entity::Host(old_host).to_json()).unwrap());
        assert!(matches!(v.open_remote(&vault, &replayed), Err(ClientError::Crypto(_))));
        // The same envelope marked deleted is refused too.
        replayed.deleted = true;
        assert!(matches!(v.open_remote(&vault, &replayed), Err(ClientError::Crypto(_))));
    }

    /// A push answer must be about the edit sent, stored under the version it was sealed for.
    #[test]
    fn push_answers_are_matched_to_the_edit_sent() {
        let op = PushOp { id: Uuid::now_v7(), base_version: 4, key_version: 1, deleted: false, blob: Bytes(vec![1]) };
        let other = Uuid::now_v7();
        let ok = |id, version| PushResult::Ok { id, version, seq: 9 };
        assert!(answers(&op, &ok(op.id, 5)));
        assert!(!answers(&op, &ok(other, 5)), "acknowledged another record");
        assert!(!answers(&op, &ok(op.id, 6)), "acknowledged under another version");
        assert!(answers(&op, &PushResult::Rejected { id: op.id, reason: "x".into() }));
        assert!(!answers(&op, &PushResult::Rejected { id: other, reason: "x".into() }));
        let record = |id| vessel_protocol::RecordDto {
            id, version: 7, seq: 9, key_version: 1, deleted: false, blob: Bytes(vec![2]),
            updated_at: 0, updated_by_device: Uuid::nil(),
        };
        assert!(answers(&op, &PushResult::Conflict { id: op.id, current: record(op.id) }));
        assert!(!answers(&op, &PushResult::Conflict { id: op.id, current: record(other) }));
        assert!(!answers(&op, &PushResult::Conflict { id: other, current: record(other) }));
    }

    /// The fix must not refuse a genuine deletion.
    #[tokio::test]
    async fn a_genuine_deletion_is_still_accepted() {
        let (mut v, vault, id) = with_host().await;
        v.delete(vault, id).unwrap();
        v.sync().await.unwrap();
        let tombstone = v.store.record(&vault, &id).unwrap().unwrap();
        assert!(tombstone.deleted);
        assert!(v.open_remote(&vault, &tombstone).unwrap().is_none());
        assert!(!labels(&v).contains(&"prod-db".to_string()));
    }

    /// The conflict path: the server answers a push with a forged "current" copy. It must not
    /// replace the local record, and the queued edit must survive for the next sync.
    #[tokio::test]
    async fn a_forged_conflict_copy_does_not_replace_the_local_record() {
        let (mut v, vault, id) = with_host().await;
        let before = v.store.record(&vault, &id).unwrap().unwrap();
        let edit = Host { label: "prod-db-2".into(), address: "10.0.0.6".into(), ..Default::default() };
        v.save(vault, Some(id), Entity::Host(edit)).unwrap();

        let mut forged = before.clone();
        forged.deleted = true;
        forged.key_version = 999;
        forged.version += 1;
        assert!(v.resolve_conflict(vault, id, forged).is_err());

        let after = v.store.record(&vault, &id).unwrap().unwrap();
        assert_eq!((after.version, after.deleted, after.key_version), (before.version, false, before.key_version));
        assert!(v.store.pending_one(&vault, &id).unwrap().is_some(), "the queued edit was lost");
        assert!(labels(&v).contains(&"prod-db-2".to_string()));
    }
}
