//! Integration tests of the real SSH and port-forward code against the Docker test lab.
//! They are `#[ignore]`d because they need the lab running:
//!
//! ```text
//! powershell -File deploy\dev\lab.ps1 up
//! cargo test -p vessel-desktop -- --ignored lab_
//! ```

use std::path::PathBuf;
use std::time::Duration;

use russh::ChannelMsg;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;
use vessel_client::model::{ForwardKind, PortForward};

use crate::forward::Forwards;
use crate::ssh::{self, Connection, Hop, Plan};

fn lab(file: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../deploy/dev/.lab")
        .join(file);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| {
            panic!(
                "{} ({e}); start the lab first: deploy/dev/lab.ps1 up",
                path.display()
            )
        })
        .trim()
        .to_string()
}

fn bastion() -> Hop {
    Hop {
        label: "bastion".into(),
        address: "127.0.0.1".into(),
        port: 2222,
        username: "dev".into(),
        password: None,
        key: None,
        known_fingerprint: None,
        keepalive: None,
    }
}

fn target() -> Hop {
    Hop {
        label: "target".into(),
        address: "target".into(),
        port: 22,
        ..bastion()
    }
}

fn with_password(mut h: Hop) -> Hop {
    h.password = Some(lab("password"));
    h
}

fn with_key(mut h: Hop) -> Hop {
    h.key = Some((lab("id_ed25519"), None));
    h
}

fn plan(hops: Vec<Hop>) -> Plan {
    Plan {
        hops,
        startup_script: None,
    }
}

async fn exec(conn: &Connection, command: &str) -> String {
    let mut ch = conn.target().channel_open_session().await.unwrap();
    ch.exec(true, command).await.unwrap();
    let mut out = Vec::new();
    while let Some(msg) = ch.wait().await {
        match msg {
            ChannelMsg::Data { data } => out.extend_from_slice(&data),
            ChannelMsg::Eof | ChannelMsg::Close => break,
            _ => {}
        }
    }
    String::from_utf8_lossy(&out).trim().to_string()
}

async fn http_get(port: u16) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    s.write_all(b"GET / HTTP/1.0\r\nHost: target\r\n\r\n")
        .await
        .unwrap();
    let mut body = String::new();
    s.read_to_string(&mut body).await.unwrap();
    body
}

#[tokio::test]
#[ignore = "needs the Docker test lab"]
async fn lab_unknown_host_key_is_reported_then_trusted() {
    let p = plan(vec![with_password(bastion())]);
    let err = ssh::connect(&p, false)
        .await
        .err()
        .expect("must refuse an unknown key");
    assert_eq!(err.code, "host_key_unknown", "{}", err.message);

    let conn = ssh::connect(&p, true).await.unwrap();
    assert_eq!(conn.new_host_keys.len(), 1);
    let fp = conn.new_host_keys[0].fingerprint.clone();
    assert!(fp.starts_with("SHA256:"), "{fp}");
    assert_eq!(exec(&conn, "hostname").await, "bastion");
    conn.close().await;

    // Once pinned, the same key connects without prompting.
    let mut pinned = with_password(bastion());
    pinned.known_fingerprint = Some(fp);
    let conn = ssh::connect(&plan(vec![pinned]), false).await.unwrap();
    assert!(conn.new_host_keys.is_empty());
    conn.close().await;
}

#[tokio::test]
#[ignore = "needs the Docker test lab"]
async fn lab_changed_host_key_is_refused_even_when_trusting() {
    let mut h = with_password(bastion());
    h.known_fingerprint = Some("SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into());
    let err = ssh::connect(&plan(vec![h]), true)
        .await
        .err()
        .expect("must refuse");
    assert_eq!(err.code, "host_key_changed", "{}", err.message);
}

#[tokio::test]
#[ignore = "needs the Docker test lab"]
async fn lab_key_auth_with_and_without_passphrase() {
    let conn = ssh::connect(&plan(vec![with_key(bastion())]), true)
        .await
        .unwrap();
    assert_eq!(exec(&conn, "whoami").await, "dev");
    conn.close().await;

    let mut h = bastion();
    h.key = Some((lab("id_ed25519_passphrase"), Some(lab("passphrase"))));
    let conn = ssh::connect(&plan(vec![h.clone()]), true).await.unwrap();
    assert_eq!(exec(&conn, "whoami").await, "dev");
    conn.close().await;

    h.key = Some((lab("id_ed25519_passphrase"), Some("errada".into())));
    let err = ssh::connect(&plan(vec![h]), true)
        .await
        .err()
        .expect("wrong passphrase");
    assert_eq!(err.code, "bad_key", "{}", err.message);

    let mut h = bastion();
    h.password = Some("senha-errada".into());
    let err = ssh::connect(&plan(vec![h]), true)
        .await
        .err()
        .expect("wrong password");
    assert_eq!(err.code, "auth_failed", "{}", err.message);
}

#[tokio::test]
#[ignore = "needs the Docker test lab"]
async fn lab_jump_host_reaches_internal_target() {
    let conn = ssh::connect(
        &plan(vec![with_key(bastion()), with_password(target())]),
        true,
    )
    .await
    .unwrap();
    assert_eq!(conn.new_host_keys.len(), 2);
    assert_eq!(conn.new_host_keys[1].host, "target:22");
    assert_eq!(exec(&conn, "hostname").await, "target");
    assert!(
        exec(&conn, "curl -s localhost:8000")
            .await
            .contains("target OK")
    );
    conn.close().await;
}

fn rule(kind: ForwardKind, bind_port: u16, dest_host: &str, dest_port: u16) -> PortForward {
    PortForward {
        label: format!("{kind:?}"),
        kind,
        bind_port,
        dest_host: dest_host.into(),
        dest_port,
        ..Default::default()
    }
}

#[tokio::test]
#[ignore = "needs the Docker test lab"]
async fn lab_local_forward() {
    let forwards = Forwards::default();
    let conn = ssh::connect(&plan(vec![with_key(bastion())]), true)
        .await
        .unwrap();
    let id = Uuid::new_v4();
    forwards
        .start(id, rule(ForwardKind::Local, 18000, "target", 8000), conn)
        .await
        .unwrap();
    assert!(http_get(18000).await.contains("target OK"));
    assert!(forwards.stop(&id).await);
}

#[tokio::test]
#[ignore = "needs the Docker test lab"]
async fn lab_socks_forward() {
    let forwards = Forwards::default();
    let conn = ssh::connect(&plan(vec![with_key(bastion())]), true)
        .await
        .unwrap();
    let id = Uuid::new_v4();
    forwards
        .start(id, rule(ForwardKind::Dynamic, 18001, "", 0), conn)
        .await
        .unwrap();

    let mut s = TcpStream::connect(("127.0.0.1", 18001)).await.unwrap();
    s.write_all(&[5, 1, 0]).await.unwrap();
    let mut hello = [0u8; 2];
    s.read_exact(&mut hello).await.unwrap();
    assert_eq!(hello, [5, 0]);
    let name = b"target";
    let mut req = vec![5, 1, 0, 3, name.len() as u8];
    req.extend_from_slice(name);
    req.extend_from_slice(&8000u16.to_be_bytes());
    s.write_all(&req).await.unwrap();
    let mut reply = [0u8; 10];
    s.read_exact(&mut reply).await.unwrap();
    assert_eq!(reply[1], 0, "SOCKS connect failed: {reply:?}");
    s.write_all(b"GET / HTTP/1.0\r\n\r\n").await.unwrap();
    let mut body = String::new();
    s.read_to_string(&mut body).await.unwrap();
    assert!(body.contains("target OK"), "{body}");
    forwards.stop(&id).await;
}

#[tokio::test]
#[ignore = "needs the Docker test lab"]
async fn lab_remote_forward() {
    // Something only this machine serves.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local_port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            let _ = s.read(&mut [0u8; 1024]).await;
            let _ = s
                .write_all(b"HTTP/1.0 200 OK\r\n\r\nresposta do Windows")
                .await;
        }
    });

    let forwards = Forwards::default();
    let conn = ssh::connect(&plan(vec![with_key(bastion())]), true)
        .await
        .unwrap();
    let id = Uuid::new_v4();
    forwards
        .start(
            id,
            rule(ForwardKind::Remote, 19080, "127.0.0.1", local_port),
            conn,
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // From inside the bastion, port 19080 must reach the listener above.
    let check = ssh::connect(&plan(vec![with_key(bastion())]), true)
        .await
        .unwrap();
    let out = exec(&check, "curl -s --max-time 5 localhost:19080").await;
    assert_eq!(out, "resposta do Windows");
    check.close().await;
    forwards.stop(&id).await;
}

#[tokio::test]
#[ignore = "needs the Docker test lab"]
async fn lab_interactive_shell_has_utf8_locale() {
    let conn = ssh::connect(&plan(vec![with_key(bastion())]), true)
        .await
        .unwrap();
    let out = exec(&conn, "locale charmap; echo ação ✓").await;
    assert_eq!(out, "UTF-8\nação ✓");
    conn.close().await;
}

/// Dumps the raw bytes ConPTY produces for a command, to see how it encodes line breaks.
#[test]
#[ignore = "diagnostic; run by hand"]
fn conpty_raw_dump() {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::io::{Read, Write};
    let pty = native_pty_system()
        .openpty(PtySize {
            rows: 32,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut child = pty
        .slave
        .spawn_command(CommandBuilder::new("cmd.exe"))
        .unwrap();
    let mut reader = pty.master.try_clone_reader().unwrap();
    let mut writer = pty.master.take_writer().unwrap();
    let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let b2 = buf.clone();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        while let Ok(n) = reader.read(&mut chunk) {
            if n == 0 {
                break;
            }
            b2.lock().unwrap().extend_from_slice(&chunk[..n]);
        }
    });
    std::thread::sleep(Duration::from_millis(300));
    writer.write_all(b"[1;1R").unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    let start = buf.lock().unwrap().len();
    writer.write_all(b"echo ola do agente\r").unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    let all = buf.lock().unwrap().clone();
    println!("STARTUP: {:?}", String::from_utf8_lossy(&all[..start]));
    println!("AFTER: {:?}", String::from_utf8_lossy(&all[start..]));
    println!("PLAIN: {:?}", crate::terminal::to_plain_text(&all[start..]));
    let _ = child.kill();
}
