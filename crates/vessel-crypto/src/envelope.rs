//! Record envelopes: the only form in which user data leaves a device.
//!
//! ```text
//! envelope = version(1) || algorithm(1) || nonce(24) || ciphertext || tag(16)
//! aad v1   = "vessel/v1/record" || version || algorithm || vault_id || record_id || key_version(u32 BE)
//! aad v2   = aad v1 || record_version(u64 BE) || deleted(1)
//! ```
//!
//! Version 2 also binds the record's version number and deletion flag, so the server can
//! neither serve an older copy of a record as a newer one nor turn a record into a deletion.
//! Version 1 envelopes are still read (and rewritten as version 2 by the apps).
//!
//! The header is authenticated, so a downgrade to another version or algorithm fails to decrypt.
//! The context binds an envelope to one record of one vault under one key version: the server
//! cannot swap envelopes between records, between vaults, or back to an older key version.
//! Plaintext is padded to a multiple of [`PAD_BLOCK`] bytes to blur record sizes.

use zeroize::Zeroizing;

use crate::secret::SymmetricKey;
use crate::{CryptoError, Id, Result, aead};

pub const FORMAT_V1: u8 = 1;
pub const FORMAT_V2: u8 = 2;
pub const ALG_XCHACHA20_POLY1305: u8 = 1;
pub const PAD_BLOCK: usize = 256;

const HEADER_LEN: usize = 2;
const AAD_DOMAIN: &[u8] = b"vessel/v1/record";

/// Where an envelope belongs. Both sides know this without decrypting anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordContext {
    pub vault_id: Id,
    pub record_id: Id,
    pub key_version: u32,
    /// The record's version and deletion flag (format 2). `None` seals format 1, for data that
    /// has no record version (vault metadata).
    pub revision: Option<Revision>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Revision {
    pub version: u64,
    pub deleted: bool,
}

impl RecordContext {
    fn aad(&self, header: &[u8; HEADER_LEN]) -> Result<Vec<u8>> {
        let mut aad = Vec::with_capacity(AAD_DOMAIN.len() + HEADER_LEN + 16 + 16 + 4 + 9);
        aad.extend_from_slice(AAD_DOMAIN);
        aad.extend_from_slice(header);
        aad.extend_from_slice(&self.vault_id);
        aad.extend_from_slice(&self.record_id);
        aad.extend_from_slice(&self.key_version.to_be_bytes());
        if header[0] == FORMAT_V2 {
            let r = self
                .revision
                .ok_or(CryptoError::Malformed("format 2 needs the record revision"))?;
            aad.extend_from_slice(&r.version.to_be_bytes());
            aad.push(u8::from(r.deleted));
        }
        Ok(aad)
    }
}

/// Encrypts a serialized record with a vault key.
pub fn seal_record(
    vault_key: &SymmetricKey,
    ctx: &RecordContext,
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    let format = if ctx.revision.is_some() { FORMAT_V2 } else { FORMAT_V1 };
    let header = [format, ALG_XCHACHA20_POLY1305];
    let padded = pad(plaintext);
    let body = aead::seal(vault_key, &ctx.aad(&header)?, &padded)?;

    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
    out.extend_from_slice(&header);
    out.extend_from_slice(&body);
    Ok(out)
}

/// Decrypts an envelope produced by [`seal_record`] for the same context.
pub fn open_record(
    vault_key: &SymmetricKey,
    ctx: &RecordContext,
    envelope: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    let (header, body) = envelope
        .split_first_chunk::<HEADER_LEN>()
        .ok_or(CryptoError::Malformed("envelope too short"))?;
    if header[0] != FORMAT_V1 && header[0] != FORMAT_V2 {
        return Err(CryptoError::UnsupportedVersion(header[0]));
    }
    if header[1] != ALG_XCHACHA20_POLY1305 {
        return Err(CryptoError::UnsupportedAlgorithm(header[1]));
    }
    let padded = aead::open(vault_key, &ctx.aad(header)?, body)?;
    unpad(&padded)
}

/// Reads the envelope format version without decrypting, e.g. to route old formats to a migrator.
pub fn format_version(envelope: &[u8]) -> Option<u8> {
    envelope.first().copied()
}

/// ISO/IEC 7816-4 padding: append 0x80, then zeros up to the next multiple of `PAD_BLOCK`.
fn pad(plaintext: &[u8]) -> Zeroizing<Vec<u8>> {
    let padded_len = (plaintext.len() / PAD_BLOCK + 1) * PAD_BLOCK;
    let mut out = Zeroizing::new(Vec::with_capacity(padded_len));
    out.extend_from_slice(plaintext);
    out.push(0x80);
    out.resize(padded_len, 0);
    out
}

fn unpad(padded: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    let marker = padded
        .iter()
        .rposition(|&b| b != 0)
        .ok_or(CryptoError::Malformed("padding"))?;
    if padded[marker] != 0x80 || !padded.len().is_multiple_of(PAD_BLOCK) {
        return Err(CryptoError::Malformed("padding"));
    }
    Ok(Zeroizing::new(padded[..marker].to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_round_trips_at_block_boundaries() {
        for len in [0, 1, PAD_BLOCK - 1, PAD_BLOCK, PAD_BLOCK + 1, 3 * PAD_BLOCK] {
            let data = vec![0u8; len];
            let padded = pad(&data);
            assert_eq!(padded.len() % PAD_BLOCK, 0);
            assert!(padded.len() > len);
            assert_eq!(*unpad(&padded).unwrap(), data);
        }
    }

    #[test]
    fn unpad_rejects_missing_marker() {
        assert!(unpad(&[0u8; PAD_BLOCK]).is_err());
        let mut bad = vec![0u8; PAD_BLOCK];
        bad[10] = 0x01;
        assert!(unpad(&bad).is_err());
    }
}
