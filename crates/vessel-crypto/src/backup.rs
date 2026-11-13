//! Encrypted server backups.
//!
//! The agent's database holds the OPAQUE server secret next to every password file, so a leaked
//! plain backup allows offline guessing of every user's password. Backups are therefore
//! encrypted with a key derived from a passphrase the operator keeps elsewhere.
//!
//! ```text
//! file  = magic(8) || version(1) || salt(16) || memory_kib(u32) || iterations(u32) || parallelism(u32)
//!         || chunk*
//! chunk = length(u32 BE) || XChaCha20-Poly1305(nonce || ciphertext || tag)
//! aad   = "vessel/backup/v1" || header || index(u64 BE) || last(1)
//! ```
//!
//! Chunks are authenticated with their position and with a final-chunk flag, so a backup cannot
//! be reordered, spliced with another one, or silently truncated.

use std::io::{Read, Write};

use crate::kdf::{KdfParams, SALT_LEN, generate_salt, local_unlock_key};
use crate::secret::SymmetricKey;
use crate::{CryptoError, Result, aead};

const MAGIC: &[u8; 8] = b"VSLBAK\0\0";
const VERSION: u8 = 1;
const AAD_DOMAIN: &[u8] = b"vessel/backup/v1";
/// Plaintext bytes per chunk.
pub const CHUNK: usize = 1024 * 1024;
const HEADER_LEN: usize = 8 + 1 + SALT_LEN + 12;

fn header(salt: &[u8; SALT_LEN], params: &KdfParams) -> Vec<u8> {
    let mut h = Vec::with_capacity(HEADER_LEN);
    h.extend_from_slice(MAGIC);
    h.push(VERSION);
    h.extend_from_slice(salt);
    h.extend_from_slice(&params.memory_kib.to_be_bytes());
    h.extend_from_slice(&params.iterations.to_be_bytes());
    h.extend_from_slice(&params.parallelism.to_be_bytes());
    h
}

fn aad(header: &[u8], index: u64, last: bool) -> Vec<u8> {
    let mut a = Vec::with_capacity(AAD_DOMAIN.len() + header.len() + 9);
    a.extend_from_slice(AAD_DOMAIN);
    a.extend_from_slice(header);
    a.extend_from_slice(&index.to_be_bytes());
    a.push(u8::from(last));
    a
}

fn io(_: std::io::Error) -> CryptoError {
    CryptoError::Malformed("backup i/o")
}

/// Reads until `buf` is full or the input ends; returns the bytes read.
fn fill(input: &mut impl Read, buf: &mut [u8]) -> Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match input.read(&mut buf[n..]).map_err(io)? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

fn key_for(passphrase: &[u8], salt: &[u8; SALT_LEN], params: &KdfParams) -> Result<SymmetricKey> {
    if passphrase.len() < 16 {
        return Err(CryptoError::Malformed("the backup passphrase needs at least 16 characters"));
    }
    local_unlock_key(passphrase, salt, params)
}

/// Encrypts `input` into `output`.
pub fn encrypt(passphrase: &[u8], input: &mut impl Read, output: &mut impl Write) -> Result<()> {
    let salt = generate_salt()?;
    let params = KdfParams::default();
    let key = key_for(passphrase, &salt, &params)?;
    let header = header(&salt, &params);
    output.write_all(&header).map_err(io)?;

    let mut current = vec![0u8; CHUNK];
    let mut next = vec![0u8; CHUNK];
    let mut len = fill(input, &mut current)?;
    let mut index = 0u64;
    loop {
        // Look ahead one chunk to know whether this one is the last.
        let next_len = if len == CHUNK { fill(input, &mut next)? } else { 0 };
        let last = next_len == 0;
        let sealed = aead::seal(&key, &aad(&header, index, last), &current[..len])?;
        let n = u32::try_from(sealed.len()).map_err(|_| CryptoError::Malformed("chunk"))?;
        output.write_all(&n.to_be_bytes()).map_err(io)?;
        output.write_all(&sealed).map_err(io)?;
        if last {
            break;
        }
        std::mem::swap(&mut current, &mut next);
        len = next_len;
        index += 1;
    }
    output.flush().map_err(io)
}

/// Decrypts a backup made by [`encrypt`]. Fails on a wrong passphrase, tampering, reordering
/// or truncation; output written before a failure must be discarded.
pub fn decrypt(passphrase: &[u8], input: &mut impl Read, output: &mut impl Write) -> Result<()> {
    let mut header = [0u8; HEADER_LEN];
    if fill(input, &mut header)? != HEADER_LEN || &header[..8] != MAGIC {
        return Err(CryptoError::Malformed("not a Vessel backup"));
    }
    if header[8] != VERSION {
        return Err(CryptoError::UnsupportedVersion(header[8]));
    }
    let salt: [u8; SALT_LEN] = header[9..9 + SALT_LEN].try_into().expect("slice length");
    let num = |at: usize| u32::from_be_bytes(header[at..at + 4].try_into().expect("slice length"));
    let params = KdfParams {
        memory_kib: num(9 + SALT_LEN),
        iterations: num(13 + SALT_LEN),
        parallelism: num(17 + SALT_LEN),
    };
    let key = key_for(passphrase, &salt, &params)?;

    let max = CHUNK + aead::NONCE_LEN + aead::TAG_LEN;
    let mut index = 0u64;
    loop {
        let mut len = [0u8; 4];
        if fill(input, &mut len)? != 4 {
            return Err(CryptoError::Malformed("backup is truncated"));
        }
        let n = u32::from_be_bytes(len) as usize;
        if n > max {
            return Err(CryptoError::Malformed("backup chunk too large"));
        }
        let mut sealed = vec![0u8; n];
        if fill(input, &mut sealed)? != n {
            return Err(CryptoError::Malformed("backup is truncated"));
        }
        // Try as an inner chunk first, then as the last one.
        let (plain, last) = match aead::open(&key, &aad(&header, index, false), &sealed) {
            Ok(p) => (p, false),
            Err(_) => (aead::open(&key, &aad(&header, index, true), &sealed)?, true),
        };
        output.write_all(&plain).map_err(io)?;
        if last {
            let mut extra = [0u8; 1];
            if fill(input, &mut extra)? != 0 {
                return Err(CryptoError::Malformed("data after the end of the backup"));
            }
            return output.flush().map_err(io);
        }
        index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASS: &[u8] = b"a long backup passphrase";

    fn round_trip(data: &[u8]) -> Vec<u8> {
        let mut enc = Vec::new();
        encrypt(PASS, &mut &data[..], &mut enc).unwrap();
        let mut dec = Vec::new();
        decrypt(PASS, &mut &enc[..], &mut dec).unwrap();
        dec
    }

    #[test]
    fn round_trips_across_chunk_boundaries() {
        for len in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, 2 * CHUNK + 7] {
            let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            assert_eq!(round_trip(&data), data, "length {len}");
        }
    }

    #[test]
    fn refuses_wrong_passphrase_tampering_and_truncation() {
        let data = vec![7u8; 2 * CHUNK + 5];
        let mut enc = Vec::new();
        encrypt(PASS, &mut &data[..], &mut enc).unwrap();
        let mut sink = Vec::new();
        assert!(decrypt(b"another passphrase here", &mut &enc[..], &mut sink).is_err());

        let mut tampered = enc.clone();
        let mid = tampered.len() / 2;
        tampered[mid] ^= 1;
        assert!(decrypt(PASS, &mut &tampered[..], &mut Vec::new()).is_err());

        // Dropping the last chunk: the one before it is not marked last.
        let first_two = HEADER_LEN + 2 * (4 + CHUNK + aead::NONCE_LEN + aead::TAG_LEN);
        assert!(decrypt(PASS, &mut &enc[..first_two], &mut Vec::new()).is_err());
    }

    #[test]
    fn short_passphrases_are_refused() {
        assert!(encrypt(b"short", &mut &b"x"[..], &mut Vec::new()).is_err());
    }
}
