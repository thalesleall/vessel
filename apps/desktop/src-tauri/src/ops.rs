//! Operations shared by the UI commands and the MCP tools.

use uuid::Uuid;
use vessel_client::model::{Entity, EntityKind, Settings, Shell, TerminalProfile};

use crate::error::{CmdError, CmdResult};
use crate::forward::ForwardStatus;
use crate::local;
use crate::ssh;
use crate::state::AppState;
use crate::terminal::{SessionInfo, SessionKind};

/// First run on a new account: terminal profiles for every shell found on this PC, default settings.
pub async fn ensure_defaults(state: &AppState) -> CmdResult<()> {
    let mut vessel = state.vessel.lock().await;
    let vault = vessel.personal_vault()?;
    let items = vessel.items(None)?;
    let mut created = false;
    if !items
        .iter()
        .any(|i| i.entity.kind() == EntityKind::TerminalProfile)
    {
        for shell in local::detect_shells() {
            vessel.save(
                vault,
                None,
                Entity::TerminalProfile(TerminalProfile {
                    label: shell.label,
                    shell: shell.shell,
                    ..Default::default()
                }),
            )?;
            created = true;
        }
    }
    if !items
        .iter()
        .any(|i| i.entity.kind() == EntityKind::Settings)
    {
        vessel.save(vault, None, Entity::Settings(Settings::default()))?;
        created = true;
    }
    drop(vessel);
    if created {
        state.changed();
    }
    Ok(())
}

pub enum LocalTarget {
    Profile(Uuid),
    Shell(Shell),
    /// A profile already read and checked by the caller; it is spawned exactly as given.
    Resolved {
        profile_id: Uuid,
        profile: TerminalProfile,
    },
}

/// Whether starting this profile runs something other than a plain built-in shell.
pub fn runs_custom_command(p: &TerminalProfile) -> bool {
    matches!(p.shell, Shell::Custom { .. }) || !p.args.is_empty() || !p.env.is_empty()
}

/// Opens a local terminal. A profile written in a shared vault (by another member) that runs a
/// custom program, arguments or environment needs `confirmed`: otherwise the full command line
/// is returned for the user to approve (`confirm_profile`).
pub async fn open_local(
    state: &AppState,
    target: LocalTarget,
    cols: u16,
    rows: u16,
    by_agent: bool,
    confirmed: bool,
) -> CmdResult<SessionInfo> {
    let (profile_id, profile) = match target {
        LocalTarget::Resolved {
            profile_id,
            profile,
        } => (Some(profile_id), profile),
        LocalTarget::Profile(id) => {
            let vessel = state.vessel.lock().await;
            let item = vessel
                .find(id)?
                .ok_or_else(|| CmdError::new("not_found", "Perfil de terminal não encontrado."))?;
            let Entity::TerminalProfile(p) = item.entity else {
                return Err(CmdError::new(
                    "not_found",
                    "Perfil de terminal não encontrado.",
                ));
            };
            let personal = vessel.personal_vault().ok();
            if !confirmed && Some(item.vault_id) != personal && runs_custom_command(&p) {
                let program = match &p.shell {
                    Shell::Custom { program } => program.clone(),
                    other => shell_label(other),
                };
                return Err(CmdError::new(
                    "confirm_profile",
                    "Este perfil vem de um cofre compartilhado e executa um comando próprio.",
                )
                .with_details(serde_json::json!({
                    "label": p.label,
                    "program": program,
                    "args": p.args,
                    "env": p.env.keys().collect::<Vec<_>>(),
                    "cwd": p.cwd,
                })));
            }
            (Some(id), p)
        }
        LocalTarget::Shell(shell) => (
            None,
            TerminalProfile {
                label: shell_label(&shell),
                shell,
                ..Default::default()
            },
        ),
    };
    let title = if profile.label.is_empty() {
        shell_label(&profile.shell)
    } else {
        profile.label.clone()
    };
    let backend = state.terms.create(
        &state.app,
        title,
        SessionKind::Local {
            profile_id,
            shell: shell_label(&profile.shell),
            shell_kind: profile.shell.clone(),
        },
        by_agent,
    );
    let info = backend.session.info();
    let id = info.id;
    if let Err(e) = local::spawn(&profile, cols, rows, backend) {
        state.terms.close(&state.app, &id);
        return Err(CmdError::new("spawn_failed", e));
    }
    Ok(info)
}

pub fn shell_label(shell: &Shell) -> String {
    match shell {
        Shell::Pwsh => "PowerShell".into(),
        Shell::WindowsPowershell => "Windows PowerShell".into(),
        Shell::Cmd => "Prompt de Comando".into(),
        Shell::Wsl { distro } => distro.clone(),
        Shell::GitBash => "Git Bash".into(),
        Shell::Custom { program } => program.clone(),
    }
}

/// Opens an SSH terminal. `trust_new` accepts a first-seen host key (saved as a known host).
pub async fn open_ssh(
    state: &AppState,
    host_id: Uuid,
    cols: u16,
    rows: u16,
    trust_new: bool,
    by_agent: bool,
) -> CmdResult<SessionInfo> {
    let plan = ssh::plan(&*state.vessel.lock().await, host_id)?;
    open_ssh_plan(state, host_id, plan, cols, rows, trust_new, by_agent).await
}

/// Opens an SSH terminal from a plan the caller already built (and possibly checked).
pub async fn open_ssh_plan(
    state: &AppState,
    host_id: Uuid,
    plan: ssh::Plan,
    cols: u16,
    rows: u16,
    trust_new: bool,
    by_agent: bool,
) -> CmdResult<SessionInfo> {
    let conn = ssh::connect(&plan, trust_new).await?;
    if !conn.new_host_keys.is_empty() {
        ssh::remember_host_keys(&mut *state.vessel.lock().await, &conn.new_host_keys)?;
        state.changed();
    }
    let target = plan.hops.last().expect("plan has a target");
    let backend = state.terms.create(
        &state.app,
        target.label.clone(),
        SessionKind::Ssh {
            host_id,
            address: format!("{}@{}", target.username, target.host_port()),
        },
        by_agent,
    );
    let info = backend.session.info();
    ssh::run_shell(conn, backend, cols, rows, plan.startup_script.clone());
    Ok(info)
}

/// A local or SOCKS listener other machines on the network can use (an open, password-less
/// tunnel). Remote forwards listen on the server and follow its `GatewayPorts` setting.
pub fn forward_exposed(rule: &vessel_client::model::PortForward) -> bool {
    use vessel_client::model::ForwardKind;
    let bind = rule.bind_address.trim();
    let loopback = bind.is_empty()
        || bind.eq_ignore_ascii_case("localhost")
        || bind
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    !matches!(rule.kind, ForwardKind::Remote) && !loopback
}

/// `allow_exposed`: the user confirmed a rule that listens beyond this computer (the app asks
/// first); agents never get to open one.
pub async fn start_forward(
    state: &AppState,
    rule_id: Uuid,
    trust_new: bool,
    allow_exposed: bool,
) -> CmdResult<ForwardStatus> {
    let (rule, plan) = {
        let vessel = state.vessel.lock().await;
        let Some((rule_vault, Entity::PortForward(rule))) =
            vessel.find(rule_id)?.map(|i| (i.vault_id, i.entity))
        else {
            return Err(CmdError::new(
                "not_found",
                "Regra de redirecionamento não encontrada.",
            ));
        };
        if !allow_exposed && forward_exposed(&rule) {
            return Err(CmdError::new(
                "forward_exposed",
                format!(
                    "A regra escuta em {}:{}, aberta para a rede. Só o usuário pode iniciá-la, no app.",
                    rule.bind_address, rule.bind_port
                ),
            ));
        }
        let host_id = rule
            .host_id
            .ok_or_else(|| CmdError::new("invalid", "A regra não tem host."))?;
        let plan = ssh::plan_from(&vessel, Some(rule_vault), host_id)?;
        (rule, plan)
    };
    let conn = ssh::connect(&plan, trust_new).await?;
    if !conn.new_host_keys.is_empty() {
        ssh::remember_host_keys(&mut *state.vessel.lock().await, &conn.new_host_keys)?;
        state.changed();
    }
    state.forwards.start(rule_id, rule, conn).await
}

#[cfg(test)]
mod forward_tests {
    use super::forward_exposed;
    use vessel_client::model::{ForwardKind, PortForward};

    fn rule(kind: ForwardKind, bind: &str) -> PortForward {
        PortForward {
            kind,
            bind_address: bind.into(),
            bind_port: 1080,
            ..Default::default()
        }
    }

    #[test]
    fn only_loopback_listeners_are_private() {
        for bind in ["", "127.0.0.1", "127.0.0.2", "localhost", "LOCALHOST", "::1", "[::1]"] {
            assert!(!forward_exposed(&rule(ForwardKind::Dynamic, bind)), "{bind}");
        }
        for bind in ["0.0.0.0", "::", "192.168.1.5", "example.com"] {
            assert!(forward_exposed(&rule(ForwardKind::Dynamic, bind)), "{bind}");
            assert!(forward_exposed(&rule(ForwardKind::Local, bind)), "{bind}");
        }
        assert!(!forward_exposed(&rule(ForwardKind::Remote, "0.0.0.0")));
    }
}
