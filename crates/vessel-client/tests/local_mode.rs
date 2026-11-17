//! Local-only mode and the migrations between it and a server, with a real agent.

use std::path::PathBuf;

use vessel_client::model::{
    Entity, EntityKind, Host, Shell, Snippet, SshKey, TabTarget, TerminalProfile, Workspace,
    WorkspaceTab,
};
use vessel_client::{ClientError, Role, Vessel};

const PW: &str = "correct horse battery staple";
const PW2: &str = "another long password";

async fn agent() -> String {
    let addr = vessel_agent::spawn_for_tests(vessel_agent::Config::for_tests())
        .await
        .unwrap();
    format!("http://{addr}")
}

fn temp_db(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vessel-test-{}", uuid_like()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn uuid_like() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

fn host(label: &str, address: &str) -> Entity {
    Entity::Host(Host {
        label: label.into(),
        address: address.into(),
        ..Default::default()
    })
}

fn labels(v: &Vessel, kind: EntityKind) -> Vec<String> {
    let mut l: Vec<String> = v
        .items(Some(kind))
        .unwrap()
        .into_iter()
        .map(|i| i.entity.label().to_string())
        .collect();
    l.sort();
    l
}

fn profile(label: &str) -> Entity {
    Entity::TerminalProfile(TerminalProfile {
        label: label.into(),
        shell: Shell::Pwsh,
        ..Default::default()
    })
}

#[tokio::test]
async fn local_mode_works_without_a_server() {
    let path = temp_db("local.db");
    let mut v = Vessel::open(&path).unwrap();
    let recovery = v.create_local("Ana", PW).unwrap();
    assert!(v.is_local() && v.status().local && v.status().unlocked);

    let vault = v.personal_vault().unwrap();
    v.save(vault, None, host("prod", "10.0.0.1")).unwrap();
    assert_eq!(v.sync().await.unwrap().pushed, 0, "nothing to sync with");
    assert!(matches!(
        v.create_vault("Time").await,
        Err(ClientError::LocalMode)
    ));
    assert!(matches!(v.devices().await, Err(ClientError::LocalMode)));

    // Survives a restart, locks and unlocks offline.
    drop(v);
    let mut v = Vessel::open(&path).unwrap();
    assert!(v.status().signed_in && v.status().local && !v.status().unlocked);
    assert!(matches!(v.unlock("wrong"), Err(ClientError::WrongPassword)));
    v.unlock(PW).unwrap();
    assert_eq!(labels(&v, EntityKind::Host), vec!["prod"]);

    // Password change and recovery-key reset, both offline.
    v.change_password(PW, PW2).await.unwrap();
    v.lock();
    assert!(v.unlock(PW).is_err());
    v.unlock(PW2).unwrap();
    v.lock();
    v.recover_local(&recovery, PW).unwrap();
    assert_eq!(labels(&v, EntityKind::Host), vec!["prod"]);
    v.lock();
    v.unlock(PW).unwrap();
}

#[tokio::test]
async fn local_to_new_account_moves_everything() {
    let url = agent().await;
    let path = temp_db("migrate.db");
    let mut v = Vessel::open(&path).unwrap();
    let old_recovery = v.create_local("", PW).unwrap();
    let vault = v.personal_vault().unwrap();
    let key = v
        .save(
            vault,
            None,
            Entity::SshKey(SshKey {
                label: "deploy".into(),
                private_key: "-----BEGIN OPENSSH PRIVATE KEY-----x".into(),
                ..Default::default()
            }),
        )
        .unwrap();
    v.save(
        vault,
        None,
        Entity::Host(Host {
            label: "api".into(),
            address: "10.0.0.2".into(),
            ssh_key_id: Some(key),
            ..Default::default()
        }),
    )
    .unwrap();
    v.save(
        vault,
        None,
        Entity::Snippet(Snippet {
            label: "disk".into(),
            script: "df -h".into(),
            ..Default::default()
        }),
    )
    .unwrap();

    v.backup(&path.with_file_name("backup.db")).unwrap();
    assert!(path.with_file_name("backup.db").exists());

    let report = v
        .connect_new_account(&url, "me@example.com", PW, "pc-1", None)
        .await
        .unwrap();
    assert_eq!(report.imported, 3);
    let new_recovery = report.recovery_key.clone().unwrap();
    assert_ne!(new_recovery, old_recovery);
    assert!(!v.is_local());
    assert_eq!(v.pending_count().unwrap(), 0, "everything was pushed");

    // A second computer sees the same items, with references intact.
    let mut pc2 = Vessel::in_memory().unwrap();
    pc2.login(&url, "me@example.com", PW, "pc-2").await.unwrap();
    assert_eq!(labels(&pc2, EntityKind::Host), vec!["api"]);
    assert_eq!(labels(&pc2, EntityKind::Snippet), vec!["disk"]);
    let api = pc2.items(Some(EntityKind::Host)).unwrap().remove(0);
    let Entity::Host(h) = api.entity else {
        unreachable!()
    };
    assert_eq!(h.ssh_key_id, Some(key));
    assert!(pc2.find(key).unwrap().is_some());

    // The new recovery key works on the server; the old one does not.
    let mut pc3 = Vessel::in_memory().unwrap();
    assert!(
        pc3.recover(&url, "me@example.com", &old_recovery, PW2, "pc-3")
            .await
            .is_err()
    );
    pc3.recover(&url, "me@example.com", &new_recovery, PW2, "pc-3")
        .await
        .unwrap();
    assert_eq!(labels(&pc3, EntityKind::Host), vec!["api"]);
}

#[tokio::test]
async fn local_to_existing_account_merges_without_duplicates() {
    let url = agent().await;
    let mut server = Vessel::in_memory().unwrap();
    server
        .signup(&url, "me@example.com", PW, "pc-1", None)
        .await
        .unwrap();
    let sv = server.personal_vault().unwrap();
    server
        .save(sv, None, host("already-there", "10.0.0.9"))
        .unwrap();
    let remote_profile = server.save(sv, None, profile("PowerShell")).unwrap();
    server.sync().await.unwrap();

    let mut v = Vessel::in_memory().unwrap();
    v.create_local("x", "local password!!").unwrap();
    let lv = v.personal_vault().unwrap();
    v.save(lv, None, host("only-local", "10.0.0.3")).unwrap();
    let local_profile = v.save(lv, None, profile("PowerShell")).unwrap();
    v.save(
        lv,
        None,
        Entity::Workspace(Workspace {
            label: "dia a dia".into(),
            tabs: vec![WorkspaceTab {
                target: TabTarget::Profile {
                    profile_id: local_profile,
                },
                title: String::new(),
            }],
            ..Default::default()
        }),
    )
    .unwrap();

    let report = v
        .connect_existing_account(&url, "me@example.com", PW, "pc-2")
        .await
        .unwrap();
    assert_eq!((report.imported, report.skipped_duplicates), (2, 1));
    assert!(report.recovery_key.is_none());

    server.sync().await.unwrap();
    assert_eq!(
        labels(&server, EntityKind::Host),
        vec!["already-there", "only-local"]
    );
    assert_eq!(
        labels(&server, EntityKind::TerminalProfile),
        vec!["PowerShell"]
    );
    let ws = server.items(Some(EntityKind::Workspace)).unwrap().remove(0);
    let Entity::Workspace(ws) = ws.entity else {
        unreachable!()
    };
    assert_eq!(
        ws.tabs[0].target,
        TabTarget::Profile {
            profile_id: remote_profile
        },
        "the reference moved to the copy that already existed"
    );
}

#[tokio::test]
async fn disconnect_keeps_personal_and_shared_items_locally() {
    let url = agent().await;
    let mut alice = Vessel::in_memory().unwrap();
    let recovery = alice
        .signup(&url, "alice@example.com", PW, "pc-1", None)
        .await
        .unwrap();
    let mut bob = Vessel::in_memory().unwrap();
    bob.signup(&url, "bob@example.com", PW, "pc-1", None)
        .await
        .unwrap();

    let personal = alice.personal_vault().unwrap();
    alice
        .save(personal, None, host("mine", "10.0.0.1"))
        .unwrap();
    alice.sync().await.unwrap();

    let team = bob.create_vault("Time").await.unwrap();
    bob.save(team, None, host("shared", "10.0.0.2")).unwrap();
    bob.sync().await.unwrap();
    let invite = bob.invite(team, Role::Viewer, None).await.unwrap();
    alice.accept_invite(&invite.url).await.unwrap();
    let alice_id = alice.status().user_id.unwrap();
    bob.approve_member(team, alice_id).await.unwrap();
    alice.sync().await.unwrap();
    assert_eq!(labels(&alice, EntityKind::Host), vec!["mine", "shared"]);

    let report = alice.disconnect_to_local().await.unwrap();
    assert_eq!(report.imported, 2);
    assert!(alice.is_local());
    assert_eq!(alice.vaults().unwrap().len(), 1);
    assert_eq!(labels(&alice, EntityKind::Host), vec!["mine", "shared"]);

    // Same password and recovery key, offline.
    alice.lock();
    alice.unlock(PW).unwrap();
    alice.lock();
    alice.recover_local(&recovery, PW2).unwrap();
    assert_eq!(labels(&alice, EntityKind::Host), vec!["mine", "shared"]);
}

#[test]
fn device_data_is_sealed_and_needs_the_unlocked_vault() {
    let db = temp_db("device.db");
    let mut v = Vessel::open(&db).unwrap();
    v.create_local("x", PW).unwrap();
    let history = vec!["secret output 4711".to_string()];
    v.set_device_sealed("runs", &history).unwrap();
    v.set_device_plain("tick", &1234_i64).unwrap();
    assert_eq!(
        v.device_sealed::<Vec<String>>("runs").unwrap(),
        Some(history.clone())
    );

    v.lock();
    assert!(matches!(
        v.device_sealed::<Vec<String>>("runs"),
        Err(ClientError::Locked)
    ));
    assert_eq!(v.device_plain::<i64>("tick").unwrap(), Some(1234));
    drop(v);
    let raw = std::fs::read(&db).unwrap();
    assert!(
        !raw.windows(11).any(|w| w == b"output 4711"),
        "history must not be on disk in clear"
    );

    let mut v = Vessel::open(&db).unwrap();
    v.unlock(PW).unwrap();
    assert_eq!(
        v.device_sealed::<Vec<String>>("runs").unwrap(),
        Some(history)
    );
}
