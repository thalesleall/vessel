//! Account key material: creation at sign-up and the three ways to unlock it
//! (master KEK after login, recovery key, device-local password unlock).

use core::fmt;
use core::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::identity::{UserKeys, UserPublicKeys};
use crate::kdf::{KdfParams, SALT_LEN, generate_salt, local_unlock_key};
use crate::secret::{KEY_LEN, SymmetricKey};
use crate::wrap::{WrapPurpose, open_bytes, seal_bytes, unwrap_key, wrap_key};
use crate::{CryptoError, Result};

/// Everything the agent stores about a user's keys. None of it is usable without the master
/// password or the recovery key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountKeyMaterial {
    pub public_keys: UserPublicKeys,
    pub wrapped_account_key_master: Vec<u8>,
    pub wrapped_account_key_recovery: Vec<u8>,
    pub encrypted_private_keys: Vec<u8>,
}

/// An unlocked account: the account key and the user's private keys, in memory only.
#[derive(Debug)]
pub struct UnlockedAccount {
    pub account_key: SymmetricKey,
    pub keys: UserKeys,
}

/// Creates the key material for a new account. Show the recovery key to the user once; it is
/// the only way back in if the master password is lost.
pub fn create_account(
    master_kek: &SymmetricKey,
) -> Result<(AccountKeyMaterial, UnlockedAccount, RecoveryKey)> {
    let account_key = SymmetricKey::generate()?;
    let recovery = RecoveryKey(SymmetricKey::generate()?);
    let keys = UserKeys::generate()?;

    let material = AccountKeyMaterial {
        public_keys: keys.public(),
        wrapped_account_key_master: wrap_key(
            master_kek,
            WrapPurpose::AccountKeyByMaster,
            &account_key,
        )?,
        wrapped_account_key_recovery: wrap_key(
            &recovery.0,
            WrapPurpose::AccountKeyByRecovery,
            &account_key,
        )?,
        encrypted_private_keys: seal_bytes(
            &account_key,
            WrapPurpose::UserPrivateKeys,
            &*keys.to_secret_bytes(),
        )?,
    };
    Ok((material, UnlockedAccount { account_key, keys }, recovery))
}

/// Unlocks with the master KEK obtained from an OPAQUE login.
pub fn unlock_with_master(
    material: &AccountKeyMaterial,
    master_kek: &SymmetricKey,
) -> Result<UnlockedAccount> {
    let account_key = unwrap_key(
        master_kek,
        WrapPurpose::AccountKeyByMaster,
        &material.wrapped_account_key_master,
    )?;
    open_private_keys(material, account_key)
}

/// Unlocks with the recovery key, e.g. after the master password was forgotten.
pub fn unlock_with_recovery(
    material: &AccountKeyMaterial,
    recovery: &RecoveryKey,
) -> Result<UnlockedAccount> {
    let account_key = unwrap_key(
        &recovery.0,
        WrapPurpose::AccountKeyByRecovery,
        &material.wrapped_account_key_recovery,
    )?;
    open_private_keys(material, account_key)
}

/// Re-wraps the account key after a master password change. No data is re-encrypted.
pub fn rewrap_for_master(
    account: &UnlockedAccount,
    new_master_kek: &SymmetricKey,
) -> Result<Vec<u8>> {
    wrap_key(
        new_master_kek,
        WrapPurpose::AccountKeyByMaster,
        &account.account_key,
    )
}

/// Issues a new recovery key for an unlocked account. The previous recovery key stops working
/// once the returned wrapping replaces `wrapped_account_key_recovery`. No data is re-encrypted.
pub fn new_recovery_key(account: &UnlockedAccount) -> Result<(Vec<u8>, RecoveryKey)> {
    let recovery = RecoveryKey(SymmetricKey::generate()?);
    let wrapped = wrap_key(
        &recovery.0,
        WrapPurpose::AccountKeyByRecovery,
        &account.account_key,
    )?;
    Ok((wrapped, recovery))
}

fn open_private_keys(
    material: &AccountKeyMaterial,
    account_key: SymmetricKey,
) -> Result<UnlockedAccount> {
    let secret = open_bytes(
        &account_key,
        WrapPurpose::UserPrivateKeys,
        &material.encrypted_private_keys,
    )?;
    let keys = UserKeys::from_secret_bytes(&secret)?;
    // The public keys come from the server; make sure they really belong to these private keys.
    if keys.public() != material.public_keys {
        return Err(CryptoError::InvalidKey);
    }
    Ok(UnlockedAccount { account_key, keys })
}

// ---------------------------------------------------------------------------------------------
// Device-local unlock (offline)
// ---------------------------------------------------------------------------------------------

/// Stored on the device only. Lets the app unlock without the network.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalUnlock {
    pub salt: [u8; SALT_LEN],
    pub params: KdfParams,
    pub wrapped_account_key: Vec<u8>,
}

pub fn create_local_unlock(
    account: &UnlockedAccount,
    password: &[u8],
    params: KdfParams,
) -> Result<LocalUnlock> {
    let salt = generate_salt()?;
    let local_key = local_unlock_key(password, &salt, &params)?;
    Ok(LocalUnlock {
        salt,
        params,
        wrapped_account_key: wrap_key(
            &local_key,
            WrapPurpose::AccountKeyByLocal,
            &account.account_key,
        )?,
    })
}

/// Unlocks from the local copy. A wrong password fails with [`CryptoError::Decrypt`].
pub fn unlock_local(
    local: &LocalUnlock,
    password: &[u8],
    material: &AccountKeyMaterial,
) -> Result<UnlockedAccount> {
    let local_key = local_unlock_key(password, &local.salt, &local.params)?;
    let account_key = unwrap_key(
        &local_key,
        WrapPurpose::AccountKeyByLocal,
        &local.wrapped_account_key,
    )?;
    open_private_keys(material, account_key)
}

// ---------------------------------------------------------------------------------------------
// Recovery key
// ---------------------------------------------------------------------------------------------

/// 256-bit recovery key, shown to the user as 16 groups of 4 hex digits.
#[derive(PartialEq, Eq)]
pub struct RecoveryKey(SymmetricKey);

impl fmt::Display for RecoveryKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, pair) in self.0.expose().as_chunks::<2>().0.iter().enumerate() {
            if i > 0 {
                f.write_str("-")?;
            }
            write!(f, "{:02X}{:02X}", pair[0], pair[1])?;
        }
        Ok(())
    }
}

impl fmt::Debug for RecoveryKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RecoveryKey(<redacted>)")
    }
}

impl RecoveryKey {
    /// Secret the client presents to the server to prove it holds the recovery key, e.g. to
    /// reset a forgotten master password. Derived one-way, so it reveals nothing about the key.
    pub fn auth_token(&self) -> Result<[u8; KEY_LEN]> {
        let mut out = [0u8; KEY_LEN];
        hkdf::Hkdf::<sha2::Sha256>::new(None, self.0.expose())
            .expand(b"vessel/v1/recovery-auth", &mut out)
            .map_err(|_| CryptoError::Kdf)?;
        Ok(out)
    }
}

/// What the server stores to verify a recovery [`RecoveryKey::auth_token`]: `SHA-256(token)`.
pub fn recovery_auth_hash(token: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(token).into()
}

impl FromStr for RecoveryKey {
    type Err = CryptoError;

    /// Accepts the displayed form; dashes, spaces and letter case are ignored.
    fn from_str(s: &str) -> Result<Self> {
        let digits: Vec<u8> = s.bytes().filter(|b| !matches!(b, b'-' | b' ')).collect();
        if digits.len() != KEY_LEN * 2 {
            return Err(CryptoError::Malformed("recovery key length"));
        }
        let mut key = zeroize::Zeroizing::new([0u8; KEY_LEN]);
        for (i, pair) in digits.as_chunks::<2>().0.iter().enumerate() {
            key[i] = (hex_value(pair[0])? << 4) | hex_value(pair[1])?;
        }
        Ok(Self(SymmetricKey::from_bytes(*key)))
    }
}

fn hex_value(c: u8) -> Result<u8> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(CryptoError::Malformed("recovery key digit")),
    }
}
