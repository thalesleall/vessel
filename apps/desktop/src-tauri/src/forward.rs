//! Port forwarding rules (`-L`, `-R`, `-D`), each over its own SSH connection.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use uuid::Uuid;
use vessel_client::model::{ForwardKind, PortForward};

use crate::error::{CmdError, CmdResult};
use crate::ssh::Connection;

#[derive(Clone, Debug, Serialize)]
pub struct ForwardStatus {
    pub rule_id: Uuid,
    pub label: String,
    pub description: String,
    pub running: bool,
    pub error: Option<String>,
}

struct Running {
    status: Arc<Mutex<ForwardStatus>>,
    task: JoinHandle<()>,
}

#[derive(Default)]
pub struct Forwards {
    running: Mutex<HashMap<Uuid, Running>>,
}

pub fn describe(rule: &PortForward) -> String {
    let bind = if rule.bind_address.is_empty() {
        "127.0.0.1"
    } else {
        &rule.bind_address
    };
    match rule.kind {
        ForwardKind::Local => format!(
            "{bind}:{} → {}:{}",
            rule.bind_port, rule.dest_host, rule.dest_port
        ),
        ForwardKind::Remote => format!(
            "remoto {bind}:{} → {}:{}",
            rule.bind_port, rule.dest_host, rule.dest_port
        ),
        ForwardKind::Dynamic => format!("SOCKS5 em {bind}:{}", rule.bind_port),
    }
}

impl Forwards {
    pub fn list(&self) -> Vec<ForwardStatus> {
        let running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        running
            .values()
            .map(|r| r.status.lock().unwrap_or_else(|e| e.into_inner()).clone())
            .collect()
    }

    pub fn is_running(&self, id: &Uuid) -> bool {
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(id)
    }

    pub async fn stop(&self, id: &Uuid) -> bool {
        let removed = self
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        if let Some(r) = removed {
            r.task.abort();
            true
        } else {
            false
        }
    }

    pub fn stop_all(&self) {
        for (_, r) in self
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
        {
            r.task.abort();
        }
    }

    /// Starts a rule over an established connection.
    pub async fn start(
        &self,
        rule_id: Uuid,
        rule: PortForward,
        conn: Connection,
    ) -> CmdResult<ForwardStatus> {
        if self.is_running(&rule_id) {
            return Err(CmdError::new(
                "already_running",
                "Esse redirecionamento já está ativo.",
            ));
        }
        let bind = if rule.bind_address.is_empty() {
            "127.0.0.1".to_string()
        } else {
            rule.bind_address.clone()
        };
        let status = Arc::new(Mutex::new(ForwardStatus {
            rule_id,
            label: rule.label.clone(),
            description: describe(&rule),
            running: true,
            error: None,
        }));
        let conn = Arc::new(conn);
        let task = match rule.kind {
            ForwardKind::Local | ForwardKind::Dynamic => {
                let listener = TcpListener::bind((bind.as_str(), rule.bind_port))
                    .await
                    .map_err(|e| {
                        CmdError::new(
                            "bind_failed",
                            format!("Não foi possível abrir {bind}:{}: {e}", rule.bind_port),
                        )
                    })?;
                let status2 = status.clone();
                tokio::spawn(async move {
                    loop {
                        let Ok((socket, peer)) = listener.accept().await else {
                            break;
                        };
                        let conn = conn.clone();
                        let rule = rule.clone();
                        tokio::spawn(async move {
                            let _ = match rule.kind {
                                ForwardKind::Dynamic => socks5(conn, socket).await,
                                _ => {
                                    tunnel(
                                        conn,
                                        socket,
                                        &rule.dest_host,
                                        rule.dest_port,
                                        peer.ip().to_string(),
                                        peer.port(),
                                    )
                                    .await
                                }
                            };
                        });
                    }
                    let mut s = status2.lock().unwrap_or_else(|e| e.into_inner());
                    s.running = false;
                    s.error = Some("parou de aceitar conexões".into());
                })
            }
            ForwardKind::Remote => {
                conn.remote_forwards
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(
                        u32::from(rule.bind_port),
                        (rule.dest_host.clone(), rule.dest_port),
                    );
                conn.target()
                    .tcpip_forward(bind.clone(), u32::from(rule.bind_port))
                    .await?;
                let status2 = status.clone();
                tokio::spawn(async move {
                    // Keep the connection alive; the SSH handler serves forwarded channels.
                    while !conn.target().is_closed() {
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    }
                    let mut s = status2.lock().unwrap_or_else(|e| e.into_inner());
                    s.running = false;
                    s.error = Some("conexão SSH encerrada".into());
                })
            }
        };
        let snapshot = status.lock().unwrap_or_else(|e| e.into_inner()).clone();
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(rule_id, Running { status, task });
        Ok(snapshot)
    }
}

async fn tunnel(
    conn: Arc<Connection>,
    mut socket: TcpStream,
    host: &str,
    port: u16,
    origin: String,
    origin_port: u16,
) -> CmdResult<()> {
    let channel = conn
        .target()
        .channel_open_direct_tcpip(
            host.to_string(),
            u32::from(port),
            origin,
            u32::from(origin_port),
        )
        .await?;
    let mut stream = channel.into_stream();
    tokio::io::copy_bidirectional(&mut stream, &mut socket).await?;
    Ok(())
}

/// Minimal SOCKS5 (no auth, CONNECT only).
async fn socks5(conn: Arc<Connection>, mut socket: TcpStream) -> CmdResult<()> {
    let mut head = [0u8; 2];
    socket.read_exact(&mut head).await?;
    if head[0] != 5 {
        return Err("not socks5".into());
    }
    let mut methods = vec![0u8; head[1] as usize];
    socket.read_exact(&mut methods).await?;
    socket.write_all(&[5, 0]).await?;

    let mut req = [0u8; 4];
    socket.read_exact(&mut req).await?;
    if req[1] != 1 {
        socket.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
        return Err("only CONNECT is supported".into());
    }
    let host = match req[3] {
        1 => {
            let mut a = [0u8; 4];
            socket.read_exact(&mut a).await?;
            Ipv4Addr::from(a).to_string()
        }
        3 => {
            let mut len = [0u8; 1];
            socket.read_exact(&mut len).await?;
            let mut name = vec![0u8; len[0] as usize];
            socket.read_exact(&mut name).await?;
            String::from_utf8_lossy(&name).into_owned()
        }
        4 => {
            let mut a = [0u8; 16];
            socket.read_exact(&mut a).await?;
            Ipv6Addr::from(a).to_string()
        }
        _ => return Err("bad address type".into()),
    };
    let mut port = [0u8; 2];
    socket.read_exact(&mut port).await?;
    let port = u16::from_be_bytes(port);
    match conn
        .target()
        .channel_open_direct_tcpip(host, u32::from(port), "127.0.0.1", 0)
        .await
    {
        Ok(channel) => {
            socket.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            let mut stream = channel.into_stream();
            tokio::io::copy_bidirectional(&mut stream, &mut socket).await?;
            Ok(())
        }
        Err(e) => {
            socket.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            Err(e.into())
        }
    }
}
