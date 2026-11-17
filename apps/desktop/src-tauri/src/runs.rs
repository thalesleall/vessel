//! Running snippets on their own target (a local shell without a window, or SSH hosts in
//! parallel) and the history of those runs.
//!
//! The history stays on this device, sealed with the account key, and is never synced: output can
//! hold anything the commands printed.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::Emitter;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;
use vessel_client::model::{Entity, Shell, Snippet, SnippetTarget};

use crate::error::{CmdError, CmdResult};
use crate::ssh;
use crate::state::{AppState, now};

/// Output kept per run, split between its targets.
pub const OUTPUT_CAP: usize = 64 * 1024;
const HISTORY_KEY: &str = "snippet-runs";
const HISTORY_LEN: usize = 300;
const MAX_TIMEOUT_SECS: u32 = 6 * 60 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOrigin {
    Manual,
    Scheduled,
    Agent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Ok,
    Failed,
    /// A scheduled time that did not run (vault locked, previous run still going).
    Skipped,
    /// A scheduled time that passed while Vessel was closed or the computer was asleep.
    Missed,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TargetResult {
    pub target: String,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub error: Option<String>,
    pub timed_out: bool,
    pub truncated: bool,
    pub duration_ms: u64,
}

impl TargetResult {
    fn ok(&self) -> bool {
        self.error.is_none() && !self.timed_out && self.exit_code == Some(0)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: Uuid,
    pub snippet_id: Uuid,
    pub label: String,
    pub origin: RunOrigin,
    pub status: RunStatus,
    /// Where it ran ("PowerShell", "3 hosts"...).
    pub target: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub results: Vec<TargetResult>,
    /// Why it was skipped or missed.
    pub note: Option<String>,
}

#[derive(Default)]
pub struct Runs {
    running: Mutex<HashSet<Uuid>>,
    /// Records finished while the vault was locked, saved at the next chance.
    unsaved: Mutex<Vec<RunRecord>>,
}

impl Runs {
    pub fn running(&self) -> Vec<Uuid> {
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .copied()
            .collect()
    }

    pub fn is_running(&self, id: &Uuid) -> bool {
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(id)
    }
}

/// Removes a snippet from the running set however the run ends.
struct RunningGuard<'a>(&'a Runs, Uuid);

impl Drop for RunningGuard<'_> {
    fn drop(&mut self) {
        self.0
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.1);
    }
}

/// One resolved destination, built while the vault is unlocked.
enum Job {
    Local(Shell),
    Ssh {
        label: String,
        plan: Result<ssh::Plan, String>,
    },
}

pub fn target_label(target: &SnippetTarget, host_label: impl Fn(Uuid) -> String) -> String {
    match target {
        SnippetTarget::None => "Terminal".into(),
        SnippetTarget::Local { shell } => crate::ops::shell_label(shell),
        SnippetTarget::Hosts { host_ids } if host_ids.len() == 1 => host_label(host_ids[0]),
        SnippetTarget::Hosts { host_ids } => format!("{} hosts", host_ids.len()),
    }
}

/// Runs a snippet on its target and records the result.
///
/// Scheduled and agent runs only take snippets of the personal vault: another member of a shared
/// vault must not be able to get commands run on this computer by editing one. By hand, a snippet
/// from a shared vault, or one marked "confirm", first returns `confirm_snippet` with its script.
pub async fn run(
    state: &AppState,
    snippet_id: Uuid,
    origin: RunOrigin,
    confirmed: bool,
) -> CmdResult<RunRecord> {
    let (snippet, jobs, target) = {
        let vessel = state.vessel.lock().await;
        if !vessel.status().unlocked {
            return Err(CmdError::new("locked", "O cofre está bloqueado."));
        }
        let item = vessel
            .find(snippet_id)?
            .ok_or_else(|| CmdError::new("not_found", "Snippet não encontrado."))?;
        let Entity::Snippet(snippet) = item.entity else {
            return Err(CmdError::new("not_found", "Snippet não encontrado."));
        };
        let personal = vessel.personal_vault().ok() == Some(item.vault_id);
        check_allowed(&snippet, origin, personal, confirmed)?;
        let items = vessel.items(None)?;
        let host_label = |id: Uuid| {
            items
                .iter()
                .find_map(|i| match &i.entity {
                    Entity::Host(h) if i.id == id => Some(if h.label.is_empty() {
                        h.address.clone()
                    } else {
                        h.label.clone()
                    }),
                    _ => None,
                })
                .unwrap_or_else(|| "host removido".into())
        };
        let jobs: Vec<Job> = match &snippet.target {
            SnippetTarget::None => unreachable!("checked above"),
            SnippetTarget::Local { shell } => vec![Job::Local(shell.clone())],
            SnippetTarget::Hosts { host_ids } => host_ids
                .iter()
                .map(|id| Job::Ssh {
                    label: host_label(*id),
                    plan: ssh::plan_from(&vessel, Some(item.vault_id), *id).map_err(|e| e.message),
                })
                .collect(),
        };
        let target = target_label(&snippet.target, host_label);
        (snippet, jobs, target)
    };

    {
        let mut running = state.runs.running.lock().unwrap_or_else(|e| e.into_inner());
        if !running.insert(snippet_id) {
            return Err(CmdError::new(
                "already_running",
                format!("\"{}\" ainda está rodando.", snippet.label),
            ));
        }
    }
    let _guard = RunningGuard(&state.runs, snippet_id);

    let mut record = RunRecord {
        id: Uuid::now_v7(),
        snippet_id,
        label: snippet.label.clone(),
        origin,
        status: RunStatus::Running,
        target,
        started_at: now(),
        finished_at: None,
        results: Vec::new(),
        note: None,
    };
    let _ = state.app.emit("snippets://run", &record);

    let timeout = Duration::from_secs(u64::from(snippet.timeout_secs.clamp(1, MAX_TIMEOUT_SECS)));
    let cap = (OUTPUT_CAP / jobs.len().max(1) / 2).max(4 * 1024);
    let tmp = state.data_dir.join("run-tmp");
    let script = snippet.script.clone();
    let futures = jobs.into_iter().map(|job| {
        let (script, tmp) = (script.clone(), tmp.clone());
        async move {
            match job {
                Job::Local(shell) => run_local(&shell, &script, timeout, cap, &tmp).await,
                Job::Ssh { label, plan } => run_ssh(label, plan, &script, timeout, cap).await,
            }
        }
    });
    record.results = futures_util::future::join_all(futures).await;
    record.finished_at = Some(now());
    record.status = if !record.results.is_empty() && record.results.iter().all(TargetResult::ok) {
        RunStatus::Ok
    } else {
        RunStatus::Failed
    };
    save(state, record.clone()).await;
    let _ = state.app.emit("snippets://run", &record);
    Ok(record)
}

fn check_allowed(
    snippet: &Snippet,
    origin: RunOrigin,
    personal: bool,
    confirmed: bool,
) -> CmdResult<()> {
    if snippet.target == SnippetTarget::None {
        return Err(CmdError::new(
            "no_target",
            "Este snippet não tem destino: use \"Colar no terminal\" ou escolha um destino ao editá-lo.",
        ));
    }
    if snippet.script.trim().is_empty() {
        return Err(CmdError::new("invalid", "O snippet está vazio."));
    }
    match origin {
        RunOrigin::Scheduled | RunOrigin::Agent if !personal => Err(CmdError::new(
            "not_personal",
            "Só snippets do seu cofre pessoal rodam por agendamento ou por agentes de IA.",
        )),
        RunOrigin::Agent if !snippet.agent_allowed => Err(CmdError::new(
            "agent_not_allowed",
            "This snippet is not allowed for AI agents. The user can allow it in Vessel (Snippets → edit → Agentes de IA).",
        )),
        RunOrigin::Agent if snippet.confirm => Err(CmdError::new(
            "agent_not_allowed",
            "This snippet asks for confirmation before running, so only the user can run it, in the app.",
        )),
        RunOrigin::Manual if !confirmed && (snippet.confirm || !personal) => Err(CmdError::new(
            "confirm_snippet",
            if personal {
                "Confirme antes de rodar este snippet."
            } else {
                "Este snippet vem de um cofre compartilhado. Confira o script antes de rodar."
            },
        )
        .with_details(
            json!({ "label": snippet.label, "script": snippet.script, "shared": !personal }),
        )),
        _ => Ok(()),
    }
}

pub(crate) async fn run_ssh(
    label: String,
    plan: Result<ssh::Plan, String>,
    script: &str,
    timeout: Duration,
    cap: usize,
) -> TargetResult {
    let started = Instant::now();
    let deadline = tokio::time::Instant::now() + timeout;
    let mut result = TargetResult {
        target: label,
        ..Default::default()
    };
    let outcome = async {
        let plan = plan.map_err(|m| CmdError::new("invalid", m))?;
        // Never accept a host key here: nobody may be watching (a schedule, an agent), and the
        // first connection is where the user confirms the fingerprint, in a terminal.
        let conn = tokio::time::timeout_at(deadline, ssh::connect(&plan, false))
            .await
            .map_err(|_| CmdError::new("timeout", "Tempo máximo de execução esgotado."))??;
        let out = ssh::exec(&conn, script, cap, deadline).await;
        conn.close().await;
        out
    }
    .await;
    match outcome {
        Ok(out) => {
            result.exit_code = out.exit_code;
            result.stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            result.stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            result.truncated = out.truncated;
            if out.exit_code.is_none() {
                result.error = Some("A conexão terminou sem código de saída.".into());
            }
        }
        Err(e) if e.code == "timeout" => {
            result.timed_out = true;
            result.error = Some(e.message);
        }
        Err(e) if e.code == "host_key_unknown" => {
            result.error = Some("A chave deste servidor ainda não é conhecida. Conecte-se a ele uma vez pelo terminal para confirmar a impressão digital.".into());
        }
        Err(e) => result.error = Some(e.message),
    }
    result.duration_ms = started.elapsed().as_millis() as u64;
    result
}

/// Program, arguments and how the script reaches it, for a shell on this computer.
struct LocalCommand {
    program: PathBuf,
    args: Vec<String>,
    /// Script written to a file passed in `args` (kept until the run ends).
    file: Option<PathBuf>,
    /// Script piped into stdin.
    stdin: Option<String>,
}

fn local_command(shell: &Shell, script: &str, tmp: &Path) -> Result<LocalCommand, String> {
    let file = |ext: &str, body: Vec<u8>| -> Result<PathBuf, String> {
        std::fs::create_dir_all(tmp).map_err(|e| e.to_string())?;
        let path = tmp.join(format!("{}.{ext}", Uuid::new_v4()));
        std::fs::write(&path, body).map_err(|e| e.to_string())?;
        Ok(path)
    };
    match shell {
        Shell::Pwsh | Shell::WindowsPowershell => {
            let (program, _) = crate::local::resolve(shell)?;
            // UTF-8 with BOM: Windows PowerShell 5.1 reads BOM-less files in the ANSI code page.
            let mut body = b"\xEF\xBB\xBF".to_vec();
            body.extend_from_slice(
                format!(
                    "[Console]::OutputEncoding = [Text.Encoding]::UTF8\r\n$OutputEncoding = [Text.Encoding]::UTF8\r\n{}\r\nif ($null -ne $LASTEXITCODE -and $LASTEXITCODE -ne 0) {{ exit $LASTEXITCODE }}\r\n",
                    script.replace("\r\n", "\n").replace('\n', "\r\n")
                )
                .as_bytes(),
            );
            let path = file("ps1", body)?;
            Ok(LocalCommand {
                program: program.into(),
                args: [
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                ]
                .iter()
                .map(|s| s.to_string())
                .chain([path.display().to_string()])
                .collect(),
                file: Some(path),
                stdin: None,
            })
        }
        Shell::Cmd => {
            let body = format!(
                "@echo off\r\nchcp 65001 >nul\r\n{}\r\n",
                script.replace("\r\n", "\n").replace('\n', "\r\n")
            );
            let path = file("cmd", body.into_bytes())?;
            // A batch file is started through cmd.exe by the standard library, quoted safely.
            Ok(LocalCommand {
                program: path.clone(),
                args: Vec::new(),
                file: Some(path),
                stdin: None,
            })
        }
        Shell::Wsl { distro } => {
            let (program, _) = crate::local::resolve(shell)?;
            Ok(LocalCommand {
                program: program.into(),
                args: vec![
                    "-d".into(),
                    distro.clone(),
                    "--cd".into(),
                    "~".into(),
                    "--exec".into(),
                    "/bin/sh".into(),
                    "-c".into(),
                    "if command -v bash >/dev/null 2>&1; then exec bash -s; else exec sh -s; fi"
                        .into(),
                ],
                file: None,
                stdin: Some(script.replace("\r\n", "\n")),
            })
        }
        Shell::GitBash => {
            let (program, _) = crate::local::resolve(shell)?;
            Ok(LocalCommand {
                program: program.into(),
                args: vec!["-l".into(), "-s".into()],
                file: None,
                stdin: Some(script.replace("\r\n", "\n")),
            })
        }
        Shell::Custom { .. } => {
            Err("Snippets rodam em PowerShell, Prompt de Comando, WSL ou Git Bash.".into())
        }
    }
}

async fn read_capped(mut r: impl AsyncRead + Unpin, cap: usize) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut truncated = false;
    let mut buf = [0u8; 8192];
    loop {
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = cap.saturating_sub(out.len());
                if n > room {
                    truncated = true;
                }
                // Keeps reading after the cap so the process never blocks on a full pipe.
                out.extend_from_slice(&buf[..n.min(room)]);
            }
        }
    }
    (out, truncated)
}

/// Ends a process and everything it started (a script's own children too).
fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(0x0800_0000)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(windows))]
    let _ = pid;
}

pub async fn run_local(
    shell: &Shell,
    script: &str,
    timeout: Duration,
    cap: usize,
    tmp: &Path,
) -> TargetResult {
    let started = Instant::now();
    let mut result = TargetResult {
        target: crate::ops::shell_label(shell),
        ..Default::default()
    };
    let cmd = match local_command(shell, script, tmp) {
        Ok(c) => c,
        Err(e) => {
            result.error = Some(e);
            return result;
        }
    };
    let mut command = tokio::process::Command::new(&cmd.program);
    command
        .args(&cmd.args)
        .stdin(if cmd.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(home) = dirs::home_dir() {
        command.current_dir(home);
    }
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    match command.spawn() {
        Err(e) => {
            result.error = Some(format!(
                "Não foi possível iniciar {}: {e}",
                cmd.program.display()
            ))
        }
        Ok(mut child) => {
            if let (Some(text), Some(mut stdin)) = (cmd.stdin, child.stdin.take()) {
                tokio::spawn(async move {
                    let _ = stdin.write_all(text.as_bytes()).await;
                    let _ = stdin.shutdown().await;
                });
            }
            let stdout = tokio::spawn(read_capped(child.stdout.take().expect("piped"), cap));
            let stderr = tokio::spawn(read_capped(child.stderr.take().expect("piped"), cap));
            match tokio::time::timeout(timeout, child.wait()).await {
                Ok(Ok(status)) => result.exit_code = status.code(),
                Ok(Err(e)) => result.error = Some(e.to_string()),
                Err(_) => {
                    if let Some(pid) = child.id() {
                        kill_tree(pid);
                    }
                    let _ = child.kill().await;
                    result.timed_out = true;
                    result.error = Some(format!(
                        "Tempo máximo de execução esgotado ({} s).",
                        timeout.as_secs()
                    ));
                }
            }
            // Children that kept the pipes open could hold the readers; give them a moment.
            let collect = |h: tokio::task::JoinHandle<(Vec<u8>, bool)>| async move {
                tokio::time::timeout(Duration::from_secs(3), h)
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .unwrap_or_default()
            };
            let (out, t1) = collect(stdout).await;
            let (err, t2) = collect(stderr).await;
            result.stdout = String::from_utf8_lossy(&out).into_owned();
            result.stderr = String::from_utf8_lossy(&err).into_owned();
            result.truncated = t1 || t2;
        }
    }
    if let Some(f) = cmd.file {
        let _ = std::fs::remove_file(f);
    }
    result.duration_ms = started.elapsed().as_millis() as u64;
    result
}

/// Script files left by a run that did not finish (the app closed mid-run).
pub fn clean_tmp(data_dir: &Path) {
    let _ = std::fs::remove_dir_all(data_dir.join("run-tmp"));
}

/// Adds a record to the history (or keeps it in memory until the vault is unlocked again).
pub async fn save(state: &AppState, record: RunRecord) {
    state
        .runs
        .unsaved
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(record);
    flush(state).await;
}

/// Writes records kept while the vault was locked.
pub async fn flush(state: &AppState) {
    let vessel = state.vessel.lock().await;
    if !vessel.status().unlocked {
        return;
    }
    let pending: Vec<RunRecord> =
        std::mem::take(&mut *state.runs.unsaved.lock().unwrap_or_else(|e| e.into_inner()));
    if pending.is_empty() {
        return;
    }
    let mut history: Vec<RunRecord> = vessel
        .device_sealed(HISTORY_KEY)
        .ok()
        .flatten()
        .unwrap_or_default();
    history.extend(pending);
    history.sort_by(|a, b| b.started_at.cmp(&a.started_at).then(b.id.cmp(&a.id)));
    history.dedup_by_key(|r| r.id);
    history.truncate(HISTORY_LEN);
    if let Err(e) = vessel.set_device_sealed(HISTORY_KEY, &history) {
        tracing::warn!("could not save the snippet history: {e}");
    }
}

pub async fn history(state: &AppState, snippet_id: Option<Uuid>) -> CmdResult<Vec<RunRecord>> {
    flush(state).await;
    let vessel = state.vessel.lock().await;
    let history: Vec<RunRecord> = vessel.device_sealed(HISTORY_KEY)?.unwrap_or_default();
    Ok(history
        .into_iter()
        .filter(|r| snippet_id.is_none_or(|id| r.snippet_id == id))
        .collect())
}

pub async fn clear_history(state: &AppState, snippet_id: Option<Uuid>) -> CmdResult<()> {
    let vessel = state.vessel.lock().await;
    let mut history: Vec<RunRecord> = vessel.device_sealed(HISTORY_KEY)?.unwrap_or_default();
    history.retain(|r| snippet_id.is_some_and(|id| r.snippet_id != id));
    vessel.set_device_sealed(HISTORY_KEY, &history)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snippet(target: SnippetTarget) -> Snippet {
        Snippet {
            label: "s".into(),
            script: "echo hi".into(),
            target,
            ..Default::default()
        }
    }

    #[test]
    fn who_may_run_what() {
        let local = snippet(SnippetTarget::Local { shell: Shell::Cmd });
        assert_eq!(
            check_allowed(&snippet(SnippetTarget::None), RunOrigin::Manual, true, true)
                .unwrap_err()
                .code,
            "no_target"
        );
        assert!(check_allowed(&local, RunOrigin::Manual, true, false).is_ok());
        assert!(check_allowed(&local, RunOrigin::Scheduled, true, false).is_ok());
        // Shared vaults: by hand only, after seeing the script.
        assert_eq!(
            check_allowed(&local, RunOrigin::Manual, false, false)
                .unwrap_err()
                .code,
            "confirm_snippet"
        );
        assert!(check_allowed(&local, RunOrigin::Manual, false, true).is_ok());
        assert_eq!(
            check_allowed(&local, RunOrigin::Scheduled, false, true)
                .unwrap_err()
                .code,
            "not_personal"
        );
        // Agents: only snippets the user allowed, and never one that asks first.
        assert_eq!(
            check_allowed(&local, RunOrigin::Agent, true, false)
                .unwrap_err()
                .code,
            "agent_not_allowed"
        );
        let allowed = Snippet {
            agent_allowed: true,
            ..local.clone()
        };
        assert!(check_allowed(&allowed, RunOrigin::Agent, true, false).is_ok());
        assert_eq!(
            check_allowed(&allowed, RunOrigin::Agent, false, false)
                .unwrap_err()
                .code,
            "not_personal"
        );
        let asks = Snippet {
            confirm: true,
            ..allowed
        };
        assert_eq!(
            check_allowed(&asks, RunOrigin::Agent, true, true)
                .unwrap_err()
                .code,
            "agent_not_allowed"
        );
        assert_eq!(
            check_allowed(&asks, RunOrigin::Manual, true, false)
                .unwrap_err()
                .code,
            "confirm_snippet"
        );
    }

    fn tmp() -> PathBuf {
        std::env::temp_dir().join(format!("vessel-run-test-{}", Uuid::new_v4()))
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn local_powershell_returns_output_and_exit_code() {
        let dir = tmp();
        let r = run_local(
            &Shell::WindowsPowershell,
            "Write-Output 'olá ✓'\ncmd /c exit 3",
            Duration::from_secs(60),
            1024,
            &dir,
        )
        .await;
        assert_eq!(r.exit_code, Some(3), "{r:?}");
        assert!(r.stdout.contains("olá ✓"), "{r:?}");
        assert!(!r.ok());
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            0,
            "the script file is removed"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn local_cmd_runs_multiline_batch() {
        let r = run_local(
            &Shell::Cmd,
            "echo um\r\necho dois 1>&2\r\nexit /b 0",
            Duration::from_secs(30),
            1024,
            &tmp(),
        )
        .await;
        assert!(r.ok(), "{r:?}");
        assert!(
            r.stdout.contains("um") && r.stderr.contains("dois"),
            "{r:?}"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn local_timeout_kills_the_script() {
        let started = Instant::now();
        let r = run_local(
            &Shell::Cmd,
            "ping -n 30 127.0.0.1 >nul",
            Duration::from_secs(2),
            1024,
            &tmp(),
        )
        .await;
        assert!(r.timed_out && !r.ok(), "{r:?}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "took {:?}",
            started.elapsed()
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn local_output_is_capped() {
        let r = run_local(
            &Shell::Cmd,
            "for /L %%i in (1,1,2000) do @echo linha %%i",
            Duration::from_secs(30),
            1000,
            &tmp(),
        )
        .await;
        assert!(r.ok(), "{r:?}");
        assert!(r.truncated && r.stdout.len() <= 1000);
    }

    #[test]
    fn custom_shells_are_refused() {
        let e = local_command(
            &Shell::Custom {
                program: "x.exe".into(),
            },
            "a",
            &tmp(),
        )
        .err()
        .unwrap();
        assert!(e.contains("PowerShell"));
    }
}
