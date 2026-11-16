//! Vault creation, verification of membership history and grants, sharing and key rotation.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use vessel_crypto::SymmetricKey;
use vessel_crypto::account::UnlockedAccount;
use vessel_crypto::envelope::{RecordContext, open_record, seal_record};
use vessel_crypto::grant::GrantParties;
use vessel_crypto::membership::{Action, Membership};
use vessel_model::{Entity, VaultMeta};
use vessel_protocol::*;

use crate::store::VaultColumn;
use crate::{ClientError, Result, Unlocked, Vessel};

/// A vault as shown to the user.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VaultView {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub color: String,
    pub personal: bool,
    pub role: Role,
    pub rotating: bool,
    pub member_count: usize,
    /// Current key version (grows by one per rotation).
    pub key_version: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemberView {
    pub user_id: Uuid,
    pub email: String,
    pub role: Role,
    pub fingerprint: String,
    pub is_me: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingView {
    pub user_id: Uuid,
    pub email: String,
    pub role: Role,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MembersView {
    pub members: Vec<MemberView>,
    pub pending: Vec<PendingView>,
}

/// Verified membership cached locally per vault.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct CachedMembership {
    pub roles: BTreeMap<Uuid, Role>,
    pub emails: BTreeMap<Uuid, String>,
    pub ever_managers: BTreeSet<Uuid>,
    /// Anchor of the history verified so far: later histories must extend it (no rollback,
    /// truncation or replacement by the server).
    #[serde(default)]
    pub anchor_seq: u64,
    #[serde(default)]
    pub anchor_head: Option<[u8; 32]>,
    #[serde(default)]
    pub creator: Option<Uuid>,
}

fn meta_context(vault_id: Uuid, key_version: u32) -> RecordContext {
    RecordContext {
        vault_id: *vault_id.as_bytes(),
        record_id: *vault_id.as_bytes(),
        key_version,
        revision: None,
    }
}

fn seal_meta(
    vk: &SymmetricKey,
    vault_id: Uuid,
    key_version: u32,
    meta: &VaultMeta,
) -> Result<Vec<u8>> {
    Ok(seal_record(
        vk,
        &meta_context(vault_id, key_version),
        &Entity::VaultMeta(meta.clone()).to_json(),
    )?)
}

/// Builds the request that creates a vault owned by `user_id`, with its fresh vault key.
pub(crate) fn new_vault_request(
    account: &UnlockedAccount,
    user_id: Uuid,
    name: &str,
    personal: bool,
) -> Result<(CreateVaultRequest, SymmetricKey)> {
    let vault_id = Uuid::now_v7();
    let vk = SymmetricKey::generate()?;
    let me = *user_id.as_bytes();
    let creation_event = Membership::default()
        .next_event(
            *vault_id.as_bytes(),
            Action::Create,
            me,
            Some(Role::Owner),
            me,
        )
        .sign(&account.keys);
    let grant = Grant::issue(
        GrantParties {
            granter_id: me,
            granter: &account.keys,
            recipient_id: me,
            recipient: &account.keys.public(),
        },
        *vault_id.as_bytes(),
        1,
        &vk,
    )?;
    let meta = seal_meta(
        &vk,
        vault_id,
        1,
        &VaultMeta {
            name: name.into(),
            ..Default::default()
        },
    )?;
    Ok((
        CreateVaultRequest {
            vault_id,
            personal,
            meta: meta.into(),
            creation_event,
            grant,
        },
        vk,
    ))
}

/// Opens every grant whose granter is trusted, adding the keys to `unlocked`.
pub(crate) fn open_grants(
    unlocked: &mut Unlocked,
    me: Uuid,
    vault_id: Uuid,
    cached: &GrantsResponse,
) {
    for grant in &cached.grants {
        // The grant authenticates its own vault id: never install it into another vault.
        if grant.vault_id != *vault_id.as_bytes() {
            tracing::warn!(%vault_id, "ignoring a grant issued for another vault");
            continue;
        }
        let Some(granter) = cached
            .granters
            .iter()
            .find(|g| g.user_id.as_bytes() == &grant.granter_id)
        else {
            continue;
        };
        match grant.open(me.as_bytes(), &unlocked.account.keys, &granter.public_keys) {
            Ok(key) => {
                // A key already installed for this version is never replaced by a different one.
                match unlocked.vault_keys.get(&(vault_id, grant.key_version)) {
                    Some(existing) if *existing != key => {
                        tracing::warn!(%vault_id, key_version = grant.key_version, "ignoring a conflicting grant");
                    }
                    Some(_) => {}
                    None => {
                        unlocked
                            .vault_keys
                            .insert((vault_id, grant.key_version), key);
                    }
                }
            }
            Err(e) => {
                tracing::warn!(%vault_id, key_version = grant.key_version, "ignoring grant: {e}")
            }
        }
    }
}

impl Vessel {
    pub(crate) fn vault_key(&self, vault_id: &Uuid, key_version: u32) -> Result<&SymmetricKey> {
        self.unlocked()?
            .vault_keys
            .get(&(*vault_id, key_version))
            .ok_or_else(|| {
                ClientError::invalid(format!("no key for vault {vault_id} v{key_version}"))
            })
    }

    fn summary(&self, vault_id: &Uuid) -> Result<VaultSummary> {
        self.store
            .vault(vault_id)?
            .ok_or_else(|| ClientError::invalid("unknown vault"))
    }

    fn cached_membership(&self, vault_id: &Uuid) -> Result<CachedMembership> {
        Ok(self
            .store
            .vault_blob(vault_id, VaultColumn::Membership)?
            .unwrap_or_default())
    }

    /// Fetches and verifies a vault's membership history; pins every user's keys.
    pub(crate) async fn verify_membership(
        &mut self,
        vault_id: Uuid,
    ) -> Result<(Membership, MembershipResponse)> {
        let response: MembershipResponse = self
            .api()?
            .get(&format!("/v1/vaults/{vault_id}/membership"))
            .await?;
        for user in response
            .users
            .iter()
            .chain(response.pending.iter().map(|p| &p.user))
        {
            self.check_pin(user)?;
        }
        let keys: BTreeMap<[u8; 16], UserPublicKeys> = response
            .users
            .iter()
            .map(|u| (*u.user_id.as_bytes(), u.public_keys))
            .collect();

        let mut anchor = self.cached_membership(&vault_id)?;
        // A new device starts from what the user's other devices verified (trust record).
        if let Some(t) = self.trusted_anchor(&vault_id)?
            && t.seq > anchor.anchor_seq
            && let Some(head) = crate::trust::anchor_head(&t)
        {
            anchor.anchor_seq = t.seq;
            anchor.anchor_head = Some(head);
            anchor.creator = t.creator.or(anchor.creator);
        }
        if anchor.anchor_seq > response.events.len() as u64 {
            return Err(ClientError::invalid(
                "the server returned a shorter membership history than the one already verified",
            ));
        }
        let creator = response
            .events
            .first()
            .map(|e| Uuid::from_bytes(e.body.actor))
            .ok_or_else(|| ClientError::invalid("empty membership history"))?;
        if anchor.creator.is_some_and(|c| c != creator) {
            return Err(ClientError::invalid(
                "the server returned a membership history with a different creator",
            ));
        }
        let mut state = Membership::default();
        let mut ever_managers = BTreeSet::new();
        for event in &response.events {
            let actor = keys.get(&event.body.actor).ok_or_else(|| {
                ClientError::invalid("membership history references an unknown user")
            })?;
            state.apply(vault_id.as_bytes(), event, actor)?;
            if state.next_seq == anchor.anchor_seq
                && anchor.anchor_head.is_some_and(|h| h != state.head)
            {
                return Err(ClientError::invalid(
                    "the server returned a membership history that rewrites events already verified",
                ));
            }
            if let Some(role) = state.role_of(&event.body.subject)
                && role.can_manage()
            {
                ever_managers.insert(Uuid::from_bytes(event.body.subject));
            }
        }
        let me = self.user_id()?;
        if state.role_of(me.as_bytes()).is_none() {
            return Err(ClientError::invalid(
                "verified history says you are not a member of this vault",
            ));
        }
        let cached = CachedMembership {
            roles: state
                .roles
                .iter()
                .map(|(k, v)| (Uuid::from_bytes(*k), *v))
                .collect(),
            emails: response
                .users
                .iter()
                .map(|u| (u.user_id, u.email.clone()))
                .collect(),
            ever_managers,
            anchor_seq: state.next_seq,
            anchor_head: Some(state.head),
            creator: Some(creator),
        };
        self.store
            .set_vault_blob(&vault_id, VaultColumn::Membership, &cached)?;
        Ok((state, response))
    }

    /// Fetches the caller's grants for a vault and opens those from trusted granters.
    pub(crate) async fn refresh_grants(&mut self, vault_id: Uuid) -> Result<()> {
        let mut response: GrantsResponse = self
            .api()?
            .get(&format!("/v1/vaults/{vault_id}/grants"))
            .await?;
        let membership = self.cached_membership(&vault_id)?;
        for granter in &response.granters {
            self.check_pin(granter)?;
        }
        let me = self.user_id()?;
        let unlocked = self.unlocked.as_mut().ok_or(ClientError::Locked)?;
        // Former managers signed the grants of the key versions they created, so those stay
        // valid. A new key version must come from someone who manages the vault now: otherwise a
        // removed or demoted manager (with the server's help) could hand out a key they know.
        let current = |g: &Uuid| membership.roles.get(g).is_some_and(|r| r.can_manage());
        let held = unlocked
            .vault_keys
            .keys()
            .filter(|(v, _)| *v == vault_id)
            .map(|(_, kv)| *kv)
            .max()
            .unwrap_or(0);
        let ceiling = response
            .grants
            .iter()
            .filter(|g| current(&Uuid::from_bytes(g.granter_id)))
            .map(|g| g.key_version)
            .max()
            .unwrap_or(0)
            .max(held);
        response.grants.retain(|g| {
            let granter = Uuid::from_bytes(g.granter_id);
            membership.ever_managers.contains(&granter)
                && (current(&granter) || g.key_version <= ceiling)
        });
        open_grants(unlocked, me, vault_id, &response);
        self.store
            .set_vault_blob(&vault_id, VaultColumn::Grants, &response)?;
        Ok(())
    }

    pub(crate) fn vault_meta(&self, summary: &VaultSummary) -> VaultMeta {
        let open = || -> Result<VaultMeta> {
            let vk = self.vault_key(&summary.vault_id, summary.meta_key_version)?;
            let plain = open_record(
                vk,
                &meta_context(summary.vault_id, summary.meta_key_version),
                summary.meta.as_slice(),
            )?;
            match Entity::from_json(&plain)? {
                Entity::VaultMeta(m) => Ok(m),
                _ => Err(ClientError::invalid("vault metadata has the wrong type")),
            }
        };
        open().unwrap_or_else(|_| VaultMeta {
            name: "Cofre (sem chave)".into(),
            ..Default::default()
        })
    }

    // -- public API -------------------------------------------------------------------------

    pub fn vaults(&self) -> Result<Vec<VaultView>> {
        self.unlocked()?;
        let personal = self.personal_vault().ok();
        let mut out: Vec<VaultView> = self
            .store
            .vaults()?
            .into_iter()
            .map(|s| {
                let meta = self.vault_meta(&s);
                let members = self
                    .cached_membership(&s.vault_id)
                    .map(|m| m.roles.len())
                    .unwrap_or(1);
                VaultView {
                    id: s.vault_id,
                    name: meta.name,
                    description: meta.description,
                    color: meta.color,
                    // Verified, not the server's flag (see personal_vault).
                    personal: Some(s.vault_id) == personal,
                    role: s.role,
                    rotating: s.state == VaultState::Rotating,
                    key_version: s.current_key_version,
                    member_count: members,
                }
            })
            .collect();
        out.sort_by(|a, b| {
            b.personal
                .cmp(&a.personal)
                .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(out)
    }

    /// The user's own vault. The server's `personal` flag alone is not trusted: the verified
    /// history must show this user as its creator and only member.
    pub fn personal_vault(&self) -> Result<Uuid> {
        let me = self.user_id()?;
        for v in self.store.vaults()?.into_iter().filter(|v| v.personal) {
            let m = self.cached_membership(&v.vault_id)?;
            let only_me = m.roles.len() == 1 && m.roles.get(&me) == Some(&Role::Owner);
            if only_me && m.creator.is_none_or(|c| c == me) {
                return Ok(v.vault_id);
            }
            tracing::warn!(vault = %v.vault_id, "ignoring a vault the server marks as personal");
        }
        Err(ClientError::invalid("personal vault not synced yet"))
    }

    pub async fn create_vault(&mut self, name: &str) -> Result<Uuid> {
        let me = self.user_id()?;
        let (req, vk) = new_vault_request(&self.unlocked()?.account, me, name, false)?;
        let vault_id = req.vault_id;
        let summary: VaultSummary = self.api()?.post("/v1/vaults", &req).await?;
        self.unlocked
            .as_mut()
            .ok_or(ClientError::Locked)?
            .vault_keys
            .insert((vault_id, 1), vk);
        self.store.put_vault(&summary)?;
        self.verify_membership(vault_id).await?;
        self.refresh_grants(vault_id).await?;
        self.persist_session().await?;
        Ok(vault_id)
    }

    pub async fn update_vault(&mut self, vault_id: Uuid, meta: VaultMeta) -> Result<()> {
        let kv = self.write_key_version(&vault_id)?;
        let sealed = seal_meta(self.vault_key(&vault_id, kv)?, vault_id, kv, &meta)?;
        self.api()?
            .put::<_, ()>(
                &format!("/v1/vaults/{vault_id}/meta"),
                &UpdateVaultMetaRequest {
                    meta: sealed.into(),
                    meta_key_version: kv,
                },
            )
            .await?;
        let summary: VaultSummary = self.api()?.get(&format!("/v1/vaults/{vault_id}")).await?;
        self.store.put_vault(&summary)?;
        self.persist_session().await
    }

    pub async fn delete_vault(&mut self, vault_id: Uuid) -> Result<()> {
        self.api()?
            .delete::<()>(&format!("/v1/vaults/{vault_id}"))
            .await?;
        self.store.remove_vault(&vault_id)?;
        self.persist_session().await
    }

    pub async fn members(&mut self, vault_id: Uuid) -> Result<MembersView> {
        let (state, response) = self.verify_membership(vault_id).await?;
        let me = self.user_id()?;
        let users: BTreeMap<Uuid, &UserPublic> =
            response.users.iter().map(|u| (u.user_id, u)).collect();
        let members = state
            .roles
            .iter()
            .filter_map(|(id, role)| {
                let id = Uuid::from_bytes(*id);
                users.get(&id).map(|u| MemberView {
                    user_id: id,
                    email: u.email.clone(),
                    role: *role,
                    fingerprint: u.public_keys.fingerprint().to_string(),
                    is_me: id == me,
                })
            })
            .collect();
        let pending = response
            .pending
            .iter()
            .map(|p| PendingView {
                user_id: p.user.user_id,
                email: p.user.email.clone(),
                role: p.role,
                fingerprint: p.user.public_keys.fingerprint().to_string(),
            })
            .collect();
        self.persist_session().await?;
        Ok(MembersView { members, pending })
    }

    pub async fn invite(
        &mut self,
        vault_id: Uuid,
        role: Role,
        email: Option<String>,
    ) -> Result<InviteResponse> {
        let invite = self
            .api()?
            .post(
                &format!("/v1/vaults/{vault_id}/invites"),
                &CreateInviteRequest { role, email },
            )
            .await?;
        self.persist_session().await?;
        Ok(invite)
    }

    /// Accepts a vault invite (token or full link). A manager must then approve you.
    pub async fn accept_invite(&mut self, token_or_link: &str) -> Result<Option<Uuid>> {
        let token = token_or_link
            .trim()
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_string();
        let resp: AcceptInviteResponse = self
            .api()?
            .post(&format!("/v1/invites/{token}/accept"), &())
            .await?;
        self.persist_session().await?;
        Ok(resp.vault_id)
    }

    async fn append_event(
        &mut self,
        vault_id: Uuid,
        action: Action,
        subject: Uuid,
        role: Option<Role>,
        grants: Vec<Grant>,
    ) -> Result<()> {
        let (state, _) = self.verify_membership(vault_id).await?;
        let me = self.user_id()?;
        let event = state
            .next_event(
                *vault_id.as_bytes(),
                action,
                *subject.as_bytes(),
                role,
                *me.as_bytes(),
            )
            .sign(&self.unlocked()?.account.keys);
        self.api()?
            .post::<_, ()>(
                &format!("/v1/vaults/{vault_id}/membership"),
                &MembershipChangeRequest { event, grants },
            )
            .await?;
        // After leaving, the vault is no longer readable: there is nothing left to verify.
        if !(action == Action::Remove && subject == me) {
            self.verify_membership(vault_id).await?;
        }
        self.persist_session().await
    }

    /// Approves a pending member: signs an `Add` event and grants them the vault key.
    /// Prefer [`Self::approve_member_verified`], which ties the approval to the fingerprint the
    /// person read out.
    pub async fn approve_member(&mut self, vault_id: Uuid, user_id: Uuid) -> Result<()> {
        self.approve(vault_id, user_id, None).await
    }

    /// Approves a pending member only if their keys still have `fingerprint`, the one the
    /// person read out over another channel. The vault key is sealed to exactly the keys that
    /// were verified, whatever the server lists as pending by the time of the approval.
    pub async fn approve_member_verified(
        &mut self,
        vault_id: Uuid,
        user_id: Uuid,
        fingerprint: &str,
    ) -> Result<()> {
        self.approve(vault_id, user_id, Some(fingerprint)).await
    }

    async fn approve(&mut self, vault_id: Uuid, user_id: Uuid, fingerprint: Option<&str>) -> Result<()> {
        let (_, response) = self.verify_membership(vault_id).await?;
        let pending = response
            .pending
            .iter()
            .find(|p| p.user.user_id == user_id)
            .ok_or_else(|| ClientError::invalid("this user has no pending invite"))?
            .clone();
        if let Some(expected) = fingerprint {
            let digits = |s: &str| s.chars().filter(char::is_ascii_digit).collect::<String>();
            let actual = pending.user.public_keys.fingerprint().to_string();
            if digits(expected).is_empty() || digits(expected) != digits(&actual) {
                return Err(ClientError::invalid(
                    "the fingerprint does not match the keys of the person waiting for approval",
                ));
            }
        }
        let kv = self.write_key_version(&vault_id)?;
        let me = self.user_id()?;
        let grant = Grant::issue(
            GrantParties {
                granter_id: *me.as_bytes(),
                granter: &self.unlocked()?.account.keys,
                recipient_id: *user_id.as_bytes(),
                recipient: &pending.user.public_keys,
            },
            *vault_id.as_bytes(),
            kv,
            self.vault_key(&vault_id, kv)?,
        )?;
        self.append_event(
            vault_id,
            Action::Add,
            user_id,
            Some(pending.role),
            vec![grant],
        )
        .await
    }

    pub async fn change_role(&mut self, vault_id: Uuid, user_id: Uuid, role: Role) -> Result<()> {
        self.append_event(
            vault_id,
            Action::ChangeRole,
            user_id,
            Some(role),
            Vec::new(),
        )
        .await
    }

    /// Removes a member and rotates the vault key so they cannot read anything written later.
    pub async fn remove_member(&mut self, vault_id: Uuid, user_id: Uuid) -> Result<()> {
        self.append_event(vault_id, Action::Remove, user_id, None, Vec::new())
            .await?;
        self.rotate_vault(vault_id).await
    }

    pub async fn leave_vault(&mut self, vault_id: Uuid) -> Result<()> {
        let me = self.user_id()?;
        self.append_event(vault_id, Action::Remove, me, None, Vec::new())
            .await?;
        self.store.remove_vault(&vault_id)?;
        Ok(())
    }

    /// Generates a new vault key, grants it to every current member and re-encrypts every
    /// record with it (`docs/ARCHITECTURE.md` §5.5). A rotation already in progress is finished
    /// first and then superseded: its key was handed to whoever was a member when it started,
    /// possibly including someone removed since.
    pub async fn rotate_vault(&mut self, vault_id: Uuid) -> Result<()> {
        self.pull_vault(vault_id).await?;
        if self.summary(&vault_id)?.state == VaultState::Rotating {
            self.finish_rotation(vault_id).await?;
            self.pull_vault(vault_id).await?;
        }
        let (state, response) = self.verify_membership(vault_id).await?;
        let summary = self.summary(&vault_id)?;
        if summary.state == VaultState::Rotating {
            return Err(ClientError::invalid(
                "another key rotation is in progress; try again",
            ));
        }
        let new_kv = summary.current_key_version + 1;
        let vk = SymmetricKey::generate()?;
        let me = self.user_id()?;
        let keys = &self.unlocked()?.account.keys;
        let users: BTreeMap<[u8; 16], UserPublicKeys> = response
            .users
            .iter()
            .map(|u| (*u.user_id.as_bytes(), u.public_keys))
            .collect();
        let grants = state
            .roles
            .keys()
            .map(|member| {
                let recipient = users
                    .get(member)
                    .ok_or_else(|| ClientError::invalid("member without keys"))?;
                Ok(Grant::issue(
                    GrantParties {
                        granter_id: *me.as_bytes(),
                        granter: keys,
                        recipient_id: *member,
                        recipient,
                    },
                    *vault_id.as_bytes(),
                    new_kv,
                    &vk,
                )?)
            })
            .collect::<Result<Vec<_>>>()?;
        let meta = seal_meta(&vk, vault_id, new_kv, &self.vault_meta(&summary))?;
        let summary: VaultSummary = self
            .api()?
            .post(
                &format!("/v1/vaults/{vault_id}/rotate/start"),
                &RotateStartRequest {
                    new_key_version: new_kv,
                    grants,
                    meta: meta.into(),
                },
            )
            .await?;
        self.unlocked
            .as_mut()
            .ok_or(ClientError::Locked)?
            .vault_keys
            .insert((vault_id, new_kv), vk);
        self.store.put_vault(&summary)?;
        self.refresh_grants(vault_id).await?;
        self.finish_rotation(vault_id).await
    }

    /// Re-encrypts records still under an old key, then closes the rotation.
    pub(crate) async fn finish_rotation(&mut self, vault_id: Uuid) -> Result<()> {
        for _ in 0..5 {
            self.pull_vault(vault_id).await?;
            let summary = self.summary(&vault_id)?;
            let kv = summary.current_key_version;
            let stale: Vec<RecordDto> = self
                .store
                .records(&vault_id)?
                .into_iter()
                .filter(|r| r.key_version < kv)
                .collect();
            if stale.is_empty() {
                let summary: VaultSummary = self
                    .api()?
                    .post(&format!("/v1/vaults/{vault_id}/rotate/finish"), &())
                    .await?;
                self.store.put_vault(&summary)?;
                return self.persist_session().await;
            }
            let mut ops = Vec::with_capacity(stale.len());
            for record in &stale {
                let entity = self.open_entity(&vault_id, record)?;
                let (key_version, blob) = self.seal_entity(
                    &vault_id,
                    &record.id,
                    entity.as_ref(),
                    record.version,
                    record.deleted,
                )?;
                ops.push(PushOp {
                    id: record.id,
                    base_version: record.version,
                    key_version,
                    deleted: record.deleted,
                    blob: blob.into(),
                });
            }
            for chunk in ops.chunks(MAX_BATCH) {
                let resp: PushResponse = self
                    .api()?
                    .post(
                        &format!("/v1/vaults/{vault_id}/changes"),
                        &PushRequest {
                            ops: chunk.to_vec(),
                        },
                    )
                    .await?;
                for r in resp.results {
                    if let PushResult::Rejected { id, reason } = r {
                        tracing::warn!(%vault_id, %id, "re-encryption rejected: {reason}");
                    }
                }
            }
        }
        Err(ClientError::invalid(
            "could not finish the key rotation; try again",
        ))
    }
}
