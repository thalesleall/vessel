//! What this account's devices verified (other people's keys, how far each vault's membership
//! history was checked), synced through the personal vault.
//!
//! Without it a new device would trust whatever keys and history the server shows first. With
//! it, the new device starts from what the user's other devices already verified: the record is
//! sealed with the personal vault's key, so the server can neither read nor forge it.

use std::collections::BTreeMap;

use uuid::Uuid;
use vessel_crypto::identity::UserPublicKeys;
use vessel_model::{Entity, EntityKind, Trust, TrustAnchor, TrustedUser};

use crate::error::{ClientError, Result};
use crate::store::VaultColumn;
use crate::vaults::CachedMembership;
use crate::{PIN_EMAILS_KEY, PINS_KEY, Vessel};

/// Anchors learned from the trust record, used when this device has none of its own.
const TRUST_ANCHORS_KEY: &str = "trust_anchors";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        out[i] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
    }
    Some(out)
}

impl Vessel {
    fn trust_item(&self) -> Result<Option<(Uuid, Trust)>> {
        let personal = self.personal_vault()?;
        let mut found: Vec<(Uuid, Trust)> = self
            .items(Some(EntityKind::Trust))?
            .into_iter()
            .filter(|i| i.vault_id == personal)
            .filter_map(|i| match i.entity {
                Entity::Trust(t) => Some((i.id, t)),
                _ => None,
            })
            .collect();
        // Two devices may each have created one: use the oldest id, deterministically.
        found.sort_by_key(|(id, _)| *id);
        Ok(found.into_iter().next())
    }

    /// The anchor the trust record holds for a vault, if any.
    pub(crate) fn trusted_anchor(&self, vault: &Uuid) -> Result<Option<TrustAnchor>> {
        let anchors: BTreeMap<Uuid, TrustAnchor> =
            self.store.get(TRUST_ANCHORS_KEY)?.unwrap_or_default();
        Ok(anchors.get(vault).cloned())
    }

    /// Takes in what the user's other devices verified. A key that differs from one this
    /// device pinned is the same alarm as a key change seen from the server.
    pub(crate) fn absorb_trust(&mut self) -> Result<()> {
        let Some((_, trust)) = self.trust_item()? else {
            return Ok(());
        };
        let mut pins = self.pins()?;
        let mut emails: BTreeMap<String, Uuid> =
            self.store.get(PIN_EMAILS_KEY)?.unwrap_or_default();
        for (id, user) in &trust.users {
            let (Some(kem), Some(sign)) = (unhex32(&user.kem), unhex32(&user.sign)) else {
                continue;
            };
            let keys = UserPublicKeys { kem, sign };
            match pins.get(id) {
                Some(pinned) if *pinned != keys => {
                    return Err(ClientError::KeyChanged {
                        email: user.email.clone(),
                    });
                }
                Some(_) => {}
                None => {
                    pins.insert(*id, keys);
                }
            }
            let email = vessel_protocol::normalize_email(&user.email);
            if !email.is_empty() {
                match emails.get(&email) {
                    Some(other) if other != id => {
                        return Err(ClientError::KeyChanged {
                            email: user.email.clone(),
                        });
                    }
                    Some(_) => {}
                    None => {
                        emails.insert(email, *id);
                    }
                }
            }
        }
        self.store.set(PINS_KEY, &pins)?;
        self.store.set(PIN_EMAILS_KEY, &emails)?;
        let mut anchors: BTreeMap<Uuid, TrustAnchor> =
            self.store.get(TRUST_ANCHORS_KEY)?.unwrap_or_default();
        for (vault, anchor) in trust.anchors {
            if anchors.get(&vault).is_none_or(|a| a.seq < anchor.seq) {
                anchors.insert(vault, anchor);
            }
        }
        self.store.set(TRUST_ANCHORS_KEY, &anchors)
    }

    /// Writes what this device verified into the trust record when it adds anything.
    pub(crate) async fn publish_trust(&mut self) -> Result<()> {
        let Ok(personal) = self.personal_vault() else {
            return Ok(());
        };
        let existing = self.trust_item()?;
        let mut trust = existing.as_ref().map(|(_, t)| t.clone()).unwrap_or_default();
        let before = trust.clone();

        let emails: BTreeMap<String, Uuid> = self.store.get(PIN_EMAILS_KEY)?.unwrap_or_default();
        let email_of: BTreeMap<Uuid, String> = emails.into_iter().map(|(e, id)| (id, e)).collect();
        for (id, keys) in self.pins()? {
            trust.users.entry(id).or_insert_with(|| TrustedUser {
                email: email_of.get(&id).cloned().unwrap_or_default(),
                kem: hex(&keys.kem),
                sign: hex(&keys.sign),
            });
        }
        for summary in self.store.vaults()? {
            let cached: CachedMembership = self
                .store
                .vault_blob(&summary.vault_id, VaultColumn::Membership)?
                .unwrap_or_default();
            let Some(head) = cached.anchor_head else { continue };
            if trust
                .anchors
                .get(&summary.vault_id)
                .is_none_or(|a| a.seq < cached.anchor_seq)
            {
                trust.anchors.insert(
                    summary.vault_id,
                    TrustAnchor {
                        seq: cached.anchor_seq,
                        head: hex(&head),
                        creator: cached.creator,
                    },
                );
            }
        }
        if existing.is_some() && trust == before {
            return Ok(());
        }
        self.save(personal, existing.map(|(id, _)| id), Entity::Trust(trust))?;
        self.push_vault(personal).await?;
        Ok(())
    }
}

pub(crate) fn anchor_head(anchor: &TrustAnchor) -> Option<[u8; 32]> {
    unhex32(&anchor.head)
}

#[cfg(test)]
mod tests {
    #[test]
    fn hex_round_trips() {
        let bytes = [0xab_u8; 32];
        assert_eq!(super::unhex32(&super::hex(&bytes)), Some(bytes));
        assert_eq!(super::unhex32("zz"), None);
    }
}
