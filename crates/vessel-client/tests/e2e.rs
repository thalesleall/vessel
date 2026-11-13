//! End-to-end tests: a real agent (in-memory SQLite) and several clients talking HTTP.

use std::time::Duration;

use vessel_client::model::{Entity, EntityKind, Host, Snippet};
use vessel_client::{ClientError, Role, ServerEvent, Vessel, events};

const PW: &str = "correct horse battery staple";

async fn agent() -> String {
    let addr = vessel_agent::spawn_for_tests(vessel_agent::Config::for_tests())
        .await
        .unwrap();
    format!("http://{addr}")
}

async fn signup(url: &str, email: &str) -> (Vessel, String) {
    let mut v = Vessel::in_memory().unwrap();
    let recovery = v.signup(url, email, PW, "pc-1", None).await.unwrap();
    (v, recovery)
}

async fn login(url: &str, email: &str, device: &str) -> Vessel {
    let mut v = Vessel::in_memory().unwrap();
    v.login(url, email, PW, device).await.unwrap();
    v
}

fn host(label: &str, address: &str) -> Entity {
    Entity::Host(Host {
        label: label.into(),
        address: address.into(),
        ..Default::default()
    })
}

fn hosts(v: &Vessel) -> Vec<(String, String)> {
    v.items(Some(EntityKind::Host))
        .unwrap()
        .into_iter()
        .map(|i| match i.entity {
            Entity::Host(h) => (h.label, h.address),
            _ => unreachable!(),
        })
        .collect()
}

#[tokio::test]
async fn same_environment_on_three_pcs() {
    let url = agent().await;
    let (mut pc1, _) = signup(&url, "Ana@Example.com").await;
    let mut pc2 = login(&url, "ana@example.com", "pc-2").await;
    let mut pc3 = login(&url, "ana@example.com", "pc-3").await;

    let personal = pc1.personal_vault().unwrap();
    let id = pc1
        .save(personal, None, host("prod-db", "10.0.0.5"))
        .unwrap();
    pc1.save(
        personal,
        None,
        Entity::Snippet(Snippet {
            label: "uptime".into(),
            script: "uptime".into(),
            ..Default::default()
        }),
    )
    .unwrap();
    assert_eq!(pc1.sync().await.unwrap().pushed, 2);

    pc2.sync().await.unwrap();
    pc3.sync().await.unwrap();
    assert_eq!(
        hosts(&pc2),
        vec![("prod-db".to_string(), "10.0.0.5".to_string())]
    );
    assert_eq!(pc3.items(None).unwrap().len(), 2);

    pc3.delete(personal, id).unwrap();
    pc3.sync().await.unwrap();
    pc1.sync().await.unwrap();
    assert!(hosts(&pc1).is_empty());
    assert_eq!(pc2.devices().await.unwrap().len(), 3);
}

#[tokio::test]
async fn concurrent_edits_merge_by_field() {
    let url = agent().await;
    let (mut a, _) = signup(&url, "a@example.com").await;
    let mut b = login(&url, "a@example.com", "b").await;
    let vault = a.personal_vault().unwrap();
    let id = a
        .save(
            vault,
            None,
            Entity::Host(Host {
                label: "db".into(),
                address: "10.0.0.1".into(),
                ..Default::default()
            }),
        )
        .unwrap();
    a.sync().await.unwrap();
    b.sync().await.unwrap();

    // Disjoint fields: both survive.
    a.save(
        vault,
        Some(id),
        Entity::Host(Host {
            label: "db-prod".into(),
            address: "10.0.0.1".into(),
            ..Default::default()
        }),
    )
    .unwrap();
    b.save(
        vault,
        Some(id),
        Entity::Host(Host {
            label: "db".into(),
            address: "10.0.0.1".into(),
            port: Some(2222),
            ..Default::default()
        }),
    )
    .unwrap();
    a.sync().await.unwrap();
    let report = b.sync().await.unwrap();
    assert_eq!(report.conflicts, 0);
    a.sync().await.unwrap();
    for v in [&a, &b] {
        let Entity::Host(h) = v.item(vault, id).unwrap().unwrap().entity else {
            panic!()
        };
        assert_eq!((h.label.as_str(), h.port), ("db-prod", Some(2222)));
    }

    // Same field: last pusher wins, the other value is kept as a conflict.
    a.save(vault, Some(id), host("db-prod", "10.0.0.2"))
        .unwrap();
    b.save(vault, Some(id), host("db-prod", "10.0.0.3"))
        .unwrap();
    a.sync().await.unwrap();
    assert_eq!(b.sync().await.unwrap().conflicts, 1);
    let conflicts = b.conflicts().unwrap();
    assert_eq!(conflicts[0].conflicts[0].field, "address");
    assert_eq!(conflicts[0].conflicts[0].remote, "10.0.0.2");
    a.sync().await.unwrap();
    assert_eq!(hosts(&a)[0].1, "10.0.0.3");
}

#[tokio::test]
async fn lock_unlock_offline_and_wrong_password() {
    let url = agent().await;
    let (mut v, _) = signup(&url, "lock@example.com").await;
    let vault = v.personal_vault().unwrap();
    v.save(vault, None, host("h", "1.2.3.4")).unwrap();
    v.lock();
    assert!(matches!(v.items(None), Err(ClientError::Locked)));
    assert!(matches!(v.unlock("nope"), Err(ClientError::WrongPassword)));
    v.unlock(PW).unwrap();
    assert_eq!(hosts(&v).len(), 1);
    // The refresh token still works after unlocking (session restored from sealed secrets).
    assert_eq!(v.sync().await.unwrap().pushed, 1);
}

#[tokio::test]
async fn team_sharing_and_member_removal_rotates_the_key() {
    let url = agent().await;
    let (mut alice, _) = signup(&url, "alice@example.com").await;
    let (mut bob, _) = signup(&url, "bob@example.com").await;

    let team = alice.create_vault("Infra").await.unwrap();
    let secret_host = alice
        .save(team, None, host("bastion", "203.0.113.10"))
        .unwrap();
    alice.sync().await.unwrap();

    let invite = alice
        .invite(team, Role::Editor, Some("bob@example.com".into()))
        .await
        .unwrap();
    assert_eq!(bob.accept_invite(&invite.url).await.unwrap(), Some(team));
    bob.sync().await.unwrap();
    assert!(
        bob.items(None).unwrap().iter().all(|i| i.vault_id != team),
        "no access before approval"
    );

    let members = alice.members(team).await.unwrap();
    let pending = &members.pending[0];
    assert_eq!(pending.email, "bob@example.com");
    assert_eq!(
        pending.fingerprint,
        bob.fingerprint().unwrap(),
        "fingerprints match out of band"
    );
    alice.approve_member(team, pending.user_id).await.unwrap();

    bob.sync().await.unwrap();
    assert!(
        bob.vaults()
            .unwrap()
            .iter()
            .any(|v| v.id == team && v.name == "Infra" && v.role == Role::Editor)
    );
    assert!(hosts(&bob).contains(&("bastion".into(), "203.0.113.10".into())));
    bob.save(team, None, host("web", "203.0.113.20")).unwrap();
    bob.sync().await.unwrap();
    alice.sync().await.unwrap();
    assert_eq!(alice.items(Some(EntityKind::Host)).unwrap().len(), 2);

    // Viewer cannot write.
    alice
        .change_role(team, members.pending[0].user_id, Role::Viewer)
        .await
        .unwrap();
    bob.sync().await.unwrap();
    assert!(bob.save(team, None, host("x", "y")).is_err());

    // Removal: Bob loses the vault; Alice's records move to key version 2.
    alice
        .remove_member(team, members.pending[0].user_id)
        .await
        .unwrap();
    alice
        .save(team, Some(secret_host), host("bastion", "198.51.100.1"))
        .unwrap();
    alice.sync().await.unwrap();
    bob.sync().await.unwrap();
    assert!(bob.vaults().unwrap().iter().all(|v| v.id != team));
    assert!(!bob.items(None).unwrap().iter().any(|i| i.vault_id == team));
    let view = alice
        .vaults()
        .unwrap()
        .into_iter()
        .find(|v| v.id == team)
        .unwrap();
    assert!(!view.rotating);
    assert_eq!(view.member_count, 1);
    assert_eq!(hosts(&alice).len(), 2);
}

#[tokio::test]
async fn recovery_key_resets_password_and_signs_out_other_devices() {
    let url = agent().await;
    let (mut old_device, recovery) = signup(&url, "forgot@example.com").await;
    let vault = old_device.personal_vault().unwrap();
    old_device
        .save(vault, None, host("keep-me", "10.1.1.1"))
        .unwrap();
    old_device.sync().await.unwrap();

    let mut fresh = Vessel::in_memory().unwrap();
    assert!(
        fresh
            .recover(&url, "forgot@example.com", "0000-1111", "x", "new")
            .await
            .is_err()
    );
    fresh
        .recover(
            &url,
            "forgot@example.com",
            &recovery,
            "brand new password",
            "new",
        )
        .await
        .unwrap();
    assert_eq!(hosts(&fresh), vec![("keep-me".into(), "10.1.1.1".into())]);

    let err = old_device.sync().await.unwrap_err();
    assert!(err.is_unauthorized(), "{err:?}");
    let mut again = Vessel::in_memory().unwrap();
    assert!(matches!(
        again.login(&url, "forgot@example.com", PW, "x").await,
        Err(ClientError::WrongPassword)
    ));
    again
        .login(&url, "forgot@example.com", "brand new password", "x")
        .await
        .unwrap();
}

#[tokio::test]
async fn password_change_keeps_data() {
    let url = agent().await;
    let (mut v, _) = signup(&url, "change@example.com").await;
    let vault = v.personal_vault().unwrap();
    v.save(vault, None, host("h", "1.1.1.1")).unwrap();
    v.sync().await.unwrap();
    assert!(matches!(
        v.change_password("wrong", "new password 123").await,
        Err(ClientError::WrongPassword)
    ));
    v.change_password(PW, "new password 123").await.unwrap();
    v.lock();
    v.unlock("new password 123").unwrap();
    let mut other = Vessel::in_memory().unwrap();
    other
        .login(&url, "change@example.com", "new password 123", "other")
        .await
        .unwrap();
    assert_eq!(hosts(&other).len(), 1);
}

#[tokio::test]
async fn websocket_notifies_other_devices() {
    let url = agent().await;
    let (mut a, _) = signup(&url, "ws@example.com").await;
    let mut b = login(&url, "ws@example.com", "b").await;
    let (server, token) = b.event_credentials().await.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        let _ = events::listen(&server, &token, move |e| {
            let _ = tx.send(e);
        })
        .await;
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let vault = a.personal_vault().unwrap();
    a.save(vault, None, host("h", "1.1.1.1")).unwrap();
    a.sync().await.unwrap();
    let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(event, ServerEvent::VaultChanged { vault_id, .. } if vault_id == vault));
}

#[tokio::test]
async fn duplicate_email_and_invite_only_mode() {
    let url = agent().await;
    signup(&url, "dup@example.com").await;
    let mut again = Vessel::in_memory().unwrap();
    assert!(
        again
            .signup(&url, "dup@example.com", PW, "x", None)
            .await
            .is_err()
    );

    let mut config = vessel_agent::Config::for_tests();
    config.signups = vessel_client::SignupMode::InviteOnly;
    let addr = vessel_agent::spawn_for_tests(config).await.unwrap();
    let url = format!("http://{addr}");
    let (mut admin, _) = signup(&url, "admin@example.com").await; // first user is always allowed
    let mut stranger = Vessel::in_memory().unwrap();
    assert!(
        stranger
            .signup(&url, "s@example.com", PW, "x", None)
            .await
            .is_err()
    );
    let invite = admin.create_signup_invite(None).await.unwrap();
    stranger
        .signup(&url, "s@example.com", PW, "x", Some(invite.token.clone()))
        .await
        .unwrap();
    let mut reuse = Vessel::in_memory().unwrap();
    assert!(
        reuse
            .signup(&url, "t@example.com", PW, "x", Some(invite.token))
            .await
            .is_err()
    );
}
