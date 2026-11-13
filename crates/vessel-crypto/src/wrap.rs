//! Symmetric key wrapping and small secret blobs encrypted under a key-encryption key.
//!
//! ```text
//! wrapped = version(1) || nonce(24) || ciphertext || tag(16)
//! aad     = "vessel/v1/wrap/" || purpose || version
//! ```
//!
//! The purpose label keeps a blob from being accepted in another role, for example an account
//! key wrapped for recovery being presented as the encrypted private keys.

use zeroize::Zeroizing;

use crate::secret::SymmetricKey;
use crate::{CryptoError, Result, aead};

pub const FORMAT_V1: u8 = 1;

const AAD_DOMAIN: &[u8] = b"vessel/v1/wrap/";

/// What a wrapped blob is for. Part of the authenticated data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WrapPurpose {
    /// Account key wrapped by the master KEK derived from OPAQUE.
    AccountKeyByMaster,
    /// Account key wrapped by the recovery key.
    AccountKeyByRecovery,
    /// Account key wrapped by the device-local unlock key (offline unlock).
    AccountKeyByLocal,
    /// The user's private key seeds encrypted by the account key.
    UserPrivateKeys,
    /// Device secrets (device signing key, refresh token) encrypted by the account key.
    DeviceSecrets,
    /// Device-local data that must not sit on disk in clear (conflict history, caches).
    LocalData,
}

impl WrapPurpose {
    fn label(self) -> &'static [u8] {
        match self {
            Self::AccountKeyByMaster => b"account-key/master",
            Self::AccountKeyByRecovery => b"account-key/recovery",
            Self::AccountKeyByLocal => b"account-key/local",
            Self::UserPrivateKeys => b"user-private-keys",
            Self::DeviceSecrets => b"device-secrets",
            Self::LocalData => b"local-data",
        }
    }

    fn aad(self) -> Vec<u8> {
        [AAD_DOMAIN, self.label(), &[FORMAT_V1]].concat()
    }
}

pub fn seal_bytes(kek: &SymmetricKey, purpose: WrapPurpose, secret: &[u8]) -> Result<Vec<u8>> {
    let body = aead::seal(kek, &purpose.aad(), secret)?;
    let mut out = Vec::with_capacity(1 + body.len());
    out.push(FORMAT_V1);
    out.extend_from_slice(&body);
    Ok(out)
}

pub fn open_bytes(
    kek: &SymmetricKey,
    purpose: WrapPurpose,
    wrapped: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    let (&version, body) = wrapped
        .split_first()
        .ok_or(CryptoError::Malformed("wrapped blob is empty"))?;
    if version != FORMAT_V1 {
        return Err(CryptoError::UnsupportedVersion(version));
    }
    aead::open(kek, &purpose.aad(), body)
}

pub fn wrap_key(kek: &SymmetricKey, purpose: WrapPurpose, key: &SymmetricKey) -> Result<Vec<u8>> {
    seal_bytes(kek, purpose, key.expose())
}

pub fn unwrap_key(
    kek: &SymmetricKey,
    purpose: WrapPurpose,
    wrapped: &[u8],
) -> Result<SymmetricKey> {
    SymmetricKey::from_slice(&open_bytes(kek, purpose, wrapped)?)
}
