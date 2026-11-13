use core::fmt;

use subtle::ConstantTimeEq;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::{CryptoError, Result};

/// Length in bytes of every symmetric key in the hierarchy.
pub const KEY_LEN: usize = 32;

/// A 256-bit symmetric key. Zeroed on drop, redacted in `Debug`, compared in constant time.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct SymmetricKey([u8; KEY_LEN]);

impl SymmetricKey {
    /// Generates a fresh key from the operating system CSPRNG.
    pub fn generate() -> Result<Self> {
        let mut key = [0u8; KEY_LEN];
        fill_random(&mut key)?;
        Ok(Self(key))
    }

    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        let key: [u8; KEY_LEN] = bytes.try_into().map_err(|_| CryptoError::InvalidKey)?;
        Ok(Self(key))
    }

    /// Raw key bytes. Keep the borrow short and never log or persist the result unencrypted.
    pub fn expose(&self) -> &[u8; KEY_LEN] {
        &self.0
    }
}

impl PartialEq for SymmetricKey {
    fn eq(&self, other: &Self) -> bool {
        self.0.ct_eq(&other.0).into()
    }
}

impl Eq for SymmetricKey {}

impl fmt::Debug for SymmetricKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SymmetricKey(<redacted>)")
    }
}

/// Fills `buf` from the operating system CSPRNG.
pub(crate) fn fill_random(buf: &mut [u8]) -> Result<()> {
    getrandom::fill(buf).map_err(|_| CryptoError::Rng)
}

/// Returns `N` random bytes from the operating system CSPRNG.
pub fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut out = [0u8; N];
    fill_random(&mut out)?;
    Ok(out)
}
