//! Key derivation: the master KEK from the OPAQUE export key, and the device-local unlock key
//! from the master password.

use argon2::{Algorithm, Argon2, Params, Version};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::secret::{KEY_LEN, SymmetricKey, random_bytes};
use crate::{CryptoError, Result};

pub const SALT_LEN: usize = 16;

const MASTER_KEK_INFO: &[u8] = b"vessel/v1/master-kek";

/// Derives the master KEK from the OPAQUE `export_key`. Only the client ever knows either value.
pub fn master_kek(export_key: &[u8]) -> Result<SymmetricKey> {
    let mut okm = [0u8; KEY_LEN];
    Hkdf::<Sha256>::new(None, export_key)
        .expand(MASTER_KEK_INFO, &mut okm)
        .map_err(|_| CryptoError::Kdf)?;
    Ok(SymmetricKey::from_bytes(okm))
}

/// Argon2id cost parameters. Stored per account so they can be raised over time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
}

impl KdfParams {
    /// The floor accepted anywhere: OWASP's minimum recommendation for Argon2id.
    pub const MINIMUM: Self = Self {
        memory_kib: 19 * 1024,
        iterations: 2,
        parallelism: 1,
    };

    /// Rejects parameters below [`KdfParams::MINIMUM`], so a malicious server cannot hand a
    /// client cheap parameters and make the password easy to brute-force.
    pub fn validate(&self) -> Result<()> {
        let min = Self::MINIMUM;
        if self.memory_kib < min.memory_kib
            || self.iterations < min.iterations
            || self.parallelism < 1
        {
            return Err(CryptoError::WeakKdfParams);
        }
        // Parameters can come from the server: an absurd cost would hang or exhaust the client.
        if self.memory_kib > 2 * 1024 * 1024 || self.iterations > 32 || self.parallelism > 16 {
            return Err(CryptoError::WeakKdfParams);
        }
        Ok(())
    }

    pub(crate) fn argon2_params(&self) -> Result<Params> {
        self.validate()?;
        Params::new(
            self.memory_kib,
            self.iterations,
            self.parallelism,
            Some(KEY_LEN),
        )
        .map_err(|_| CryptoError::WeakKdfParams)
    }
}

impl Default for KdfParams {
    fn default() -> Self {
        Self {
            memory_kib: 64 * 1024,
            iterations: 3,
            parallelism: 1,
        }
    }
}

pub fn generate_salt() -> Result<[u8; SALT_LEN]> {
    random_bytes()
}

/// Derives the device-local unlock key: `Argon2id(password, salt)`. Lets the app unlock offline,
/// since the master KEK needs an OPAQUE round-trip with the server.
pub fn local_unlock_key(
    password: &[u8],
    salt: &[u8; SALT_LEN],
    params: &KdfParams,
) -> Result<SymmetricKey> {
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params.argon2_params()?);
    let mut out = [0u8; KEY_LEN];
    argon2
        .hash_password_into(password, salt, &mut out)
        .map_err(|_| CryptoError::Kdf)?;
    Ok(SymmetricKey::from_bytes(out))
}
