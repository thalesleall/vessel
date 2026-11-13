//! End-to-end key hierarchy: OPAQUE sign-up and login against the server half, then every way
//! of unlocking the account. Uses the minimum Argon2id cost to keep the suite fast.

use vessel_crypto::account::{
    RecoveryKey, create_account, create_local_unlock, rewrap_for_master, unlock_local,
    unlock_with_master, unlock_with_recovery,
};
use vessel_crypto::kdf::KdfParams;
use vessel_crypto::opaque::{
    OpaqueIdentity, ServerSetup, client_login_start, client_registration_start, server_login_start,
    server_registration_finish, server_registration_start,
};
use vessel_crypto::{CryptoError, SymmetricKey};

const PARAMS: KdfParams = KdfParams::MINIMUM;
const IDENTITY: OpaqueIdentity<'static> = OpaqueIdentity {
    user: b"ana@example.com",
    server: b"vessel.example.com",
};
const CREDENTIAL_ID: &[u8] = b"user-0001";

/// Runs OPAQUE registration and returns (password file, master KEK).
fn register(setup: &ServerSetup, password: &[u8]) -> (Vec<u8>, SymmetricKey) {
    let (state, request) = client_registration_start(password).unwrap();
    let response = server_registration_start(setup, &request, CREDENTIAL_ID).unwrap();
    let (upload, master_kek) = state
        .finish(password, &response, &IDENTITY, &PARAMS)
        .unwrap();
    (server_registration_finish(&upload).unwrap(), master_kek)
}

/// Runs OPAQUE login; returns the master KEK if both sides accept.
fn login(
    setup: &ServerSetup,
    file: Option<&[u8]>,
    password: &[u8],
) -> Result<SymmetricKey, CryptoError> {
    let (state, request) = client_login_start(password)?;
    let (server_state, response) =
        server_login_start(setup, file, &request, CREDENTIAL_ID, &IDENTITY)?;
    let out = state.finish(password, &response, &IDENTITY, &PARAMS)?;
    let server_session = server_state.finish(&out.finalization, &IDENTITY)?;
    assert_eq!(*out.session_key, *server_session);
    Ok(out.master_kek)
}

#[test]
fn sign_up_then_log_in_on_another_device() {
    let setup = ServerSetup::generate();
    let (file, kek_at_signup) = register(&setup, b"correct horse battery staple");
    let (material, account, _recovery) = create_account(&kek_at_signup).unwrap();

    // New device: only the password, the password file and the stored material.
    let kek = login(&setup, Some(&file), b"correct horse battery staple").unwrap();
    assert_eq!(kek, kek_at_signup);
    let unlocked = unlock_with_master(&material, &kek).unwrap();
    assert_eq!(unlocked.account_key, account.account_key);
    assert_eq!(unlocked.keys.public(), account.keys.public());
}

#[test]
fn wrong_password_fails_login() {
    let setup = ServerSetup::generate();
    let (file, _) = register(&setup, b"right password");
    assert_eq!(
        login(&setup, Some(&file), b"wrong password").unwrap_err(),
        CryptoError::Opaque
    );
}

#[test]
fn unknown_user_fails_like_a_wrong_password() {
    let setup = ServerSetup::generate();
    assert_eq!(
        login(&setup, None, b"anything").unwrap_err(),
        CryptoError::Opaque
    );
}

#[test]
fn password_file_is_bound_to_its_server() {
    let setup = ServerSetup::generate();
    let (file, _) = register(&setup, b"pw");
    let other_server = ServerSetup::generate();
    assert!(login(&other_server, Some(&file), b"pw").is_err());
}

#[test]
fn server_setup_survives_restart() {
    let setup = ServerSetup::generate();
    let (file, kek) = register(&setup, b"pw");
    let restored = ServerSetup::from_bytes(&setup.to_bytes()).unwrap();
    assert_eq!(login(&restored, Some(&file), b"pw").unwrap(), kek);
}

#[test]
fn weak_kdf_params_are_refused() {
    let weak = KdfParams {
        memory_kib: 1024,
        iterations: 1,
        parallelism: 1,
    };
    let (state, request) = client_registration_start(b"pw").unwrap();
    let setup = ServerSetup::generate();
    let response = server_registration_start(&setup, &request, CREDENTIAL_ID).unwrap();
    assert_eq!(
        state
            .finish(b"pw", &response, &IDENTITY, &weak)
            .unwrap_err(),
        CryptoError::WeakKdfParams
    );
}

#[test]
fn recovery_key_unlocks_and_round_trips_as_text() {
    let (material, account, recovery) = create_account(&SymmetricKey::generate().unwrap()).unwrap();

    let typed: RecoveryKey = recovery
        .to_string()
        .to_lowercase()
        .replace('-', " ")
        .parse()
        .unwrap();
    assert_eq!(typed, recovery);
    let unlocked = unlock_with_recovery(&material, &typed).unwrap();
    assert_eq!(unlocked.account_key, account.account_key);

    assert!("not-a-key".parse::<RecoveryKey>().is_err());
    assert!(format!("{recovery:?}").contains("redacted"));
}

#[test]
fn master_password_change_keeps_data_keys() {
    let old_kek = SymmetricKey::generate().unwrap();
    let (mut material, account, _) = create_account(&old_kek).unwrap();

    let new_kek = SymmetricKey::generate().unwrap();
    material.wrapped_account_key_master = rewrap_for_master(&account, &new_kek).unwrap();

    assert_eq!(
        unlock_with_master(&material, &new_kek).unwrap().account_key,
        account.account_key
    );
    assert_eq!(
        unlock_with_master(&material, &old_kek).unwrap_err(),
        CryptoError::Decrypt
    );
}

#[test]
fn offline_unlock_with_local_password() {
    let (material, account, _) = create_account(&SymmetricKey::generate().unwrap()).unwrap();
    let local = create_local_unlock(&account, b"pw", PARAMS).unwrap();

    assert_eq!(
        unlock_local(&local, b"pw", &material).unwrap().account_key,
        account.account_key
    );
    assert_eq!(
        unlock_local(&local, b"nope", &material).unwrap_err(),
        CryptoError::Decrypt
    );
}

#[test]
fn substituted_public_keys_are_detected() {
    let kek = SymmetricKey::generate().unwrap();
    let (mut material, _, _) = create_account(&kek).unwrap();
    let (other, _, _) = create_account(&kek).unwrap();

    // The server serves someone else's public keys next to this user's encrypted private keys.
    material.public_keys = other.public_keys;
    assert_eq!(
        unlock_with_master(&material, &kek).unwrap_err(),
        CryptoError::InvalidKey
    );
}

#[test]
fn account_kdf_params_cannot_be_weaker_than_the_client_default() {
    use vessel_crypto::kdf::KdfParams;
    KdfParams::default().validate_account().unwrap();
    // Above OWASP's floor, but cheaper than what this client registers with.
    assert!(KdfParams::MINIMUM.validate_account().is_err());
    let mut p = KdfParams::default();
    p.iterations -= 1;
    assert!(p.validate_account().is_err());
}
