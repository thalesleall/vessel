use vessel_crypto::grant::{Grant, GrantParties};
use vessel_crypto::identity::UserKeys;
use vessel_crypto::{CryptoError, Id, SymmetricKey};

const VAULT: Id = [0x10; 16];
const OWNER: Id = [0xA0; 16];
const ALICE: Id = [0xA1; 16];
const BOB: Id = [0xB0; 16];

fn issue(
    granter_id: Id,
    granter: &UserKeys,
    recipient_id: Id,
    recipient: &UserKeys,
    vk: &SymmetricKey,
) -> Grant {
    let parties = GrantParties {
        granter_id,
        granter,
        recipient_id,
        recipient: &recipient.public(),
    };
    Grant::issue(parties, VAULT, 1, vk).unwrap()
}

#[test]
fn member_recovers_vault_key() {
    let owner = UserKeys::generate().unwrap();
    let alice = UserKeys::generate().unwrap();
    let vk = SymmetricKey::generate().unwrap();

    let grant = issue(OWNER, &owner, ALICE, &alice, &vk);
    assert_eq!(grant.open(&ALICE, &alice, &owner.public()).unwrap(), vk);
}

#[test]
fn grant_survives_serialization() {
    let owner = UserKeys::generate().unwrap();
    let alice = UserKeys::generate().unwrap();
    let vk = SymmetricKey::generate().unwrap();
    let grant = issue(OWNER, &owner, ALICE, &alice, &vk);

    let json = serde_json::to_string(&grant).unwrap();
    let restored: Grant = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, grant);
    assert_eq!(restored.open(&ALICE, &alice, &owner.public()).unwrap(), vk);
}

#[test]
fn other_user_cannot_open_grant() {
    let owner = UserKeys::generate().unwrap();
    let alice = UserKeys::generate().unwrap();
    let bob = UserKeys::generate().unwrap();
    let grant = issue(
        OWNER,
        &owner,
        ALICE,
        &alice,
        &SymmetricKey::generate().unwrap(),
    );

    // Bob claiming to be Alice: id matches, keys do not.
    assert_eq!(
        grant.open(&ALICE, &bob, &owner.public()).unwrap_err(),
        CryptoError::GrantMismatch("recipient keys")
    );
    assert_eq!(
        grant.open(&BOB, &bob, &owner.public()).unwrap_err(),
        CryptoError::GrantMismatch("recipient id")
    );
}

#[test]
fn grant_signed_by_someone_else_is_rejected() {
    let owner = UserKeys::generate().unwrap();
    let impostor = UserKeys::generate().unwrap();
    let alice = UserKeys::generate().unwrap();

    // A malicious server forges a grant with its own key and claims the owner issued it.
    let forged = issue(
        OWNER,
        &impostor,
        ALICE,
        &alice,
        &SymmetricKey::generate().unwrap(),
    );
    assert_eq!(
        forged.verify(&owner.public()).unwrap_err(),
        CryptoError::InvalidSignature
    );
    assert_eq!(
        forged.open(&ALICE, &alice, &owner.public()).unwrap_err(),
        CryptoError::InvalidSignature
    );
}

#[test]
fn any_modified_field_breaks_the_grant() {
    let owner = UserKeys::generate().unwrap();
    let alice = UserKeys::generate().unwrap();
    let grant = issue(
        OWNER,
        &owner,
        ALICE,
        &alice,
        &SymmetricKey::generate().unwrap(),
    );

    let mut tampered = Vec::new();
    tampered.push(Grant {
        key_version: 2,
        ..grant.clone()
    });
    tampered.push(Grant {
        vault_id: [0x11; 16],
        ..grant.clone()
    });
    tampered.push(Grant {
        granter_id: [0xEE; 16],
        ..grant.clone()
    });
    let mut wrapped = grant.wrapped_vault_key.clone();
    wrapped[0] ^= 1;
    tampered.push(Grant {
        wrapped_vault_key: wrapped,
        ..grant.clone()
    });

    for g in tampered {
        assert!(g.open(&ALICE, &alice, &owner.public()).is_err(), "{g:?}");
    }
}

#[test]
fn member_can_regrant_to_another_member() {
    let owner = UserKeys::generate().unwrap();
    let alice = UserKeys::generate().unwrap();
    let bob = UserKeys::generate().unwrap();
    let vk = SymmetricKey::generate().unwrap();

    let to_alice = issue(OWNER, &owner, ALICE, &alice, &vk);
    let alice_vk = to_alice.open(&ALICE, &alice, &owner.public()).unwrap();
    let to_bob = issue(ALICE, &alice, BOB, &bob, &alice_vk);
    assert_eq!(to_bob.open(&BOB, &bob, &alice.public()).unwrap(), vk);
}

#[test]
fn identity_round_trips_through_secret_bytes() {
    let keys = UserKeys::generate().unwrap();
    let restored = UserKeys::from_secret_bytes(&*keys.to_secret_bytes()).unwrap();
    assert_eq!(restored.public(), keys.public());

    let sig = keys.sign(b"hello");
    restored.public().verify(b"hello", &sig).unwrap();
    assert_eq!(
        keys.public().verify(b"hellO", &sig).unwrap_err(),
        CryptoError::InvalidSignature
    );
}

#[test]
fn fingerprint_is_stable_and_readable() {
    let keys = UserKeys::generate().unwrap();
    let fp = keys.public().fingerprint();
    assert_eq!(fp, keys.public().fingerprint());
    let shown = fp.to_string();
    assert_eq!(shown.len(), 5 * 5 + 4, "{shown}");
    assert!(
        shown
            .split(' ')
            .all(|g| g.len() == 5 && g.bytes().all(|b| b.is_ascii_digit()))
    );
    assert_ne!(fp, UserKeys::generate().unwrap().public().fingerprint());
}
