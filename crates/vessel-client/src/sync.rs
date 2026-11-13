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
        let remote: Vec<VaultSummary> = self.api()?.get("/v1/vaults").await?;
        let remote_ids: BTreeSet<Uuid> = remote.iter().map(|v| v.vault_id).collect();
        for local in self.store.vaults()? {
            if !remote_ids.contains(&local.vault_id) {
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
            let (pushed, conflicts) = self.push_vault(vault).await?;
            report.pushed += pushed;
            report.conflicts += conflicts;

            let summary = self.store.vault(&vault)?.unwrap_or(summary);
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
        self.persist_session().await?;
        Ok(report)
    }

    /// Pulls every change after the vault's cursor. Returns the number of records received.
    pub(crate) async fn pull_vault(&mut self, vault: Uuid) -> Result<usize> {
        let mut total = 0;
        loop {
            let since = self.store.cursor(&vault)?;
            let page: ChangesPage = self
                .api()?
                .get(&format!("/v1/vaults/{vault}/changes?since={since}"))
                .await?;
            total += page.changes.len();
            for record in &page.changes {
                self.store.put_record(&vault, record)?;
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
                let (key_version, blob) =
                    self.seal_entity(&vault, &p.id, payload.entity.as_ref())?;
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
        self.store.put_record(&vault, &current)?;
        let Some(pending) = self.store.pending_one(&vault, &id)? else {
            return Ok(0);
        };
        let payload: PendingPayload = self.open_local(&pending.payload)?;
        let remote = self.open_entity(&vault, &current)?;

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
