//! The AWS screen: the CLI profiles of this computer, an on-demand map of their EC2 instances,
//! and Session Manager sessions (shell, SSH and port forward) to them. Local only: nothing about
//! AWS goes to the synced vault or the server, and Vessel never reads or keeps AWS keys.

pub mod cli;
pub mod config;
pub mod inventory;
pub mod snippets;

#[cfg(test)]
mod fake_tests;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use tauri::Emitter;
use uuid::Uuid;
use vessel_client::model::{Entity, Shell, TerminalProfile};

use crate::error::{CmdError, CmdResult};
use crate::state::AppState;
use crate::terminal::{SessionInfo, SessionKind};
use cli::{AwsError, Credentials, Tools};
use config::Profile;
use inventory::{Cancel, Inventory, Job, Limits, ProfileNode, Progress, RegionNode};

const INVENTORY_KEY: &str = "aws-inventory";
const SETTINGS_KEY: &str = "aws-settings";
const HOST_KEYS_KEY: &str = "aws-host-keys";

/// Profile choice for "Mapear tudo" and the session check: every profile, or one master and
/// the profiles that assume roles from it.
pub const ALL_PROFILES: &str = "*";

#[derive(Default)]
pub struct AwsState {
    tools: tokio::sync::Mutex<Option<Tools>>,
    mapping: Mutex<Option<Cancel>>,
    http: OnceLock<aws_sdk_ec2::config::SharedHttpClient>,
}

impl AwsState {
    pub async fn tools(&self, refresh: bool) -> Tools {
        let mut t = self.tools.lock().await;
        if refresh || t.is_none() {
            *t = Some(cli::detect().await);
        }
        t.clone().unwrap_or_default()
    }
    pub fn http(&self) -> aws_sdk_ec2::config::SharedHttpClient {
        self.http.get_or_init(inventory::shared_http_client).clone()
    }
    pub fn is_mapping(&self) -> bool {
        self.mapping
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }
}

/// Choices kept on this device (sealed, not synced): they reveal accounts and instances.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AwsSettings {
    /// The master profile ("*" for every profile); None picks the likeliest.
    pub master: Option<String>,
    /// Regions to map per profile; a profile absent here maps every enabled region.
    pub regions: BTreeMap<String, Vec<String>>,
    pub favorites: Vec<Favorite>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Favorite {
    pub profile: String,
    pub region: String,
    pub id: String,
}

#[derive(Serialize)]
pub struct Overview {
    pub tools: Tools,
    pub config_path: Option<String>,
    pub config_error: Option<String>,
    pub profiles: Vec<Profile>,
    /// The master in use: the chosen one, or the likeliest.
    pub master: Option<String>,
    pub settings: AwsSettings,
    pub mapped_at: Option<i64>,
    pub partial: bool,
    pub instance_count: usize,
    pub mapping: bool,
    pub agents_allowed: bool,
}

fn load_profiles() -> (Vec<Profile>, Option<String>, Option<String>) {
    let Some(path) = config::config_path() else {
        return (
            Vec::new(),
            None,
            Some("Pasta do usuário não encontrada.".into()),
        );
    };
    match config::load(&path) {
        Ok(p) => (p, Some(path.display().to_string()), None),
        Err(e) => (
            Vec::new(),
            Some(path.display().to_string()),
            Some(format!("Não foi possível ler {}: {e}", path.display())),
        ),
    }
}

fn profile<'a>(profiles: &'a [Profile], name: &str) -> CmdResult<&'a Profile> {
    config::valid_profile(profiles, name).ok_or_else(|| {
        CmdError::new(
            "invalid",
            format!("O perfil \"{name}\" não existe no arquivo de configuração da AWS."),
        )
    })
}

fn check_region(r: &str) -> CmdResult<()> {
    if config::valid_region(r) {
        Ok(())
    } else {
        Err(CmdError::new("invalid", format!("Região inválida: {r}")))
    }
}

/// EC2 (`i-…`) and hybrid (`mi-…`) instance IDs.
pub fn valid_instance(id: &str) -> bool {
    let rest = id.strip_prefix("i-").or_else(|| id.strip_prefix("mi-"));
    rest.is_some_and(|r| (8..=17).contains(&r.len()) && r.chars().all(|c| c.is_ascii_hexdigit()))
}

fn check_instance(id: &str) -> CmdResult<()> {
    if valid_instance(id) {
        Ok(())
    } else {
        Err(CmdError::new(
            "invalid",
            format!("ID de instância inválido: {id}"),
        ))
    }
}

pub async fn settings(state: &AppState) -> CmdResult<AwsSettings> {
    Ok(state
        .vessel
        .lock()
        .await
        .device_sealed(SETTINGS_KEY)?
        .unwrap_or_default())
}

pub async fn cached_inventory(state: &AppState) -> CmdResult<Option<Inventory>> {
    Ok(state.vessel.lock().await.device_sealed(INVENTORY_KEY)?)
}

async fn save_inventory(state: &AppState, inv: &Inventory) {
    if let Err(e) = state
        .vessel
        .lock()
        .await
        .set_device_sealed(INVENTORY_KEY, inv)
    {
        tracing::warn!("could not save the AWS inventory: {e}");
    }
}

fn master_of(settings: &AwsSettings, profiles: &[Profile]) -> Option<String> {
    settings
        .master
        .clone()
        .filter(|m| m == ALL_PROFILES || profiles.iter().any(|p| &p.name == m))
        .or_else(|| config::default_master(profiles))
}

#[tauri::command]
pub async fn aws_overview(
    state: tauri::State<'_, AppState>,
    refresh_tools: Option<bool>,
) -> CmdResult<Overview> {
    let tools = state.aws.tools(refresh_tools.unwrap_or(false)).await;
    let (profiles, config_path, config_error) = load_profiles();
    let settings = settings(&state).await?;
    let inv = cached_inventory(&state).await.ok().flatten();
    Ok(Overview {
        master: master_of(&settings, &profiles),
        tools,
        config_path,
        config_error,
        profiles,
        mapped_at: inv.as_ref().map(|i| i.mapped_at),
        partial: inv.as_ref().is_some_and(|i| i.partial),
        instance_count: inv.as_ref().map(Inventory::instance_count).unwrap_or(0),
        settings,
        mapping: state.aws.is_mapping(),
        agents_allowed: state.settings().aws_agents,
    })
}

#[derive(Serialize)]
pub struct SessionStatus {
    pub active: bool,
    pub error: Option<AwsError>,
}

/// Whether the profile can get credentials now (they are dropped right away).
#[tauri::command]
pub async fn aws_session_status(
    state: tauri::State<'_, AppState>,
    profile: String,
) -> CmdResult<SessionStatus> {
    let (profiles, ..) = load_profiles();
    let p = self::profile(&profiles, &profile)?;
    let tools = state.aws.tools(false).await;
    Ok(
        match cli::export_credentials(&tools, &p.name, &home_region(&profiles, p)).await {
            Ok(_) => SessionStatus {
                active: true,
                error: None,
            },
            Err(e) => SessionStatus {
                active: false,
                error: Some(e),
            },
        },
    )
}

/// Opens a terminal tab with the CLI's sign-in for the master profile (`aws login`, or
/// `aws sso login` for SSO profiles), where the user follows it.
#[tauri::command]
pub async fn aws_login(
    state: tauri::State<'_, AppState>,
    profile: String,
    cols: u16,
    rows: u16,
) -> CmdResult<SessionInfo> {
    let (profiles, ..) = load_profiles();
    let p = self::profile(&profiles, &profile)?;
    let args: Vec<String> = match p.sign_in.as_deref() {
        Some("sso") => vec![
            "sso".into(),
            "login".into(),
            "--profile".into(),
            p.name.clone(),
        ],
        _ => vec!["login".into(), "--profile".into(), p.name.clone()],
    };
    let tools = state.aws.tools(false).await;
    open_cli_tab(
        &state,
        &tools,
        format!("Entrar na AWS ({})", p.name),
        SessionKind::Local {
            profile_id: None,
            shell: "AWS CLI".into(),
            shell_kind: Shell::Custom {
                program: "aws".into(),
            },
        },
        args,
        cols,
        rows,
        false,
    )
}

/// Runs the CLI in a terminal tab, without a shell around it.
#[allow(clippy::too_many_arguments)]
fn open_cli_tab(
    state: &AppState,
    tools: &Tools,
    title: String,
    kind: SessionKind,
    args: Vec<String>,
    cols: u16,
    rows: u16,
    by_agent: bool,
) -> CmdResult<SessionInfo> {
    let cli = tools
        .cli
        .clone()
        .ok_or_else(|| CmdError::new("aws_cli_missing", AwsError::cli_missing().message))?;
    let mut env = BTreeMap::new();
    env.insert("AWS_PAGER".to_string(), String::new());
    if let Some(path) = cli::child_path(tools) {
        env.insert("PATH".to_string(), path.to_string_lossy().into_owned());
    }
    let profile = TerminalProfile {
        label: title.clone(),
        shell: Shell::Custom { program: cli },
        args,
        env,
        ..Default::default()
    };
    let backend = state.terms.create(&state.app, title, kind, by_agent);
    let info = backend.session.info();
    if let Err(e) = crate::local::spawn(&profile, cols, rows, backend) {
        state.terms.close(&state.app, &info.id);
        return Err(CmdError::new("spawn_failed", e));
    }
    Ok(info)
}

/// The CLI as credential source, with each profile's region.
struct CliCreds(Tools, BTreeMap<String, String>);

impl inventory::CredentialSource for CliCreds {
    fn credentials(
        &self,
        profile: &str,
    ) -> impl std::future::Future<Output = Result<Credentials, AwsError>> + Send {
        let tools = self.0.clone();
        let region = self
            .1
            .get(profile)
            .cloned()
            .unwrap_or_else(|| "us-east-1".into());
        let profile = profile.to_string();
        async move { cli::export_credentials(&tools, &profile, &region).await }
    }
}

/// A profile's region: its own, else its master's, else us-east-1.
fn home_region(profiles: &[Profile], p: &Profile) -> String {
    p.region
        .clone()
        .filter(|r| config::valid_region(r))
        .or_else(|| {
            profiles
                .iter()
                .find(|x| x.name == p.master)
                .and_then(|m| m.region.clone())
        })
        .filter(|r| config::valid_region(r))
        .unwrap_or_else(|| "us-east-1".into())
}

struct Events(tauri::AppHandle);

impl inventory::Sink for Events {
    fn progress(&self, p: &Progress) {
        let _ = self.0.emit("aws://progress", p);
    }
    fn profile(&self, node: &ProfileNode) {
        let _ = self.0.emit("aws://profile", node);
    }
    fn region(&self, profile: &str, node: &RegionNode) {
        let _ = self.0.emit(
            "aws://region",
            serde_json::json!({ "profile": profile, "node": node }),
        );
    }
}

fn jobs_for(
    profiles: &[Profile],
    settings: &AwsSettings,
    names: &[String],
    only_region: Option<&str>,
) -> Vec<Job> {
    names
        .iter()
        .filter_map(|n| profiles.iter().find(|p| &p.name == n))
        .filter(|p| p.problem.is_none())
        .map(|p| Job {
            profile: p.name.clone(),
            account: p.account.clone(),
            role_arn: p.role_arn.clone(),
            home_region: home_region(profiles, p),
            regions: match only_region {
                Some(r) => Some(vec![r.to_string()]),
                None => settings
                    .regions
                    .get(&p.name)
                    .cloned()
                    .filter(|r| !r.is_empty()),
            },
        })
        .collect()
}

/// Starts a mapping in the background. `master` "*" maps every profile; a name maps it and the
/// profiles that depend on it. `region` remaps a single region of a single profile; `profile`
/// alone remaps one profile. Results merge into the cache.
#[tauri::command]
pub async fn aws_map(
    state: tauri::State<'_, AppState>,
    master: Option<String>,
    profile: Option<String>,
    region: Option<String>,
) -> CmdResult<()> {
    if !state.vessel.lock().await.status().unlocked {
        return Err(CmdError::new("locked", "O cofre está bloqueado."));
    }
    let tools = state.aws.tools(false).await;
    if tools.cli.is_none() {
        return Err(CmdError::new(
            "aws_cli_missing",
            AwsError::cli_missing().message,
        ));
    }
    let (profiles, ..) = load_profiles();
    let settings = settings(&state).await?;
    if let Some(r) = &region {
        check_region(r)?;
    }
    let (names, full) = match (&profile, &master) {
        (Some(p), _) => (vec![self::profile(&profiles, p)?.name.clone()], false),
        (None, Some(m)) if m == ALL_PROFILES => {
            (profiles.iter().map(|p| p.name.clone()).collect(), true)
        }
        (None, Some(m)) => {
            let m = self::profile(&profiles, m)?;
            (
                config::family(&profiles, &m.name)
                    .iter()
                    .map(|p| p.name.clone())
                    .collect(),
                true,
            )
        }
        (None, None) => return Err(CmdError::new("invalid", "Escolha o perfil mestre.")),
    };
    let jobs = jobs_for(&profiles, &settings, &names, region.as_deref());
    let regions: BTreeMap<String, String> = jobs
        .iter()
        .map(|j| (j.profile.clone(), j.home_region.clone()))
        .collect();
    if jobs.is_empty() {
        return Err(CmdError::new(
            "invalid",
            "Nenhum perfil válido para mapear.",
        ));
    }
    let cancel = {
        let mut m = state.aws.mapping.lock().unwrap_or_else(|e| e.into_inner());
        if m.is_some() {
            return Err(CmdError::new("busy", "Um mapeamento já está em andamento."));
        }
        let c = Cancel::default();
        *m = Some(c.clone());
        c
    };
    let state = (*state).clone();
    tokio::spawn(async move {
        let result = inventory::map(
            jobs,
            CliCreds(tools, regions),
            Events(state.app.clone()),
            Limits {
                // The same endpoint override the AWS CLI honors (a local emulator, a proxy, tests).
                endpoint: std::env::var("AWS_ENDPOINT_URL")
                    .ok()
                    .filter(|u| u.starts_with("http")),
                ..Limits::default()
            },
            cancel,
            state.aws.http(),
        )
        .await;
        let merged = if full && region.is_none() {
            result
        } else {
            let mut inv = cached_inventory(&state)
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            merge(&mut inv, result, region.as_deref());
            inv
        };
        save_inventory(&state, &merged).await;
        *state.aws.mapping.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let _ = state.app.emit(
            "aws://done",
            serde_json::json!({ "partial": merged.partial, "count": merged.instance_count() }),
        );
    });
    Ok(())
}

/// Puts a partial result (one profile, or one region of it) into the cached inventory.
fn merge(inv: &mut Inventory, part: Inventory, region: Option<&str>) {
    for node in part.profiles {
        match inv.profiles.iter_mut().find(|p| p.profile == node.profile) {
            Some(old) => match region {
                Some(r) if node.error.is_none() => {
                    let new_region = node.regions.into_iter().find(|x| x.region == r);
                    if let Some(new_region) = new_region {
                        match old.regions.iter_mut().find(|x| x.region == r) {
                            Some(slot) => *slot = new_region,
                            None => {
                                old.regions.push(new_region);
                                old.regions.sort_by(|a, b| a.region.cmp(&b.region));
                            }
                        }
                    }
                    old.error = None;
                }
                Some(_) => old.error = node.error,
                None => *old = node,
            },
            None => inv.profiles.push(node),
        }
    }
    inv.profiles.sort_by(|a, b| a.profile.cmp(&b.profile));
    if inv.mapped_at == 0 {
        inv.mapped_at = part.mapped_at;
    }
}

#[tauri::command]
pub fn aws_cancel(state: tauri::State<'_, AppState>) -> bool {
    match state
        .aws
        .mapping
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
    {
        Some(c) => {
            c.cancel();
            true
        }
        None => false,
    }
}

#[tauri::command]
pub async fn aws_inventory(state: tauri::State<'_, AppState>) -> CmdResult<Option<Inventory>> {
    cached_inventory(&state).await
}

#[tauri::command]
pub async fn aws_save_settings(
    state: tauri::State<'_, AppState>,
    settings: AwsSettings,
) -> CmdResult<AwsSettings> {
    let (profiles, ..) = load_profiles();
    if let Some(m) = &settings.master
        && m != ALL_PROFILES
    {
        profile(&profiles, m)?;
    }
    for (p, regions) in &settings.regions {
        profile(&profiles, p)?;
        for r in regions {
            check_region(r)?;
        }
    }
    for f in &settings.favorites {
        check_region(&f.region)?;
        check_instance(&f.id)?;
    }
    let mut clean = settings;
    clean.regions.retain(|_, r| !r.is_empty());
    clean.favorites.dedup();
    state
        .vessel
        .lock()
        .await
        .set_device_sealed(SETTINGS_KEY, &clean)?;
    Ok(clean)
}

#[tauri::command]
pub fn aws_set_agents(state: tauri::State<'_, AppState>, allowed: bool) -> CmdResult<bool> {
    state.update_settings(|s| s.aws_agents = allowed)?;
    Ok(allowed)
}

/// Validated session target, from the cached inventory: a session only goes to an instance
/// that the mapping found under that profile and region.
async fn target(
    state: &AppState,
    profile: &str,
    region: &str,
    instance_id: &str,
) -> CmdResult<(Profile, inventory::Instance)> {
    let (profiles, ..) = load_profiles();
    let p = self::profile(&profiles, profile)?.clone();
    check_region(region)?;
    check_instance(instance_id)?;
    let inv = cached_inventory(state).await?.unwrap_or_default();
    let inst = inv
        .profiles
        .iter()
        .filter(|n| n.profile == p.name)
        .flat_map(|n| &n.regions)
        .filter(|r| r.region == region)
        .flat_map(|r| &r.instances)
        .find(|i| i.id == instance_id)
        .cloned()
        .ok_or_else(|| {
            CmdError::new(
                "not_found",
                "Instância não encontrada no último mapeamento. Mapeie de novo.",
            )
        })?;
    if inst.ssm == inventory::SsmStatus::Unmanaged {
        return Err(CmdError::new(
            "aws_unmanaged",
            "Esta instância não é gerenciada pelo Session Manager.",
        ));
    }
    Ok((p, inst))
}

fn need_plugin(tools: &Tools) -> CmdResult<()> {
    if tools.cli.is_none() {
        return Err(CmdError::new(
            "aws_cli_missing",
            AwsError::cli_missing().message,
        ));
    }
    if tools.plugin.is_none() {
        return Err(CmdError::new(
            "aws_plugin_missing",
            "O Session Manager Plugin não foi encontrado neste computador.",
        ));
    }
    Ok(())
}

fn label(inst: &inventory::Instance) -> String {
    if inst.name.is_empty() {
        inst.id.clone()
    } else {
        inst.name.clone()
    }
}

/// A shell on the instance through Session Manager, in a terminal tab.
pub async fn open_session(
    state: &AppState,
    profile: &str,
    region: &str,
    instance_id: &str,
    cols: u16,
    rows: u16,
    by_agent: bool,
) -> CmdResult<SessionInfo> {
    let (p, inst) = target(state, profile, region, instance_id).await?;
    let tools = state.aws.tools(false).await;
    need_plugin(&tools)?;
    open_cli_tab(
        state,
        &tools,
        label(&inst),
        SessionKind::Aws {
            instance_id: inst.id.clone(),
            profile: p.name.clone(),
            region: region.into(),
            mode: "shell".into(),
        },
        vec![
            "ssm".into(),
            "start-session".into(),
            "--target".into(),
            inst.id.clone(),
            "--profile".into(),
            p.name.clone(),
            "--region".into(),
            region.into(),
        ],
        cols,
        rows,
        by_agent,
    )
}

#[tauri::command]
pub async fn aws_open_session(
    state: tauri::State<'_, AppState>,
    profile: String,
    region: String,
    instance_id: String,
    cols: u16,
    rows: u16,
) -> CmdResult<SessionInfo> {
    open_session(&state, &profile, &region, &instance_id, cols, rows, false).await
}

fn valid_host(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && !h.starts_with('-')
        && h.chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-_:".contains(c))
}

/// A Session Manager port forward in a terminal tab (closing the tab stops it): a port of the
/// instance, or of a host the instance reaches, on a port of this computer.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn aws_port_forward(
    state: tauri::State<'_, AppState>,
    profile: String,
    region: String,
    instance_id: String,
    remote_port: u16,
    local_port: u16,
    remote_host: Option<String>,
    cols: u16,
    rows: u16,
) -> CmdResult<SessionInfo> {
    let (p, inst) = target(&state, &profile, &region, &instance_id).await?;
    if remote_port == 0 || local_port == 0 {
        return Err(CmdError::new(
            "invalid",
            "Porta inválida: use um número de 1 a 65535.",
        ));
    }
    let remote_host = remote_host
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty());
    if let Some(h) = &remote_host
        && !valid_host(h)
    {
        return Err(CmdError::new(
            "invalid",
            format!("Host de destino inválido: {h}"),
        ));
    }
    let tools = state.aws.tools(false).await;
    need_plugin(&tools)?;
    let (document, params) = match &remote_host {
        Some(h) => (
            "AWS-StartPortForwardingSessionToRemoteHost",
            serde_json::json!({ "host": [h], "portNumber": [remote_port.to_string()], "localPortNumber": [local_port.to_string()] }),
        ),
        None => (
            "AWS-StartPortForwardingSession",
            serde_json::json!({ "portNumber": [remote_port.to_string()], "localPortNumber": [local_port.to_string()] }),
        ),
    };
    let dest = match &remote_host {
        Some(h) => format!("{h}:{remote_port}"),
        None => format!("{}:{remote_port}", label(&inst)),
    };
    open_cli_tab(
        &state,
        &tools,
        format!("localhost:{local_port} → {dest}"),
        SessionKind::Aws {
            instance_id: inst.id.clone(),
            profile: p.name.clone(),
            region: region.clone(),
            mode: "port_forward".into(),
        },
        vec![
            "ssm".into(),
            "start-session".into(),
            "--target".into(),
            inst.id.clone(),
            "--document-name".into(),
            document.into(),
            "--parameters".into(),
            params.to_string(),
            "--profile".into(),
            p.name.clone(),
            "--region".into(),
            region,
        ],
        cols,
        rows,
        false,
    )
}

/// The Session Manager SSH tunnel as a byte stream: the CLI's stdin and stdout. What it says on
/// stderr explains a failure.
struct Tunnel {
    out: tokio::process::ChildStdout,
    inp: tokio::process::ChildStdin,
    _child: tokio::process::Child,
}

impl tokio::io::AsyncRead for Tunnel {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.out).poll_read(cx, buf)
    }
}

impl tokio::io::AsyncWrite for Tunnel {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inp).poll_write(cx, buf)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inp).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inp).poll_shutdown(cx)
    }
}

/// Forgets the SSH host key saved for an instance (after a reinstall, the next connection asks
/// to confirm the new one).
#[tauri::command]
pub async fn aws_forget_host_key(
    state: tauri::State<'_, AppState>,
    instance_id: String,
) -> CmdResult<bool> {
    check_instance(&instance_id)?;
    let vessel = state.vessel.lock().await;
    let mut known: BTreeMap<String, String> =
        vessel.device_sealed(HOST_KEYS_KEY)?.unwrap_or_default();
    let removed = known.remove(&format!("{instance_id}:22")).is_some();
    vessel.set_device_sealed(HOST_KEYS_KEY, &known)?;
    Ok(removed)
}

fn ssh_tunnel(
    tools: &Tools,
    profile: &str,
    region: &str,
    instance_id: &str,
) -> CmdResult<(Tunnel, Arc<Mutex<String>>)> {
    use tokio::io::AsyncReadExt;
    let cli = tools
        .cli_path()
        .ok_or_else(|| CmdError::new("aws_cli_missing", AwsError::cli_missing().message))?;
    let mut cmd = tokio::process::Command::new(cli);
    cmd.args([
        "ssm",
        "start-session",
        "--target",
        instance_id,
        "--document-name",
        "AWS-StartSSHSession",
        "--parameters",
        "portNumber=22",
        "--profile",
        profile,
        "--region",
        region,
    ])
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .kill_on_drop(true)
    .env("AWS_PAGER", "");
    if let Some(p) = cli::child_path(tools) {
        cmd.env("PATH", p);
    }
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let mut child = cmd.spawn()?;
    let out = child.stdout.take().ok_or("sem saída do AWS CLI")?;
    let inp = child.stdin.take().ok_or("sem entrada do AWS CLI")?;
    let errors = Arc::new(Mutex::new(String::new()));
    if let Some(mut err) = child.stderr.take() {
        let errors = errors.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            while let Ok(n) = err.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                let mut e = errors.lock().unwrap_or_else(|e| e.into_inner());
                if e.len() < 16 * 1024 {
                    e.push_str(&String::from_utf8_lossy(&buf[..n]));
                }
            }
        });
    }
    Ok((
        Tunnel {
            out,
            inp,
            _child: child,
        },
        errors,
    ))
}

/// SSH to the instance through Session Manager, with a vault credential and the host key
/// checked against the one seen before (kept on this device, not synced).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn aws_ssh(
    state: tauri::State<'_, AppState>,
    profile: String,
    region: String,
    instance_id: String,
    username: String,
    identity_id: Option<Uuid>,
    ssh_key_id: Option<Uuid>,
    cols: u16,
    rows: u16,
    trust_new: bool,
) -> CmdResult<SessionInfo> {
    let (p, inst) = target(&state, &profile, &region, &instance_id).await?;
    let tools = state.aws.tools(false).await;
    need_plugin(&tools)?;
    let host = format!("{}:22", inst.id);
    let (hop, known) = {
        let vessel = state.vessel.lock().await;
        let identity = match identity_id {
            Some(id) => match vessel.find(id)?.map(|i| i.entity) {
                Some(Entity::Identity(i)) => Some(i),
                _ => return Err(CmdError::new("not_found", "Identidade não encontrada.")),
            },
            None => None,
        };
        let key_id = ssh_key_id.or_else(|| identity.as_ref().and_then(|i| i.ssh_key_id));
        let key = match key_id {
            Some(id) => match vessel.find(id)?.map(|i| i.entity) {
                Some(Entity::SshKey(k)) => Some((k.private_key, k.passphrase)),
                _ => return Err(CmdError::new("not_found", "Chave SSH não encontrada.")),
            },
            None => None,
        };
        let username = Some(username.trim().to_string())
            .filter(|u| !u.is_empty())
            .or_else(|| {
                identity
                    .as_ref()
                    .map(|i| i.username.clone())
                    .filter(|u| !u.is_empty())
            })
            .ok_or_else(|| CmdError::new("invalid", "Informe o usuário SSH."))?;
        let password = identity.as_ref().and_then(|i| i.password.clone());
        if key.is_none() && password.is_none() {
            return Err(CmdError::new(
                "invalid",
                "Escolha uma chave SSH ou uma identidade com senha.",
            ));
        }
        let known: BTreeMap<String, String> =
            vessel.device_sealed(HOST_KEYS_KEY)?.unwrap_or_default();
        let hop = crate::ssh::Hop {
            label: label(&inst),
            address: inst.id.clone(),
            port: 22,
            username,
            password,
            key,
            known_fingerprint: known.get(&host).cloned(),
            keepalive: Some(30),
            borrowed_credential: None,
        };
        (hop, known)
    };
    let plan = crate::ssh::Plan {
        hops: vec![hop],
        startup_script: None,
    };
    let (tunnel, errors) = ssh_tunnel(&tools, &p.name, &region, &inst.id)?;
    let conn = match crate::ssh::connect_via(&plan, trust_new, Some(Box::new(tunnel))).await {
        Ok(c) => c,
        Err(e) => {
            // The tunnel's own complaint says more than "connection closed".
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let said = errors.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if e.code.starts_with("host_key") || said.trim().is_empty() {
                return Err(e);
            }
            let a = cli::classify(&said, Some("ssm:StartSession"));
            return Err(CmdError::new("aws_session_failed", a.message));
        }
    };
    if !conn.new_host_keys.is_empty() {
        let mut known = known;
        for k in &conn.new_host_keys {
            known.insert(k.host.clone(), k.fingerprint.clone());
        }
        state
            .vessel
            .lock()
            .await
            .set_device_sealed(HOST_KEYS_KEY, &known)?;
    }
    let target_hop = &plan.hops[0];
    let backend = state.terms.create(
        &state.app,
        format!("{} (SSH)", target_hop.label),
        SessionKind::Aws {
            instance_id: inst.id.clone(),
            profile: p.name.clone(),
            region,
            mode: "ssh".into(),
        },
        false,
    );
    let info = backend.session.info();
    crate::ssh::run_shell(conn, backend, cols, rows, None);
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_ids_are_checked() {
        assert!(
            valid_instance("i-0123456789abcdef0")
                && valid_instance("i-12345678")
                && valid_instance("mi-0123456789abcdef0")
        );
        assert!(
            !valid_instance("i-xyz")
                && !valid_instance("i-1234567")
                && !valid_instance("--target")
                && !valid_instance("i-0123456789abcdef0; ls")
        );
        assert!(
            valid_host("db.internal")
                && valid_host("10.0.0.5")
                && !valid_host("-oProxy")
                && !valid_host("a b")
        );
    }

    fn region(r: &str, n: usize) -> RegionNode {
        RegionNode {
            region: r.into(),
            instances: (0..n)
                .map(|i| inventory::Instance {
                    id: format!("i-{i:08x}"),
                    name: String::new(),
                    state: "running".into(),
                    ssm: inventory::SsmStatus::Online,
                    platform: String::new(),
                    private_ip: String::new(),
                })
                .collect(),
            error: None,
        }
    }

    fn profile_node(name: &str, regions: Vec<RegionNode>) -> ProfileNode {
        ProfileNode {
            profile: name.into(),
            account: None,
            role_arn: None,
            regions,
            error: None,
        }
    }

    #[test]
    fn retries_merge_into_the_cache() {
        let mut inv = Inventory {
            mapped_at: 10,
            profiles: vec![profile_node(
                "a",
                vec![region("us-east-1", 1), region("sa-east-1", 2)],
            )],
            partial: false,
        };
        // One region of one profile again.
        merge(
            &mut inv,
            Inventory {
                mapped_at: 20,
                profiles: vec![profile_node("a", vec![region("sa-east-1", 5)])],
                partial: false,
            },
            Some("sa-east-1"),
        );
        assert_eq!(inv.instance_count(), 6);
        assert_eq!(
            inv.mapped_at, 10,
            "a partial remap keeps the date of the full one"
        );
        // A whole profile again, and a new one.
        merge(
            &mut inv,
            Inventory {
                mapped_at: 30,
                profiles: vec![
                    profile_node("a", vec![region("us-east-1", 3)]),
                    profile_node("b", vec![]),
                ],
                partial: false,
            },
            None,
        );
        assert_eq!(inv.instance_count(), 3);
        assert_eq!(inv.profiles.len(), 2);
    }
}
