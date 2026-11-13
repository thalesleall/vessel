//! Signed, hash-chained membership history of a vault (see `docs/ARCHITECTURE.md` §7.3).
//!
//! Every change to a vault's members is an event signed by the member who made it and chained to
//! the previous event by hash. [`replay`] validates a whole chain and returns the resulting roles,
//! so a client never has to trust the server's member list, and the server uses the same function
//! to refuse invalid events before storing them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::identity::{UserKeys, UserPublicKeys};
use crate::{CryptoError, Id, Result};

pub const HASH_LEN: usize = 32;

const SIGNATURE_DOMAIN: &[u8] = b"vessel/v1/membership-event";
const HASH_DOMAIN: &[u8] = b"vessel/v1/membership-hash";

/// Vault roles, ordered by privilege.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Viewer = 1,
    Editor = 2,
    Admin = 3,
    Owner = 4,
}

impl Role {
    pub fn can_write(self) -> bool {
        self >= Role::Editor
    }

    pub fn can_manage(self) -> bool {
        self >= Role::Admin
    }

    fn code(self) -> u8 {
        self as u8
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// First event of every vault: the creator becomes its owner.
    Create,
    Add,
    ChangeRole,
    /// Removal by a manager, or a member leaving (actor == subject).
    Remove,
}

impl Action {
    fn code(self) -> u8 {
        match self {
            Self::Create => 1,
            Self::Add => 2,
            Self::ChangeRole => 3,
            Self::Remove => 4,
        }
    }
}

/// The signed part of a membership event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventBody {
    pub vault_id: Id,
    pub seq: u64,
    pub prev_hash: [u8; HASH_LEN],
    pub action: Action,
    pub subject: Id,
    /// New role for `Create`, `Add` and `ChangeRole`; `None` for `Remove`.
    pub role: Option<Role>,
    pub actor: Id,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedEvent {
    pub body: EventBody,
    pub signature: Vec<u8>,
}

impl EventBody {
    fn signing_bytes(&self) -> Vec<u8> {
        let mut m = Vec::with_capacity(SIGNATURE_DOMAIN.len() + 96);
        m.extend_from_slice(SIGNATURE_DOMAIN);
        m.extend_from_slice(&self.vault_id);
        m.extend_from_slice(&self.seq.to_be_bytes());
        m.extend_from_slice(&self.prev_hash);
        m.push(self.action.code());
        m.extend_from_slice(&self.subject);
        m.push(self.role.map_or(0, Role::code));
        m.extend_from_slice(&self.actor);
        m
    }

    pub fn sign(self, actor_keys: &UserKeys) -> SignedEvent {
        let signature = actor_keys.sign(&self.signing_bytes()).to_vec();
        SignedEvent {
            body: self,
            signature,
        }
    }
}

impl SignedEvent {
    /// Hash linking this event to the next one.
    pub fn hash(&self) -> [u8; HASH_LEN] {
        let digest = Sha256::new()
            .chain_update(HASH_DOMAIN)
            .chain_update(self.body.signing_bytes())
            .chain_update(&self.signature)
            .finalize();
        let mut out = [0u8; HASH_LEN];
        out.copy_from_slice(&digest);
        out
    }
}

/// The state of a vault after replaying its history.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Membership {
    pub roles: BTreeMap<Id, Role>,
    pub next_seq: u64,
    pub head: [u8; HASH_LEN],
}

impl Membership {
    pub fn role_of(&self, user: &Id) -> Option<Role> {
        self.roles.get(user).copied()
    }

    /// Builds the body of the next event, ready to be signed by `actor`.
    pub fn next_event(
        &self,
        vault_id: Id,
        action: Action,
        subject: Id,
        role: Option<Role>,
        actor: Id,
    ) -> EventBody {
        EventBody {
            vault_id,
            seq: self.next_seq,
            prev_hash: self.head,
            action,
            subject,
            role,
            actor,
        }
    }

    /// Validates one more event against the current state and applies it.
    pub fn apply(
        &mut self,
        vault_id: &Id,
        event: &SignedEvent,
        actor_keys: &UserPublicKeys,
    ) -> Result<()> {
        let b = &event.body;
        let bad = CryptoError::Malformed;
        if &b.vault_id != vault_id {
            return Err(bad("membership event for another vault"));
        }
        if b.seq != self.next_seq || b.prev_hash != self.head {
            return Err(bad("membership chain is broken"));
        }
        actor_keys.verify(&b.signing_bytes(), &event.signature)?;

        let actor_role = self.role_of(&b.actor);
        let subject_role = self.role_of(&b.subject);
        match (b.action, b.role) {
            (Action::Create, Some(Role::Owner)) if b.seq == 0 && b.actor == b.subject => {}
            (Action::Add, Some(role)) => {
                let actor = actor_role.ok_or(bad("actor is not a member"))?;
                // Same rule as ChangeRole: only the owner can make someone an admin.
                if !actor.can_manage()
                    || subject_role.is_some()
                    || role == Role::Owner
                    || (role >= Role::Admin && actor != Role::Owner)
                {
                    return Err(bad("add is not allowed"));
                }
            }
            (Action::ChangeRole, Some(role)) => {
                let actor = actor_role.ok_or(bad("actor is not a member"))?;
                let subject = subject_role.ok_or(bad("subject is not a member"))?;
                let allowed = actor.can_manage()
                    && role != Role::Owner
                    && subject != Role::Owner
                    && (actor == Role::Owner || (role < Role::Admin && subject < Role::Admin));
                if !allowed {
                    return Err(bad("role change is not allowed"));
                }
            }
            (Action::Remove, None) => {
                let subject = subject_role.ok_or(bad("subject is not a member"))?;
                let leaving = b.actor == b.subject;
                let actor = actor_role.ok_or(bad("actor is not a member"))?;
                let allowed = subject != Role::Owner
                    && (leaving
                        || (actor.can_manage() && (actor == Role::Owner || subject < Role::Admin)));
                if !allowed {
                    return Err(bad("removal is not allowed"));
                }
            }
            _ => return Err(bad("invalid membership event")),
        }

        match b.role {
            Some(role) => self.roles.insert(b.subject, role),
            None => self.roles.remove(&b.subject),
        };
        self.next_seq += 1;
        self.head = event.hash();
        Ok(())
    }
}

/// Replays a vault's full history. `public_keys` resolves the signing keys of each actor.
pub fn replay<'a>(
    vault_id: &Id,
    events: impl IntoIterator<Item = &'a SignedEvent>,
    mut public_keys: impl FnMut(&Id) -> Option<UserPublicKeys>,
) -> Result<Membership> {
    let mut state = Membership::default();
    for event in events {
        let keys = public_keys(&event.body.actor).ok_or(CryptoError::Malformed("unknown actor"))?;
        state.apply(vault_id, event, &keys)?;
    }
    if state.next_seq == 0 {
        return Err(CryptoError::Malformed("empty membership history"));
    }
    Ok(state)
}
