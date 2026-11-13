//! Client-side cryptography for Vessel.
//!
//! Key hierarchy (see `docs/ARCHITECTURE.md` §5):
//!
//! ```text
//! password ──OPAQUE──► export_key ──HKDF──► master KEK ─┐
//!                                       recovery key ───┼─wraps─► account key ─encrypts─► user key pairs
//!                       Argon2id(password) local key ───┘
//! vault key ──HPKE grant per member──► record envelopes
//! ```
//!
//! The sync agent only ever sees the outputs of this crate: envelopes, wrapped keys, grants,
//! public keys and the server half of OPAQUE (behind the `server` feature).

pub mod account;
mod aead;
pub mod envelope;
pub mod error;
pub mod grant;
pub mod identity;
pub mod kdf;
pub mod membership;
pub mod opaque;
pub mod secret;
pub mod wrap;

pub use error::{CryptoError, Result};
pub use secret::SymmetricKey;

/// Length of the opaque 128-bit identifiers (UUIDs) used for users, vaults and records.
pub const ID_LEN: usize = 16;

/// A 128-bit identifier, as raw bytes.
pub type Id = [u8; ID_LEN];
