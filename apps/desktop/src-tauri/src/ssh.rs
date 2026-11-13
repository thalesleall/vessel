//! SSH connections: credential resolution from synced entities, host-key verification against
//! synced known hosts (trust on first use), jump-host chains, interactive shells.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::client::{self, AuthResult, Handle};
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::{ChannelMsg, Disconnect};
use serde_json::json;
use uuid::Uuid;
use vessel_client::Vessel;
use vessel_client::model::{Entity, EntityKind, Host, KnownHost};

use crate::error::{CmdError, CmdResult};
use crate::terminal::{Input, SessionBackend};

/// Everything needed to reach one host, resolved while the vault is unlocked.
#[derive(Clone)]
pub struct Hop {
    pub label: String,
    pub address: String,
    pub port: u16,
    pub username: String,
    pub password: Option<String>,
    pub key: Option<(String, Option<String>)>,
    pub known_fingerprint: Option<String>,
    pub keepalive: Option<u32>,
}

impl Hop {
    pub fn host_port(&self) -> String {
        format!("{}:{}", self.address, self.port)
    }
}

#[derive(Clone)]
pub struct Plan {
    /// First hop first; the target is last.
    pub hops: Vec<Hop>,
    pub startup_script: Option<String>,
}

/// Where references are allowed to resolve. Another member of a shared vault must not be able to
/// redirect the user's references (credentials, pins, snippets, jump hosts) to records they
/// control, so a reference made by an item resolves only in that item's own vault or in the
/// user's personal vault, and trust pins come only from the personal vault.
struct Scope<'a> {
    items: &'a [vessel_client::Item],
    personal: Option<Uuid>,
}

impl<'a> Scope<'a> {
    fn new(items: &'a [vessel_client::Item], vessel: &Vessel) -> Self {
        Self {
            items,
            personal: vessel.personal_vault().ok(),
        }
    }

    /// A reference made by an item stored in `origin`.
    fn get(&self, origin: Uuid, id: Uuid) -> Option<&'a vessel_client::Item> {
        let in_vault = |v: Uuid| self.items.iter().find(|i| i.id == id && i.vault_id == v);
        in_vault(origin).or_else(|| self.personal.and_then(in_vault))
    }

    /// An entry point chosen by the user (the host they clicked). Refuses an id present in
    /// several vaults instead of picking one.
    fn entry(&self, id: Uuid) -> CmdResult<&'a vessel_client::Item> {
        let mut matches = self.items.iter().filter(|i| i.id == id);
        let first = matches.next();
        if first.is_some() && matches.next().is_some() {
            return Err(CmdError::new(
                "invalid",
                "Esse host aparece em mais de um cofre com o mesmo id; a conexão foi recusada.",
            ));
        }
        first.ok_or_else(|| CmdError::new("not_found", "Host não encontrado."))
    }

    fn pin(&self, host_port: &str) -> Option<String> {
        let personal = self.personal?;
        self.items.iter().find_map(|i| match &i.entity {
            Entity::KnownHost(k) if i.vault_id == personal && k.host == host_port => {
                Some(k.fingerprint.clone())
            }
            _ => None,
        })
    }
}

fn hop_for(scope: &Scope, origin: Uuid, host: &Host) -> CmdResult<Hop> {
    let find = |owner: Uuid, id: Uuid| scope.get(owner, id).map(|i| (i.vault_id, &i.entity));
    // Credentials: inline values on the host win, then the host's identity, then its groups'.
    let mut identity_ref = host.identity_id.map(|id| (origin, id));
    let mut group = host.group_id.map(|id| (origin, id));
    let mut depth = 0;
    while identity_ref.is_none() && depth < 8 {
        let Some((vault, Entity::Group(g))) = group.and_then(|(o, id)| find(o, id)) else {
            break;
        };
        identity_ref = g.identity_id.map(|id| (vault, id));
        group = g.parent_id.map(|id| (vault, id));
        depth += 1;
    }
    let identity = identity_ref.and_then(|(o, id)| match find(o, id) {
        Some((vault, Entity::Identity(i))) => Some((vault, i.clone())),
        _ => None,
    });
    let identity_vault = identity.as_ref().map(|(v, _)| *v);
    let identity = identity.map(|(_, i)| i);
    let username = host
        .username
        .clone()
        .filter(|u| !u.is_empty())
        .or_else(|| {
            identity
                .as_ref()
                .map(|i| i.username.clone())
                .filter(|u| !u.is_empty())
        })
        .ok_or_else(|| {
            CmdError::new(
                "no_username",
                format!("Defina um usuário para {}.", host.address),
            )
        })?;
    let password = host
        .password
        .clone()
        .filter(|p| !p.is_empty())
        .or_else(|| identity.as_ref().and_then(|i| i.password.clone()));
    let key_ref = host.ssh_key_id.map(|id| (origin, id)).or_else(|| {
        identity
            .as_ref()
            .and_then(|i| i.ssh_key_id)
            .map(|id| (identity_vault.unwrap_or(origin), id))
    });
    let key = key_ref.and_then(|(o, id)| match find(o, id) {
        Some((_, Entity::SshKey(k))) if !k.private_key.trim().is_empty() => {
            Some((k.private_key.clone(), k.passphrase.clone()))
        }
        _ => None,
    });
    let port = host.effective_port();
    let known_fingerprint = scope.pin(&format!("{}:{port}", host.address));
    Ok(Hop {
        label: if host.label.is_empty() {
            host.address.clone()
        } else {
            host.label.clone()
        },
        address: host.address.clone(),
        port,
        username,
        password,
        key,
        known_fingerprint,
        keepalive: host.keepalive_secs,
    })
}

/// Resolves a host (and its jump chain) into a connection plan.
pub fn plan(vessel: &Vessel, host_id: Uuid) -> CmdResult<Plan> {
    let items = vessel.items(None)?;
    let scope = Scope::new(&items, vessel);
    let as_host = |item: &vessel_client::Item| match &item.entity {
        Entity::Host(h) => Ok((item.vault_id, h.clone())),
        _ => Err(CmdError::new("not_found", "Host não encontrado.")),
    };
    let (target_vault, target) = as_host(scope.entry(host_id)?)?;
    if target.address.trim().is_empty() {
        return Err(CmdError::new("invalid", "O host não tem endereço."));
    }
    let startup_script = target.startup_snippet_id.and_then(|id| {
        match scope.get(target_vault, id).map(|i| &i.entity) {
            Some(Entity::Snippet(s)) => Some(s.script.clone()),
            _ => None,
        }
    });
    let mut chain = vec![(target_vault, target.clone())];
    let mut next = target.jump_host_id.map(|id| (target_vault, id));
    while let Some((origin, id)) = next {
        if chain.len() > 4 || id == host_id {
            return Err(CmdError::new(
                "invalid",
                "Cadeia de jump hosts muito longa ou circular.",
            ));
        }
        let item = scope
            .get(origin, id)
            .ok_or_else(|| CmdError::new("not_found", "Jump host não encontrado."))?;
        let (vault, jump) = as_host(item)?;
        next = jump.jump_host_id.map(|id| (vault, id));
        chain.push((vault, jump));
    }
    chain.reverse();
    let hops = chain
        .iter()
        .map(|(vault, h)| hop_for(&scope, *vault, h))
        .collect::<CmdResult<Vec<_>>>()?;
    Ok(Plan {
        hops,
        startup_script,
    })
}

/// Host keys seen while connecting that were not known yet (to save after success).
#[derive(Clone, Debug)]
pub struct NewHostKey {
    pub host: String,
    pub key_type: String,
    pub fingerprint: String,
}

pub struct Client {
    host: String,
    expected: Option<String>,
    trust_new: bool,
    seen: Arc<Mutex<Option<NewHostKey>>>,
    pub remote_forwards: Arc<Mutex<HashMap<u32, (String, u16)>>>,
}

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let (fingerprint, key_type) = match key {
            PublicKeyOrCertificate::PublicKey { key, .. } => (
                key.fingerprint(HashAlg::Sha256).to_string(),
                key.algorithm().as_str().to_string(),
            ),
            PublicKeyOrCertificate::Certificate(cert) => {
                let public = russh::keys::ssh_key::PublicKey::new(cert.public_key().clone(), "");
                (
                    public.fingerprint(HashAlg::Sha256).to_string(),
                    cert.algorithm().as_str().to_string(),
                )
            }
        };
        let accepted = match &self.expected {
            Some(expected) => *expected == fingerprint,
            None => self.trust_new,
        };
        *self.seen.lock().unwrap_or_else(|e| e.into_inner()) = Some(NewHostKey {
            host: self.host.clone(),
            key_type,
            fingerprint,
        });
        Ok(accepted)
    }

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: russh::Channel<client::Msg>,
        _connected_address: &str,
        connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        let dest = self
            .remote_forwards
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&connected_port)
            .cloned();
        let Some((host, port)) = dest else {
            return Ok(()); // dropping `reply` rejects the channel
        };
        reply.accept().await;
        tokio::spawn(async move {
            if let Ok(mut tcp) = tokio::net::TcpStream::connect((host.as_str(), port)).await {
                let mut stream = channel.into_stream();
                let _ = tokio::io::copy_bidirectional(&mut stream, &mut tcp).await;
            }
        });
        Ok(())
    }
}

/// A live connection: the target handle plus the jump connections it rides on.
pub struct Connection {
    handles: Vec<Handle<Client>>,
    pub new_host_keys: Vec<NewHostKey>,
    pub remote_forwards: Arc<Mutex<HashMap<u32, (String, u16)>>>,
}

impl Connection {
    pub fn target(&self) -> &Handle<Client> {
        self.handles
            .last()
            .expect("a connection has at least one hop")
    }

    pub async fn close(&self) {
        for h in self.handles.iter().rev() {
            let _ = h.disconnect(Disconnect::ByApplication, "", "pt-BR").await;
        }
    }
}

fn config(hop: &Hop) -> Arc<client::Config> {
    Arc::new(client::Config {
        inactivity_timeout: None,
        keepalive_interval: Some(Duration::from_secs(u64::from(
            hop.keepalive.unwrap_or(30).max(5),
        ))),
        keepalive_max: 4,
        nodelay: true,
        ..Default::default()
    })
}

async fn authenticate(handle: &mut Handle<Client>, hop: &Hop) -> CmdResult<()> {
    if let Some((pem, passphrase)) = &hop.key {
        let key =
            russh::keys::decode_secret_key(pem, passphrase.as_deref().filter(|p| !p.is_empty()))
                .map_err(|e| {
                    CmdError::new("bad_key", format!("Não foi possível ler a chave SSH: {e}"))
                })?;
        let hash = if key.algorithm().is_rsa() {
            handle.best_supported_rsa_hash().await?.flatten()
        } else {
            None
        };
        let result = handle
            .authenticate_publickey(
                &hop.username,
                PrivateKeyWithHashAlg::new(Arc::new(key), hash),
            )
            .await?;
        if matches!(result, AuthResult::Success) {
            return Ok(());
        }
    }
    if let Some(password) = &hop.password {
        let result = handle
            .authenticate_password(&hop.username, password)
            .await?;
        if matches!(result, AuthResult::Success) {
            return Ok(());
        }
    }
    Err(CmdError::new(
        "auth_failed",
        format!(
            "Autenticação recusada em {} como {}.",
            hop.label, hop.username
        ),
    ))
}

/// Connects through every hop. `trust_new` accepts unknown host keys (they are returned in
/// `new_host_keys` to be saved); a changed key is always refused.
pub async fn connect(plan: &Plan, trust_new: bool) -> CmdResult<Connection> {
    let remote_forwards = Arc::new(Mutex::new(HashMap::new()));
    let mut handles: Vec<Handle<Client>> = Vec::new();
    let mut new_host_keys = Vec::new();
    for hop in &plan.hops {
        let seen = Arc::new(Mutex::new(None));
        let handler = Client {
            host: hop.host_port(),
            expected: hop.known_fingerprint.clone(),
            trust_new,
            seen: seen.clone(),
            remote_forwards: remote_forwards.clone(),
        };
        let result = match handles.last() {
            None => tokio::time::timeout(
                Duration::from_secs(15),
                client::connect(config(hop), (hop.address.as_str(), hop.port), handler),
            )
            .await
            .map_err(|_| {
                CmdError::new(
                    "timeout",
                    format!("Tempo esgotado ao conectar em {}.", hop.host_port()),
                )
            })?,
            Some(previous) => {
                let channel = previous
                    .channel_open_direct_tcpip(
                        hop.address.clone(),
                        u32::from(hop.port),
                        "127.0.0.1",
                        0,
                    )
                    .await?;
                client::connect_stream(config(hop), channel.into_stream(), handler).await
            }
        };
        let seen = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let mut handle = match result {
            Ok(h) => h,
            Err(e) => {
                for h in handles.iter().rev() {
                    let _ = h.disconnect(Disconnect::ByApplication, "", "pt-BR").await;
                }
                return Err(host_key_error(hop, seen).unwrap_or_else(|| {
                    CmdError::new(
                        "connect_failed",
                        format!("Não foi possível conectar em {}: {e}", hop.host_port()),
                    )
                }));
            }
        };
        if hop.known_fingerprint.is_none()
            && let Some(key) = seen
        {
            new_host_keys.push(key);
        }
        authenticate(&mut handle, hop).await?;
        handles.push(handle);
    }
    Ok(Connection {
        handles,
        new_host_keys,
        remote_forwards,
    })
}

fn host_key_error(hop: &Hop, seen: Option<NewHostKey>) -> Option<CmdError> {
    let seen = seen?;
    match &hop.known_fingerprint {
        Some(expected) if *expected != seen.fingerprint => Some(
            CmdError::new(
                "host_key_changed",
                format!(
                    "ATENÇÃO: a chave do servidor {} mudou. Pode ser uma reinstalação ou um ataque man-in-the-middle.",
                    seen.host
                ),
            )
            .with_details(json!({ "host": seen.host, "expected": expected, "actual": seen.fingerprint, "key_type": seen.key_type })),
        ),
        None => Some(
            CmdError::new("host_key_unknown", format!("Primeira conexão com {}. Confirme a impressão digital do servidor.", seen.host))
                .with_details(json!({ "host": seen.host, "fingerprint": seen.fingerprint, "key_type": seen.key_type })),
        ),
        _ => None,
    }
}

/// Saves host keys accepted on first use as synced `KnownHost` entities.
pub fn remember_host_keys(vessel: &mut Vessel, keys: &[NewHostKey]) -> CmdResult<()> {
    if keys.is_empty() {
        return Ok(());
    }
    // Trust decisions are personal: they live only in the personal vault, the only place pins
    // are read from (see `Scope::pin`).
    let vault = vessel.personal_vault()?;
    let existing = vessel.items(Some(EntityKind::KnownHost))?;
    for key in keys {
        let entity = Entity::KnownHost(KnownHost {
            host: key.host.clone(),
            key_type: key.key_type.clone(),
            fingerprint: key.fingerprint.clone(),
            ..Default::default()
        });
        let id = existing
            .iter()
            .find(|i| {
                i.vault_id == vault
                    && matches!(&i.entity, Entity::KnownHost(k) if k.host == key.host)
            })
            .map(|i| i.id);
        vessel.save(vault, id, entity)?;
    }
    Ok(())
}

/// Runs an interactive shell on an established connection until either side closes it.
pub fn run_shell(
    conn: Connection,
    backend: SessionBackend,
    cols: u16,
    rows: u16,
    startup: Option<String>,
) {
    let SessionBackend { session, mut input } = backend;
    tokio::spawn(async move {
        let result: CmdResult<()> = async {
            let mut channel = conn.target().channel_open_session().await?;
            channel.request_pty(false, "xterm-256color", u32::from(cols), u32::from(rows), 0, 0, &[]).await?;
            channel.request_shell(false).await?;
            if let Some(script) = startup.filter(|s| !s.trim().is_empty()) {
                let script = if script.ends_with('\n') { script } else { format!("{script}\n") };
                channel.data(script.as_bytes()).await?;
            }
            loop {
                tokio::select! {
                    msg = channel.wait() => match msg {
                        Some(ChannelMsg::Data { data }) => session.push_output(&data),
                        Some(ChannelMsg::ExtendedData { data, .. }) => session.push_output(&data),
                        Some(ChannelMsg::ExitStatus { exit_status }) => {
                            session.mark_exited(&format!("sessão encerrada com código {exit_status}"));
                        }
                        Some(ChannelMsg::Eof | ChannelMsg::Close) | None => break,
                        _ => {}
                    },
                    cmd = input.recv() => match cmd {
                        Some(Input::Data(bytes)) => channel.data(bytes.as_slice()).await?,
                        Some(Input::Resize { cols, rows }) => channel.window_change(u32::from(cols), u32::from(rows), 0, 0).await?,
                        Some(Input::Close) | None => {
                            let _ = channel.close().await;
                            break;
                        }
                    },
                }
            }
            Ok(())
        }
        .await;
        conn.close().await;
        match result {
            Ok(()) => session.mark_exited("conexão encerrada"),
            Err(e) => session.mark_exited(&e.message),
        }
    });
}
