//! A user's long-term key pairs: X25519 (HPKE) to receive vault keys, Ed25519 to sign grants
//! and membership events.
//!
//! Both private keys are derived from 32-byte seeds, so the secret part of an identity is just
//! 64 bytes and the public keys can always be recomputed from it.

use core::fmt;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use hpke::{Deserializable, Kem as _, Serializable};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::secret::random_bytes;
use crate::{CryptoError, Result};

pub(crate) type Kem = hpke::kem::X25519HkdfSha256;
type KemPrivateKey = <Kem as hpke::Kem>::PrivateKey;
pub(crate) type KemPublicKey = <Kem as hpke::Kem>::PublicKey;

pub const PUBLIC_KEY_LEN: usize = 32;
pub const SECRET_LEN: usize = 64;
pub const SIGNATURE_LEN: usize = 64;
pub const FINGERPRINT_LEN: usize = 20;

const FINGERPRINT_DOMAIN: &[u8] = b"vessel/v1/fingerprint";

/// The private half of a user identity. Never leaves the device unencrypted.
pub struct UserKeys {
    kem_seed: Zeroizing<[u8; 32]>,
    kem_sk: KemPrivateKey,
    kem_pk: KemPublicKey,
    signing: SigningKey,
}

impl UserKeys {
    pub fn generate() -> Result<Self> {
        let mut secret = Zeroizing::new([0u8; SECRET_LEN]);
        secret[..32].copy_from_slice(&random_bytes::<32>()?);
        secret[32..].copy_from_slice(&random_bytes::<32>()?);
        Self::from_secret_bytes(&*secret)
    }

    /// Rebuilds an identity from the 64 bytes produced by [`UserKeys::to_secret_bytes`].
    pub fn from_secret_bytes(secret: &[u8]) -> Result<Self> {
        if secret.len() != SECRET_LEN {
            return Err(CryptoError::InvalidKey);
        }
        let mut kem_seed = Zeroizing::new([0u8; 32]);
        kem_seed.copy_from_slice(&secret[..32]);
        let mut sign_seed = [0u8; 32];
        sign_seed.copy_from_slice(&secret[32..]);

        let (kem_sk, kem_pk) = Kem::derive_keypair(&*kem_seed);
        let signing = SigningKey::from_bytes(&sign_seed);
        sign_seed.zeroize();
        Ok(Self {
            kem_seed,
            kem_sk,
            kem_pk,
            signing,
        })
    }

    /// `kem_seed || ed25519_seed`. Encrypt this with the account key before storing it.
    pub fn to_secret_bytes(&self) -> Zeroizing<[u8; SECRET_LEN]> {
        let mut out = Zeroizing::new([0u8; SECRET_LEN]);
        out[..32].copy_from_slice(&*self.kem_seed);
        out[32..].copy_from_slice(&self.signing.to_bytes());
        out
    }

    pub fn public(&self) -> UserPublicKeys {
        let mut kem = [0u8; PUBLIC_KEY_LEN];
        kem.copy_from_slice(&self.kem_pk.to_bytes());
        UserPublicKeys {
            kem,
            sign: self.signing.verifying_key().to_bytes(),
        }
    }

    pub fn sign(&self, message: &[u8]) -> [u8; SIGNATURE_LEN] {
        self.signing.sign(message).to_bytes()
    }

    pub(crate) fn kem_private(&self) -> &KemPrivateKey {
        &self.kem_sk
    }
}

impl fmt::Debug for UserKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UserKeys")
            .field("public", &self.public())
            .finish_non_exhaustive()
    }
}

/// The public half of a user identity, published through the agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UserPublicKeys {
    /// X25519 public key for HPKE.
    pub kem: [u8; PUBLIC_KEY_LEN],
    /// Ed25519 verifying key.
    pub sign: [u8; PUBLIC_KEY_LEN],
}

impl UserPublicKeys {
    /// Verifies an Ed25519 signature made by [`UserKeys::sign`]. Uses strict verification
    /// (rejects malleable and small-order encodings).
    pub fn verify(&self, message: &[u8], signature: &[u8]) -> Result<()> {
        let signature: [u8; SIGNATURE_LEN] = signature
            .try_into()
            .map_err(|_| CryptoError::InvalidSignature)?;
        VerifyingKey::from_bytes(&self.sign)
            .map_err(|_| CryptoError::InvalidKey)?
            .verify_strict(message, &Signature::from_bytes(&signature))
            .map_err(|_| CryptoError::InvalidSignature)
    }

    pub fn fingerprint(&self) -> Fingerprint {
        let digest = Sha256::new()
            .chain_update(FINGERPRINT_DOMAIN)
            .chain_update(self.kem)
            .chain_update(self.sign)
            .finalize();
        let mut out = [0u8; FINGERPRINT_LEN];
        out.copy_from_slice(&digest[..FINGERPRINT_LEN]);
        Fingerprint(out)
    }

    pub(crate) fn kem_public(&self) -> Result<KemPublicKey> {
        KemPublicKey::from_bytes(&self.kem).map_err(|_| CryptoError::InvalidKey)
    }
}

/// Verifies a bare Ed25519 signature (e.g. a device key signing a refresh request).
pub fn verify_ed25519(public_key: &[u8], message: &[u8], signature: &[u8]) -> Result<()> {
    let public_key: [u8; PUBLIC_KEY_LEN] =
        public_key.try_into().map_err(|_| CryptoError::InvalidKey)?;
    let signature: [u8; SIGNATURE_LEN] = signature
        .try_into()
        .map_err(|_| CryptoError::InvalidSignature)?;
    VerifyingKey::from_bytes(&public_key)
        .map_err(|_| CryptoError::InvalidKey)?
        .verify_strict(message, &Signature::from_bytes(&signature))
        .map_err(|_| CryptoError::InvalidSignature)
}

/// A standalone Ed25519 key, used as a per-device key.
pub struct DeviceKey(SigningKey);

impl DeviceKey {
    pub fn generate() -> Result<Self> {
        let seed = Zeroizing::new(random_bytes::<32>()?);
        Ok(Self(SigningKey::from_bytes(&seed)))
    }

    pub fn from_seed(seed: &[u8]) -> Result<Self> {
        let seed: [u8; 32] = seed.try_into().map_err(|_| CryptoError::InvalidKey)?;
        Ok(Self(SigningKey::from_bytes(&seed)))
    }

    pub fn seed(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.0.to_bytes())
    }

    pub fn public_key(&self) -> [u8; PUBLIC_KEY_LEN] {
        self.0.verifying_key().to_bytes()
    }

    pub fn sign(&self, message: &[u8]) -> [u8; SIGNATURE_LEN] {
        self.0.sign(message).to_bytes()
    }
}

impl fmt::Debug for DeviceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DeviceKey(<redacted>)")
    }
}

/// A short, human-comparable digest of a user's public keys (see `docs/ARCHITECTURE.md` §5.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Fingerprint(pub [u8; FINGERPRINT_LEN]);

impl fmt::Display for Fingerprint {
    /// Five groups of five digits, e.g. `04821 99130 57702 31468 80915`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, chunk) in self.0.as_chunks::<4>().0.iter().enumerate() {
            let n = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) % 100_000;
            if i > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{n:05}")?;
        }
        Ok(())
    }
}
