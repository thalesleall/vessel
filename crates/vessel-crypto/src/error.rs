/// Errors produced by this crate.
///
/// Decryption failures are deliberately not more specific: a wrong key, a tampered ciphertext
/// and a ciphertext moved to another context all surface as [`CryptoError::Decrypt`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CryptoError {
    #[error("the operating system random number generator failed")]
    Rng,
    #[error("encryption failed")]
    Encrypt,
    #[error("decryption failed")]
    Decrypt,
    #[error("unsupported format version {0}")]
    UnsupportedVersion(u8),
    #[error("unsupported algorithm {0}")]
    UnsupportedAlgorithm(u8),
    #[error("malformed input: {0}")]
    Malformed(&'static str),
    #[error("invalid key")]
    InvalidKey,
    #[error("invalid signature")]
    InvalidSignature,
    #[error("key derivation failed")]
    Kdf,
    #[error("key derivation parameters are too weak")]
    WeakKdfParams,
    #[error("OPAQUE protocol failure")]
    Opaque,
    #[error("grant does not match: {0}")]
    GrantMismatch(&'static str),
}

pub type Result<T> = core::result::Result<T, CryptoError>;
