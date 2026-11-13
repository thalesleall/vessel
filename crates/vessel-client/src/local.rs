//! Local-only mode (no sync server) and the migrations between it and a server.
//!
//! In local mode the account keys, the personal vault and every item live only in the local
//! store, encrypted exactly as in server mode; the password unlocks them offline and the
//! recovery key resets the password. Migrations never drop data:
//!
//! - Every item is decrypted into memory first, then re-queued, sealed with the account key, in
//!   the new store (`import_queue`) before anything is synced. An interrupted import resumes on
//!   the next unlock or sync.
//! - The caller should take a [`Vessel::backup`] first; the copy stays encrypted.
//! - Items that already exist on the other side (same content) are not duplicated; references
//!   to them are rewritten to the surviving copy.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use reqwest::Method;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use vessel_crypto::SymmetricKey;
use vessel_crypto::account::{
    AccountKeyMaterial, LocalUnlock, RecoveryKey, UnlockedAccount, create_account,
    create_local_unlock, new_recovery_key, rewrap_for_master, unlock_with_recovery,
};
use vessel_crypto::identity::DeviceKey;
use vessel_crypto::kdf::KdfParams;
use vessel_crypto::opaque::{OpaqueIdentity, client_registration_start};
use vessel_crypto::wrap::{WrapPurpose, seal_bytes};
use vessel_model::{Entity, EntityKind};
use vessel_protocol::*;

use crate::api::Api;
use crate::store::VaultColumn;
use crate::vaults::{CachedMembership, new_vault_request};
use crate::{
    ClientError, DeviceSecrets, PINS_KEY, PROFILE_KEY, Profile, Result, Unlocked, Vessel,
    account_keys, server_info,
};

const IMPORT_KEY: &str = "import_queue";

/// What a migration did, for the UI.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationReport {
    /// Items written into the destination.
    pub imported: usize,
    /// Items that already existed there with the same content.
    pub skipped_duplicates: usize,
    /// Set when the previous recovery key stopped working; show it to the user once.
    pub recovery_key: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct ImportQueue {
    entries: Vec<(Uuid, Entity)>,
}

impl Vessel {
    /// Starts using Vessel on this computer only. Returns the recovery key to show the user.
    pub fn create_local(&mut self, name: &str, password: &str) -> Result<String> {
        // The master KEK only matters on a server; local mode unlocks with the password.
        let unused_master = SymmetricKey::generate()?;
        let (material, account, recovery) = create_account(&unused_master)?;
        let local_unlock =
            create_local_unlock(&account, password.as_bytes(), KdfParams::default())?;
        self.become_local(
            local_name(name),
            Uuid::now_v7(),
            material,
            local_unlock,
            account,
            Vec::new(),
        )?;
        Ok(recovery.to_string())
    }

    /// Sets a new password in local mode using the recovery key.
    pub fn recover_local(&mut self, recovery_key: &str, new_password: &str) -> Result<()> {
        let mut profile = self.profile()?.clone();
        if !profile.local {
            return Err(ClientError::invalid(
                "this device uses a server; recover the account through it",
            ));
        }
        let recovery: RecoveryKey = recovery_key.parse()?;
        let account = unlock_with_recovery(&profile.material, &recovery).map_err(|_| {
            ClientError::invalid("this recovery key does not belong to this account")
        })?;
        profile.local_unlock =
            create_local_unlock(&account, new_password.as_bytes(), KdfParams::default())?;
        self.store.set(PROFILE_KEY, &profile)?;
        let user_id = profile.user_id;
        self.profile = Some(profile);
        self.install_unlocked(user_id, account)?;
        self.resume_import()?;
        Ok(())
    }

    /// Writes an encrypted copy of the local database to `path` (taken before migrations).
    pub fn backup(&self, path: &Path) -> Result<()> {
        self.store.backup(path)
    }

    /// Moves this local-only setup to a new account on a server. The account keeps its keys,
    /// so nothing is re-encrypted; it gets a new recovery key, returned in the report.
    pub async fn connect_new_account(
        &mut self,
        server_url: &str,
        email: &str,
        password: &str,
        device_name: &str,
        invite_token: Option<String>,
    ) -> Result<MigrationReport> {
        self.require_local()?;
        let carry = self.snapshot()?;
        let profile = self.profile()?.clone();

        let info = server_info(server_url).await?;
        let email = normalize_email(email);
        let api = Api::new(server_url)?;
        let identity = OpaqueIdentity {
            user: email.as_bytes(),
            server: info.server_id.as_bytes(),
        };
        let (state, request) = client_registration_start(password.as_bytes())?;
        let start: RegisterStartResponse = api
            .public(
                Method::POST,
                "/v1/auth/register/start",
                Some(&RegisterStartRequest {
                    email: email.clone(),
                    request: request.into(),
                    invite_token,
                }),
            )
            .await?;
        let (upload, master_kek) = state.finish(
            password.as_bytes(),
            start.response.as_slice(),
            &identity,
            &start.kdf,
        )?;

        let account_ref = &self.unlocked()?.account;
        // The server stores a hash of a recovery proof, which needs the recovery key itself;
        // issuing a new one avoids asking the user for the old key.
        let (wrapped_recovery, recovery) = new_recovery_key(account_ref)?;
        let mut keys: AccountKeyMaterial = profile.material.clone();
        keys.wrapped_account_key_master = rewrap_for_master(account_ref, &master_kek)?;
        keys.wrapped_account_key_recovery = wrapped_recovery;
        let (personal_vault, _vk) = new_vault_request(account_ref, start.user_id, "Pessoal", true)?;
        let device_key = DeviceKey::generate()?;
        let auth: AuthResponse = api
            .public(
                Method::POST,
                "/v1/auth/register/finish",
                Some(&RegisterFinishRequest {
                    registration_id: start.registration_id,
                    upload: upload.into(),
                    account: account_keys(&keys),
                    recovery_auth_token: Bytes(recovery.auth_token()?.to_vec()),
                    personal_vault,
                    device: NewDevice {
                        name: device_name.into(),
                        public_key: Bytes(device_key.public_key().to_vec()),
                    },
                }),
            )
            .await?;

        let account = self.unlocked.take().ok_or(ClientError::Locked)?.account;
        self.import_stats = (0, 0);
        self.finish_auth(api, info, email, password, auth, account, device_key, carry)
            .await?;
        Ok(MigrationReport {
            imported: self.import_stats.0,
            skipped_duplicates: self.import_stats.1,
            recovery_key: Some(recovery.to_string()),
        })
    }

    /// Signs in to an existing server account and adds everything from local mode to its
    /// personal vault. That account's own password and recovery key apply from then on.
    pub async fn connect_existing_account(
        &mut self,
        server_url: &str,
        email: &str,
        password: &str,
        device_name: &str,
    ) -> Result<MigrationReport> {
        self.require_local()?;
        let carry = self.snapshot()?;
        self.import_stats = (0, 0);
        self.login_with(server_url, email, password, device_name, carry)
            .await?;
        Ok(MigrationReport {
            imported: self.import_stats.0,
            skipped_duplicates: self.import_stats.1,
            recovery_key: None,
        })
    }

    /// Leaves the server and keeps a local-only copy of every item this device can see
    /// (personal and shared vaults). Password and recovery key stay the same.
    pub async fn disconnect_to_local(&mut self) -> Result<MigrationReport> {
        if self.is_local() {
            return Err(ClientError::invalid("this device is already in local mode"));
        }
        self.unlocked()?;
        let carry = self.snapshot()?;
        let profile = self.profile()?.clone();
        if let Ok(api) = self.api() {
            // Best effort: end this device's session on the server.
            let _ = api.post::<_, ()>("/v1/auth/logout", &()).await;
        }
        let account = self.unlocked.take().ok_or(ClientError::Locked)?.account;
        self.import_stats = (0, 0);
        self.become_local(
            local_name(&profile.email),
            profile.user_id,
            profile.material,
            profile.local_unlock,
            account,
            carry,
        )?;
        Ok(MigrationReport {
            imported: self.import_stats.0,
            skipped_duplicates: self.import_stats.1,
            recovery_key: None,
        })
    }

    // -- internals ----------------------------------------------------------------------------

    fn require_local(&self) -> Result<()> {
        self.unlocked()?;
        if !self.is_local() {
            return Err(ClientError::invalid("this device already uses a server"));
        }
        Ok(())
    }

    /// Every item, decrypted.
    fn snapshot(&self) -> Result<Vec<(Uuid, Entity)>> {
        Ok(self
            .items(None)?
            .into_iter()
            .filter(|i| i.entity.kind() != EntityKind::VaultMeta)
            .map(|i| (i.id, i.entity))
            .collect())
    }

    /// Replaces the local store with a local-only profile and a fresh personal vault, then
    /// imports `carry` into it.
    fn become_local(
        &mut self,
        name: String,
        user_id: Uuid,
        material: AccountKeyMaterial,
        local_unlock: LocalUnlock,
        account: UnlockedAccount,
        carry: Vec<(Uuid, Entity)>,
    ) -> Result<()> {
        let device_key = DeviceKey::generate()?;
        let secrets = DeviceSecrets {
            device_seed: device_key.seed().to_vec(),
            refresh_token: String::new(),
        };
        let profile = Profile {
            server_url: String::new(),
            server_id: String::new(),
            email: name.clone(),
            user_id,
            device_id: Uuid::now_v7(),
            device_secrets: seal_bytes(
                &account.account_key,
                WrapPurpose::DeviceSecrets,
                &serde_json::to_vec(&secrets)?,
            )?,
            material,
            local_unlock,
            is_server_admin: false,
            local: true,
        };
        let (req, vk) = new_vault_request(&account, user_id, "Pessoal", true)?;
        let public = account.keys.public();

        self.store.wipe()?;
        self.api = None;
        self.store.set(PROFILE_KEY, &profile)?;
        self.store
            .set(PINS_KEY, &BTreeMap::from([(user_id, public)]))?;
        self.store.put_vault(&VaultSummary {
            vault_id: req.vault_id,
            personal: true,
            role: Role::Owner,
            current_key_version: 1,
            state: VaultState::Active,
            meta: req.meta.clone(),
            meta_key_version: 1,
            last_seq: 0,
        })?;
        self.store.set_vault_blob(
            &req.vault_id,
            VaultColumn::Grants,
            &GrantsResponse {
                grants: vec![req.grant.clone()],
                granters: vec![UserPublic {
                    user_id,
                    email: name.clone(),
                    public_keys: public,
                }],
            },
        )?;
        self.store.set_vault_blob(
            &req.vault_id,
            VaultColumn::Membership,
            &CachedMembership {
                roles: BTreeMap::from([(user_id, Role::Owner)]),
                emails: BTreeMap::from([(user_id, name)]),
                ever_managers: BTreeSet::from([user_id]),
            },
        )?;
        self.profile = Some(profile);
        self.unlocked = Some(Unlocked {
            account,
            vault_keys: HashMap::from([((req.vault_id, 1), vk)]),
        });
        self.queue_import(carry)?;
        self.resume_import()?;
        Ok(())
    }

    /// Adds entries to the encrypted import queue.
    pub(crate) fn queue_import(&mut self, entries: Vec<(Uuid, Entity)>) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let mut all = self.queued_imports()?;
        all.extend(entries);
        let sealed = self.seal_local(&ImportQueue { entries: all })?;
        self.store.set(IMPORT_KEY, &sealed)
    }

    fn queued_imports(&self) -> Result<Vec<(Uuid, Entity)>> {
        match self.store.get::<Vec<u8>>(IMPORT_KEY)? {
            Some(sealed) => Ok(self.open_local::<ImportQueue>(&sealed)?.entries),
            None => Ok(Vec::new()),
        }
    }

    /// Imports the queue into the personal vault if it is available. Returns how many items
    /// were written. Safe to call repeatedly.
    pub(crate) fn resume_import(&mut self) -> Result<usize> {
        if self.unlocked.is_none() {
            return Ok(0);
        }
        let entries = self.queued_imports()?;
        if entries.is_empty() {
            return Ok(0);
        }
        let Ok(vault) = self.personal_vault() else {
            return Ok(0);
        };
        let Some(summary) = self.store.vault(&vault)? else {
            return Ok(0);
        };
        if self.vault_key(&vault, summary.current_key_version).is_err() {
            return Ok(0); // keys not synced yet; try again after the next sync
        }
        let (imported, skipped) = self.import_entities(vault, entries)?;
        self.store.remove(IMPORT_KEY)?;
        self.import_stats.0 += imported;
        self.import_stats.1 += skipped;
        Ok(imported)
    }

    /// Writes entries into `vault`, skipping ones whose content already exists and pointing
    /// references at the surviving copy.
    fn import_entities(
        &mut self,
        vault: Uuid,
        entries: Vec<(Uuid, Entity)>,
    ) -> Result<(usize, usize)> {
        let existing = self.items(None)?;
        let existing_ids: BTreeSet<Uuid> = existing.iter().map(|i| i.id).collect();
        let has_settings = existing
            .iter()
            .any(|i| i.entity.kind() == EntityKind::Settings);
        let mut remap: HashMap<String, String> = HashMap::new();
        let mut keep: Vec<(Uuid, Entity)> = Vec::new();
        let mut skipped = 0;
        for (id, entity) in entries {
            if existing_ids.contains(&id) || keep.iter().any(|(k, _)| *k == id) {
                skipped += 1; // already there (e.g. an import that resumed)
                continue;
            }
            if entity.kind() == EntityKind::Settings
                && (has_settings || keep.iter().any(|(_, e)| e.kind() == EntityKind::Settings))
            {
                skipped += 1;
                continue;
            }
            let duplicate = existing
                .iter()
                .find(|i| i.entity == entity)
                .map(|i| i.id)
                .or_else(|| keep.iter().find(|(_, e)| *e == entity).map(|(k, _)| *k));
            if let Some(survivor) = duplicate {
                remap.insert(id.to_string(), survivor.to_string());
                skipped += 1;
                continue;
            }
            keep.push((id, entity));
        }
        let mut imported = 0;
        for (id, entity) in keep {
            let entity = if remap.is_empty() {
                entity
            } else {
                let mut json = serde_json::to_string(&entity)?;
                for (from, to) in &remap {
                    json = json.replace(from.as_str(), to.as_str());
                }
                serde_json::from_str(&json)?
            };
            self.save(vault, Some(id), entity)?;
            imported += 1;
        }
        Ok((imported, skipped))
    }
}

fn local_name(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        "Este computador".to_string()
    } else {
        name.to_string()
    }
}
