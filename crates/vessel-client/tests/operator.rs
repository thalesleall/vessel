//! Account deletion, server administration, quotas, and the on-disk database with readers.

use vessel_client::model::{Entity, Host};
use vessel_client::{AdminUserUpdate, ClientError, ErrorCode, Role, Vessel};

const PW: &str = "correct horse battery staple";

async fn agent_with(f: impl FnOnce(&mut vessel_agent::Config)) -> String {
    let mut config = vessel_agent::Config::for_tests();
    f(&mut config);
    let addr = vessel_agent::spawn_for_tests(config).await.unwrap();
    format!("http://{addr}")
}

async fn signup(url: &str, email: &str) -> Vessel {
    let mut v = Vessel::in_memory().unwrap();
    v.signup(url, email, PW, "pc", None).await.unwrap();
    v
}

fn host(label: &str) -> Entity {
    Entity::Host(Host {
        label: label.into(),
        address: "10.0.0.1".into(),
        ..Default::default()
    })
}

async fn share(owner: &mut Vessel, member: &mut Vessel, name: &str, role: Role) -> uuid::Uuid {
    let vault = owner.create_vault(name).await.unwrap();
    let invite = owner.invite(vault, role, None).await.unwrap();
    member.accept_invite(&invite.url).await.unwrap();
    owner
        .approve_member(vault, member.status().user_id.unwrap())
        .await
        .unwrap();
    member.sync().await.unwrap();
    vault
}

#[tokio::test]
async fn deleting_an_account_removes_its_vaults_and_memberships() {
    let url = agent_with(|_| {}).await;
    let _admin = signup(&url, "admin@example.com").await;
    let mut alice = signup(&url, "alice@example.com").await;
    let mut bob = signup(&url, "bob@example.com").await;
    let alices = share(&mut alice, &mut bob, "Da Alice", Role::Editor).await;
    let bobs = share(&mut bob, &mut alice, "Do Bob", Role::Editor).await;

    assert!(matches!(
        alice.delete_account("wrong password").await,
        Err(ClientError::WrongPassword)
    ));
    alice.delete_account(PW).await.unwrap();
    assert!(!alice.status().signed_in);

    // Bob loses the vault Alice owned; Alice left Bob's with a signed event.
    bob.sync().await.unwrap();
    let vaults = bob.vaults().unwrap();
    assert!(!vaults.iter().any(|v| v.id == alices));
    assert!(vaults.iter().any(|v| v.id == bobs));
    let members = bob.members(bobs).await.unwrap().members;
    assert_eq!(members.len(), 1, "alice is still listed: {members:?}");

    // The account cannot sign in, and the e-mail is free again.
    let mut again = Vessel::in_memory().unwrap();
    assert!(again.login(&url, "alice@example.com", PW, "pc").await.is_err());
    signup(&url, "alice@example.com").await;
}

#[tokio::test]
async fn the_operator_suspends_sets_quotas_and_keeps_an_admin() {
    let url = agent_with(|_| {}).await;
    let mut admin = signup(&url, "admin@example.com").await;
    let mut bob = signup(&url, "bob@example.com").await;
    let bob_id = bob.status().user_id.unwrap();
    let admin_id = admin.status().user_id.unwrap();

    // Not an admin: no access to the operator routes.
    assert!(bob.admin_users().await.is_err());

    let users = admin.admin_users().await.unwrap();
    assert_eq!(users.len(), 2);

    // Suspension signs Bob out and blocks his login until lifted.
    admin
        .admin_update_user(bob_id, &AdminUserUpdate { suspended: Some(true), ..Default::default() })
        .await
        .unwrap();
    assert!(bob.sync().await.unwrap_err().is_unauthorized());
    let mut bob2 = Vessel::in_memory().unwrap();
    match bob2.login(&url, "bob@example.com", PW, "pc-2").await {
        Err(ClientError::Api { code, .. }) => assert_eq!(code, ErrorCode::Suspended),
        other => panic!("suspended login: {other:?}"),
    }
    admin
        .admin_update_user(bob_id, &AdminUserUpdate { suspended: Some(false), ..Default::default() })
        .await
        .unwrap();
    bob2.login(&url, "bob@example.com", PW, "pc-2").await.unwrap();

    // A tiny quota: the edit is refused but kept on the device.
    admin
        .admin_update_user(bob_id, &AdminUserUpdate { quota_bytes: Some(Some(1)), ..Default::default() })
        .await
        .unwrap();
    let personal = bob2.personal_vault().unwrap();
    bob2.save(personal, None, host("big")).unwrap();
    match bob2.sync().await {
        Err(ClientError::Api { code, .. }) => assert_eq!(code, ErrorCode::QuotaExceeded),
        other => panic!("over quota: {other:?}"),
    }
    admin
        .admin_update_user(bob_id, &AdminUserUpdate { quota_bytes: Some(None), ..Default::default() })
        .await
        .unwrap();
    bob2.sync().await.unwrap();
    assert!(admin.admin_stats().await.unwrap().bytes > 0);

    // The server keeps at least one admin, and the operator cannot delete themself here.
    assert!(
        admin
            .admin_update_user(admin_id, &AdminUserUpdate { is_admin: Some(false), ..Default::default() })
            .await
            .is_err()
    );
    assert!(admin.admin_delete_user(admin_id).await.is_err());
    admin.admin_delete_user(bob_id).await.unwrap();
    assert_eq!(admin.admin_users().await.unwrap().len(), 1);
}

/// The agent on a file (as deployed) reads through separate connections.
#[tokio::test]
async fn an_on_disk_agent_syncs_between_devices() {
    let dir = std::env::temp_dir().join(format!("vessel-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("vessel.db");
    let db = path.clone();
    let url = agent_with(move |c| c.database = db).await;
    let mut pc1 = signup(&url, "disk@example.com").await;
    let vault = pc1.personal_vault().unwrap();
    pc1.save(vault, None, host("on-disk")).unwrap();
    pc1.sync().await.unwrap();
    let mut pc2 = Vessel::in_memory().unwrap();
    pc2.login(&url, "disk@example.com", PW, "pc-2").await.unwrap();
    pc2.sync().await.unwrap();
    assert!(pc2.items(None).unwrap().iter().any(|i| matches!(&i.entity, Entity::Host(h) if h.label == "on-disk")));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Edits not yet sent to a vault that disappears are kept in the personal vault.
#[tokio::test]
async fn unsent_edits_of_a_deleted_vault_are_kept() {
    let url = agent_with(|_| {}).await;
    let mut owner = signup(&url, "owner@example.com").await;
    let mut bob = signup(&url, "bob@example.com").await;
    let vault = share(&mut owner, &mut bob, "Time", Role::Editor).await;
    bob.save(vault, None, host("unsent")).unwrap();
    owner.delete_vault(vault).await.unwrap();

    let report = bob.sync().await.unwrap();
    assert_eq!(report.rescued, 1);
    let personal = bob.personal_vault().unwrap();
    assert!(bob.items(None).unwrap().iter().any(
        |i| i.vault_id == personal && matches!(&i.entity, Entity::Host(h) if h.label == "unsent")
    ));
}

/// What one device verified reaches the account's other devices through the personal vault.
#[tokio::test]
async fn verified_keys_and_histories_reach_new_devices() {
    let url = agent_with(|_| {}).await;
    let mut owner = signup(&url, "owner@example.com").await;
    let mut bob = signup(&url, "bob@example.com").await;
    let vault = share(&mut owner, &mut bob, "Time", Role::Editor).await;
    owner.sync().await.unwrap();

    let trust = |v: &Vessel| {
        v.items(Some(vessel_client::model::EntityKind::Trust))
            .unwrap()
            .into_iter()
            .find_map(|i| match i.entity {
                Entity::Trust(t) => Some(t),
                _ => None,
            })
    };
    let t = trust(&owner).expect("no trust record after sync");
    assert!(t.users.contains_key(&bob.status().user_id.unwrap()));
    assert!(t.anchors.get(&vault).is_some_and(|a| a.seq >= 2));

    // A new device of the owner gets it and syncs the shared vault against it.
    let mut laptop = Vessel::in_memory().unwrap();
    laptop.login(&url, "owner@example.com", PW, "laptop").await.unwrap();
    laptop.sync().await.unwrap();
    assert!(trust(&laptop).is_some());
    assert!(laptop.vaults().unwrap().iter().any(|v| v.id == vault));
}

/// When every account is deleted, the next one is a new first admin (and needs the code).
#[tokio::test]
async fn a_server_emptied_by_deletions_gets_a_new_first_admin() {
    let code = "installation-code-0123456789";
    let url = agent_with(|c| c.bootstrap_token = Some(code.into())).await;
    let mut first = Vessel::in_memory().unwrap();
    first.signup(&url, "first@example.com", PW, "pc", Some(code.into())).await.unwrap();
    first.delete_account(PW).await.unwrap();
    assert!(vessel_client::server_info(&url).await.unwrap().first_user);
    let mut next = Vessel::in_memory().unwrap();
    assert!(next.signup(&url, "next@example.com", PW, "pc", None).await.is_err());
    next.signup(&url, "next@example.com", PW, "pc", Some(code.into())).await.unwrap();
    assert!(next.status().is_server_admin || next.admin_users().await.is_ok());
}
