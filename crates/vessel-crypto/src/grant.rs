//! Vault key grants: how a vault key reaches each member (see `docs/ARCHITECTURE.md` §5.4).
//!
//! The vault key is sealed to the recipient's X25519 key with HPKE (base mode,
//! X25519-HKDF-SHA256 / HKDF-SHA256 / ChaCha20-Poly1305) and the whole grant is signed with the
//! granter's Ed25519 key. The HPKE `info` binds the grant to vault, key version and recipient; the
//! signature lets the recipient check who issued it.
//!
//! This module only proves *who* signed a grant. Whether that user was allowed to grant access
//! is decided by the signed membership history, which lives above this crate.

use hpke::{Deserializable, OpModeR, OpModeS, Serializable};
use serde::{Deserialize, Serialize};

use crate::identity::{FINGERPRINT_LEN, Fingerprint, Kem, UserKeys, UserPublicKeys};
use crate::secret::SymmetricKey;
use crate::{CryptoError, Id, Result};

type Aead = hpke::aead::ChaCha20Poly1305;
type Kdf = hpke::kdf::HkdfSha256;
type EncappedKey = <Kem as hpke::Kem>::EncappedKey;

pub const GRANT_V1: u8 = 1;

const INFO_DOMAIN: &[u8] = b"vessel/v1/vk-grant";
const SIGNATURE_DOMAIN: &[u8] = b"vessel/v1/vk-grant-signature";

/// A vault key sealed to one member. Safe to store and serve from the agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub version: u8,
    pub vault_id: Id,
    pub key_version: u32,
    pub recipient_id: Id,
    pub recipient_fingerprint: [u8; FINGERPRINT_LEN],
    pub granter_id: Id,
    pub encapped_key: Vec<u8>,
    pub wrapped_vault_key: Vec<u8>,
    pub signature: Vec<u8>,
}

/// The identities a grant is issued between.
#[derive(Clone, Copy, Debug)]
pub struct GrantParties<'a> {
    pub granter_id: Id,
    pub granter: &'a UserKeys,
    pub recipient_id: Id,
    pub recipient: &'a UserPublicKeys,
}

impl Grant {
    /// Seals `vault_key` to the recipient and signs the result as the granter.
    pub fn issue(
        parties: GrantParties<'_>,
        vault_id: Id,
        key_version: u32,
        vault_key: &SymmetricKey,
    ) -> Result<Self> {
        let recipient_fingerprint = parties.recipient.fingerprint().0;
        let info = hpke_info(&vault_id, key_version, &parties.recipient_id);
        let aad = hpke_aad(&parties.granter_id, &recipient_fingerprint);

        let (encapped, wrapped_vault_key) = hpke::single_shot_seal::<Aead, Kdf, Kem>(
            &OpModeS::Base,
            &parties.recipient.kem_public()?,
            &info,
            vault_key.expose(),
            &aad,
        )
        .map_err(|_| CryptoError::Encrypt)?;

        let mut grant = Self {
            version: GRANT_V1,
            vault_id,
            key_version,
            recipient_id: parties.recipient_id,
            recipient_fingerprint,
            granter_id: parties.granter_id,
            encapped_key: encapped.to_bytes().to_vec(),
            wrapped_vault_key,
            signature: Vec::new(),
        };
        grant.signature = parties.granter.sign(&grant.signed_message()).to_vec();
        Ok(grant)
    }

    /// Checks that the grant was signed by `granter`.
    pub fn verify(&self, granter: &UserPublicKeys) -> Result<()> {
        if self.version != GRANT_V1 {
            return Err(CryptoError::UnsupportedVersion(self.version));
        }
        granter.verify(&self.signed_message(), &self.signature)
    }

    /// Verifies the signature and recovers the vault key as the recipient.
    pub fn open(
        &self,
        recipient_id: &Id,
        recipient: &UserKeys,
        granter: &UserPublicKeys,
    ) -> Result<SymmetricKey> {
        self.verify(granter)?;
        if &self.recipient_id != recipient_id {
            return Err(CryptoError::GrantMismatch("recipient id"));
        }
        if Fingerprint(self.recipient_fingerprint) != recipient.public().fingerprint() {
            return Err(CryptoError::GrantMismatch("recipient keys"));
        }

        let encapped = EncappedKey::from_bytes(&self.encapped_key)
            .map_err(|_| CryptoError::Malformed("encapsulated key"))?;
        let vault_key = hpke::single_shot_open::<Aead, Kdf, Kem>(
            &OpModeR::Base,
            recipient.kem_private(),
            &encapped,
            &hpke_info(&self.vault_id, self.key_version, &self.recipient_id),
            &self.wrapped_vault_key,
            &hpke_aad(&self.granter_id, &self.recipient_fingerprint),
        )
        .map_err(|_| CryptoError::Decrypt)?;
        let key = SymmetricKey::from_slice(&vault_key);
        drop(zeroize::Zeroizing::new(vault_key));
        key
    }

    /// Canonical, length-prefixed encoding of every field except the signature.
    fn signed_message(&self) -> Vec<u8> {
        let mut m =
            Vec::with_capacity(160 + self.encapped_key.len() + self.wrapped_vault_key.len());
        m.extend_from_slice(SIGNATURE_DOMAIN);
        m.push(self.version);
        m.extend_from_slice(&self.vault_id);
        m.extend_from_slice(&self.key_version.to_be_bytes());
        m.extend_from_slice(&self.recipient_id);
        m.extend_from_slice(&self.recipient_fingerprint);
        m.extend_from_slice(&self.granter_id);
        for field in [&self.encapped_key, &self.wrapped_vault_key] {
            m.extend_from_slice(&(field.len() as u32).to_be_bytes());
            m.extend_from_slice(field);
        }
        m
    }
}

fn hpke_info(vault_id: &Id, key_version: u32, recipient_id: &Id) -> Vec<u8> {
    [
        INFO_DOMAIN,
        vault_id,
        &key_version.to_be_bytes(),
        recipient_id,
    ]
    .concat()
}

fn hpke_aad(granter_id: &Id, recipient_fingerprint: &[u8; FINGERPRINT_LEN]) -> Vec<u8> {
    [granter_id.as_slice(), recipient_fingerprint].concat()
}
