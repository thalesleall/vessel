//! Regression tests for the app <-> agent handshake against a hosted (possibly hostile) server.

use vessel_client::{Role, SignupMode, Vessel};

const PW: &str = "correct horse battery staple";

async fn agent_with(f: impl FnOnce(&mut vessel_agent::Config)) -> String {
    let mut config = vessel_agent::Config::for_tests();
    f(&mut config);
    let addr = vessel_agent::spawn_for_tests(config).await.unwrap();
    format!("http://{addr}")
}

async fn signup(url: &str, email: &str, invite: Option<String>) -> Vessel {
    let mut v = Vessel::in_memory().unwrap();
    v.signup(url, email, PW, "pc", invite).await.unwrap();
    v
}

/// A user could sign out any other user's device: the sessions were deleted before checking
/// that the device was the caller's.
#[tokio::test]
async fn revoking_someone_elses_device_changes_nothing() {
    let url = agent_with(|_| {}).await;
    let mut alice = signup(&url, "alice@example.com", None).await;
    let mut bob = signup(&url, "bob@example.com", None).await;
    let bob_device = bob.devices().await.unwrap()[0].id;

    assert!(alice.revoke_device(bob_device).await.is_err());
    // Bob's session still works (it would 401 if its session had been deleted).
    assert_eq!(bob.devices().await.unwrap().len(), 1);
    bob.sync().await.unwrap();
}

/// There is no user directory to enumerate a hosted server's accounts.
#[tokio::test]
async fn there_is_no_user_directory() {
    let url = agent_with(|_| {}).await;
    let _alice = signup(&url, "alice@example.com", None).await;
    let http = reqwest::Client::new();
    for path in ["/v1/users/lookup?email=alice@example.com", "/v1/users/00000000-0000-0000-0000-000000000000"] {
        let status = http.get(format!("{url}{path}")).send().await.unwrap().status();
        assert_eq!(status, reqwest::StatusCode::NOT_FOUND, "{path}");
    }
}

/// Whoever reached a fresh server first became its admin.
#[tokio::test]
async fn the_first_account_needs_the_installation_code() {
    let code = "installation-code-0123456789";
    let url = agent_with(|c| c.bootstrap_token = Some(code.into())).await;
    let mut eve = Vessel::in_memory().unwrap();
    assert!(eve.signup(&url, "eve@example.com", PW, "pc", None).await.is_err());
    assert!(
        eve.signup(&url, "eve@example.com", PW, "pc", Some("wrong-code-0123456789".into()))
            .await
            .is_err()
    );
    let mut admin = Vessel::in_memory().unwrap();
    admin
        .signup(&url, "admin@example.com", PW, "pc", Some(code.into()))
        .await
        .unwrap();
}

/// On a hosted service, customers' vault invites must not admit new accounts.
#[tokio::test]
async fn vault_invites_can_stop_admitting_accounts() {
    let url = agent_with(|c| {
        c.signups = SignupMode::InviteOnly;
        c.vault_invite_signups = false;
    })
    .await;
    let mut admin = signup(&url, "admin@example.com", None).await;
    let vault = admin.create_vault("Time").await.unwrap();
    let invite = admin.invite(vault, Role::Viewer, None).await.unwrap();
    let mut bob = Vessel::in_memory().unwrap();
    assert!(
        bob.signup(&url, "bob@example.com", PW, "pc", Some(invite.token))
            .await
            .is_err()
    );
}

/// Changing the password proves the current one to the server (not only to this device).
#[tokio::test]
async fn password_change_still_works_with_the_proof() {
    let url = agent_with(|_| {}).await;
    let mut v = signup(&url, "alice@example.com", None).await;
    v.change_password(PW, "a brand new password 42").await.unwrap();
    let mut other = Vessel::in_memory().unwrap();
    other
        .login(&url, "alice@example.com", "a brand new password 42", "laptop")
        .await
        .unwrap();
}

/// A demoted manager signed the current key's grants, and clients accept new key versions only
/// from current managers: demotion rotates the vault key on the next manager sync.
#[tokio::test]
async fn demoting_a_manager_rotates_the_vault_key() {
    let url = agent_with(|_| {}).await;
    let mut owner = signup(&url, "owner@example.com", None).await;
    let mut bob = signup(&url, "bob@example.com", None).await;
    let vault = owner.create_vault("Time").await.unwrap();
    let invite = owner.invite(vault, Role::Admin, None).await.unwrap();
    bob.accept_invite(&invite.url).await.unwrap();
    let bob_id = bob.status().user_id.unwrap();
    owner.approve_member(vault, bob_id).await.unwrap();
    bob.sync().await.unwrap();
    owner.sync().await.unwrap();
    let version = |v: &Vessel| {
        v.vaults()
            .unwrap()
            .into_iter()
            .find(|x| x.id == vault)
            .unwrap()
            .key_version
    };
    let before = version(&owner);

    owner.change_role(vault, bob_id, Role::Editor).await.unwrap();
    owner.sync().await.unwrap();
    assert_eq!(version(&owner), before + 1, "no fresh key after demoting a manager");
    // Bob, still a member, gets the new key from the owner (a current manager).
    bob.sync().await.unwrap();
    assert_eq!(version(&bob), before + 1);
}

/// A recovery key that was used may have leaked: recovery issues a new one and the old one
/// stops working.
#[tokio::test]
async fn recovery_replaces_the_recovery_key() {
    let url = agent_with(|_| {}).await;
    let mut v = Vessel::in_memory().unwrap();
    let first = v
        .signup(&url, "alice@example.com", PW, "pc", None)
        .await
        .unwrap();
    let mut pc2 = Vessel::in_memory().unwrap();
    let second = pc2
        .recover(&url, "alice@example.com", &first, "new password one 1", "pc-2")
        .await
        .unwrap();
    assert_ne!(first, second);
    let mut pc3 = Vessel::in_memory().unwrap();
    assert!(
        pc3.recover(&url, "alice@example.com", &first, "new password two 2", "pc-3")
            .await
            .is_err(),
        "the used recovery key still works"
    );
    pc3.recover(&url, "alice@example.com", &second, "new password two 2", "pc-3")
        .await
        .unwrap();
}

/// The approval is tied to the fingerprint the person read out: the vault key goes only to keys
/// with that fingerprint, whatever the server lists as pending.
#[tokio::test]
async fn an_approval_needs_the_fingerprint_of_the_keys_that_get_the_vault_key() {
    let url = agent_with(|_| {}).await;
    let mut alice = signup(&url, "alice@example.com", None).await;
    let mut bob = signup(&url, "bob@example.com", None).await;
    let vault = alice.create_vault("Time").await.unwrap();
    let invite = alice.invite(vault, Role::Editor, None).await.unwrap();
    bob.accept_invite(&invite.url).await.unwrap();
    let bob_id = bob.status().user_id.unwrap();

    // Someone else's fingerprint (here Alice's own), or none, approves nobody.
    let wrong = alice.fingerprint().unwrap();
    assert!(alice.approve_member_verified(vault, bob_id, &wrong).await.is_err());
    assert!(alice.approve_member_verified(vault, bob_id, "").await.is_err());
    bob.sync().await.unwrap();
    assert!(bob.vaults().unwrap().iter().all(|v| v.id != vault));

    // The right one does, however it was typed.
    let spoken = bob.fingerprint().unwrap().replace(' ', "-");
    alice.approve_member_verified(vault, bob_id, &spoken).await.unwrap();
    bob.sync().await.unwrap();
    assert!(bob.vaults().unwrap().iter().any(|v| v.id == vault));
}
