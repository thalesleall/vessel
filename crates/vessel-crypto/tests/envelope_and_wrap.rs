use vessel_crypto::envelope::{PAD_BLOCK, RecordContext, open_record, seal_record};
use vessel_crypto::wrap::{WrapPurpose, open_bytes, seal_bytes, unwrap_key, wrap_key};
use vessel_crypto::{CryptoError, SymmetricKey};

fn ctx() -> RecordContext {
    RecordContext {
        vault_id: [1; 16],
        record_id: [2; 16],
        key_version: 1,
    }
}

#[test]
fn record_round_trip() {
    let vk = SymmetricKey::generate().unwrap();
    let plaintext = br#"{"type":"host","address":"10.0.0.5","port":22}"#;
    let envelope = seal_record(&vk, &ctx(), plaintext).unwrap();
    assert_eq!(*open_record(&vk, &ctx(), &envelope).unwrap(), plaintext);
}

#[test]
fn envelope_hides_exact_length() {
    let vk = SymmetricKey::generate().unwrap();
    let short = seal_record(&vk, &ctx(), b"a").unwrap();
    let longer = seal_record(&vk, &ctx(), &[b'a'; 200]).unwrap();
    assert_eq!(short.len(), longer.len());
    // header(2) + nonce(24) + one padded block + tag(16)
    assert_eq!(short.len(), 2 + 24 + PAD_BLOCK + 16);
}

#[test]
fn same_plaintext_gives_different_envelopes() {
    let vk = SymmetricKey::generate().unwrap();
    assert_ne!(
        seal_record(&vk, &ctx(), b"x").unwrap(),
        seal_record(&vk, &ctx(), b"x").unwrap()
    );
}

#[test]
fn record_rejects_wrong_key() {
    let envelope = seal_record(&SymmetricKey::generate().unwrap(), &ctx(), b"secret").unwrap();
    let other = SymmetricKey::generate().unwrap();
    assert_eq!(
        open_record(&other, &ctx(), &envelope).unwrap_err(),
        CryptoError::Decrypt
    );
}

#[test]
fn record_is_bound_to_its_context() {
    let vk = SymmetricKey::generate().unwrap();
    let envelope = seal_record(&vk, &ctx(), b"secret").unwrap();

    let moved_record = RecordContext {
        record_id: [3; 16],
        ..ctx()
    };
    let moved_vault = RecordContext {
        vault_id: [9; 16],
        ..ctx()
    };
    let old_key_version = RecordContext {
        key_version: 0,
        ..ctx()
    };
    for wrong in [moved_record, moved_vault, old_key_version] {
        assert_eq!(
            open_record(&vk, &wrong, &envelope).unwrap_err(),
            CryptoError::Decrypt
        );
    }
}

#[test]
fn record_detects_tampering_anywhere() {
    let vk = SymmetricKey::generate().unwrap();
    let envelope = seal_record(&vk, &ctx(), b"secret").unwrap();
    for i in [2, 10, 30, envelope.len() - 1] {
        let mut tampered = envelope.clone();
        tampered[i] ^= 0x01;
        assert!(
            open_record(&vk, &ctx(), &tampered).is_err(),
            "byte {i} not authenticated"
        );
    }
}

#[test]
fn record_rejects_unknown_header() {
    let vk = SymmetricKey::generate().unwrap();
    let mut envelope = seal_record(&vk, &ctx(), b"secret").unwrap();
    envelope[0] = 2;
    assert_eq!(
        open_record(&vk, &ctx(), &envelope).unwrap_err(),
        CryptoError::UnsupportedVersion(2)
    );
    envelope[0] = 1;
    envelope[1] = 7;
    assert_eq!(
        open_record(&vk, &ctx(), &envelope).unwrap_err(),
        CryptoError::UnsupportedAlgorithm(7)
    );
    assert!(open_record(&vk, &ctx(), &[1]).is_err());
}

#[test]
fn key_wrap_round_trip() {
    let kek = SymmetricKey::generate().unwrap();
    let key = SymmetricKey::generate().unwrap();
    let wrapped = wrap_key(&kek, WrapPurpose::AccountKeyByMaster, &key).unwrap();
    assert_eq!(
        unwrap_key(&kek, WrapPurpose::AccountKeyByMaster, &wrapped).unwrap(),
        key
    );
}

#[test]
fn key_wrap_is_bound_to_its_purpose() {
    let kek = SymmetricKey::generate().unwrap();
    let wrapped = seal_bytes(&kek, WrapPurpose::AccountKeyByRecovery, &[7; 32]).unwrap();
    assert_eq!(
        open_bytes(&kek, WrapPurpose::AccountKeyByMaster, &wrapped).unwrap_err(),
        CryptoError::Decrypt
    );
}

#[test]
fn debug_output_redacts_keys() {
    let key = SymmetricKey::from_bytes([0xAB; 32]);
    let shown = format!("{key:?}");
    assert!(!shown.to_lowercase().contains("ab"), "{shown}");
}
