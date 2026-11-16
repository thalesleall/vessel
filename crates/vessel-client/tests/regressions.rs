//! Regression tests: sign-up invites, key rotation after a member leaves, and trust decisions.

use vessel_client::model::{Entity, Host, Snippet};
use vessel_client::{Role, SignupMode, Vessel};

const PW: &str = "correct horse battery staple";

async fn agent(signups: SignupMode) -> String {
    let mut config = vessel_agent::Config::for_tests();
    config.signups = signups;
    let addr = vessel_agent::spawn_for_tests(config).await.unwrap();
    format!("http://{addr}")
}

async fn signup(url: &str, email: &str, invite: Option<String>) -> Vessel {
    let mut v = Vessel::in_memory().unwrap();
    v.signup(url, email, PW, "pc", invite).await.unwrap();
    v
}

/// A vault invite admits one sign-up only, and none once its vault is gone.
#[tokio::test]
async fn a_vault_invite_admits_one_signup_only() {
    let url = agent(SignupMode::InviteOnly).await;
    let mut admin = signup(&url, "admin@example.com", None).await;
    let vault = admin.create_vault("Time").await.unwrap();
    let invite = admin.invite(vault, Role::Viewer, None).await.unwrap();

    // The invitee signs up with it and can still join the vault with the same token.
    let mut bob = signup(&url, "bob@example.com", Some(invite.token.clone())).await;
    bob.accept_invite(&invite.url).await.unwrap();

    // Nobody else can sign up with that token.
    let mut eve = Vessel::in_memory().unwrap();
    assert!(
        eve.signup(
            &url,
            "eve@example.com",
            PW,
            "pc",
            Some(invite.token.clone())
        )
        .await
        .is_err(),
        "a vault invite admitted a second sign-up"
    );

    // An unused invite of a deleted vault admits nobody.
    let other = admin.create_vault("Outro").await.unwrap();
    let orphan = admin.invite(other, Role::Viewer, None).await.unwrap();
    admin.delete_vault(other).await.unwrap();
    assert!(
        eve.signup(&url, "eve@example.com", PW, "pc", Some(orphan.token))
            .await
            .is_err(),
        "the invite of a deleted vault admitted a sign-up"
    );
}

/// A member who leaves must not keep a usable key: the next manager sync rotates it.
#[tokio::test]
async fn a_member_leaving_makes_the_next_manager_sync_rotate() {
    let url = agent(SignupMode::Open).await;
    let mut owner = signup(&url, "owner@example.com", None).await;
    let mut bob = signup(&url, "bob@example.com", None).await;
    let vault = owner.create_vault("Time").await.unwrap();
    let invite = owner.invite(vault, Role::Editor, None).await.unwrap();
    bob.accept_invite(&invite.url).await.unwrap();
    owner
        .approve_member(vault, bob.status().user_id.unwrap())
        .await
        .unwrap();
    bob.sync().await.unwrap();
    let version = |v: &Vessel| {
        v.vaults()
            .unwrap()
            .into_iter()
            .find(|x| x.id == vault)
            .unwrap()
            .key_version
    };
    owner.sync().await.unwrap();
    let before = version(&owner);

    // Leaving needs no manager action, so the key must be rotated on the next manager sync.
    bob.leave_vault(vault).await.expect("leave_vault");
    owner.sync().await.unwrap();
    assert_eq!(
        version(&owner),
        before + 1,
        "no fresh key after a member left"
    );
    owner.sync().await.unwrap();
    assert_eq!(
        version(&owner),
        before + 1,
        "rotated again without a removal"
    );
}

/// An id present in two vaults is never resolved to whichever copy comes first.
#[tokio::test]
async fn an_id_reused_in_another_vault_is_refused() {
    let url = agent(SignupMode::Open).await;
    let mut alice = signup(&url, "alice@example.com", None).await;
    let mut mallory = signup(&url, "mallory@example.com", None).await;

    let personal = alice.personal_vault().unwrap();
    let host_id = alice
        .save(
            personal,
            None,
            Entity::Host(Host {
                label: "prod".into(),
                address: "10.0.0.1".into(),
                ..Default::default()
            }),
        )
        .unwrap();
    alice.sync().await.unwrap();

    // Mallory shares a vault with Alice and writes a record with the same id into it.
    let shared = mallory.create_vault("Time").await.unwrap();
    let invite = mallory.invite(shared, Role::Viewer, None).await.unwrap();
    alice.accept_invite(&invite.url).await.unwrap();
    mallory
        .approve_member(shared, alice.status().user_id.unwrap())
        .await
        .unwrap();
    mallory
        .save(
            shared,
            Some(host_id),
            Entity::Snippet(Snippet {
                label: "!".into(),
                script: "echo shadowed".into(),
                ..Default::default()
            }),
        )
        .unwrap();
    mallory.sync().await.unwrap();
    alice.sync().await.unwrap();

    assert!(
        alice.find(host_id).is_err(),
        "an id present in two vaults resolved silently"
    );
    // Lookups scoped to a vault still work.
    assert!(alice.item(personal, host_id).unwrap().is_some());
}
