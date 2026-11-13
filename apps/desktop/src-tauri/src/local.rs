//! Local shells: detection (PowerShell, CMD, WSL distros, Git Bash) and PTY sessions (ConPTY).

use std::io::{Read, Write};
use std::path::PathBuf;

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use serde::Serialize;
use vessel_client::model::{Shell, TerminalProfile};

use crate::terminal::{Input, SessionBackend};

#[derive(Clone, Debug, Serialize)]
pub struct DetectedShell {
    pub label: String,
    pub shell: Shell,
    pub program: String,
}

fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|p| p.is_file())
}

fn system32(file: &str) -> PathBuf {
    let root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    root.join("System32").join(file)
}

/// Lists WSL distributions (`wsl.exe -l -q`, which prints UTF-16LE).
pub fn wsl_distros() -> Vec<String> {
    let wsl = system32("wsl.exe");
    if !wsl.is_file() {
        return Vec::new();
    }
    let mut cmd = std::process::Command::new(wsl);
    cmd.args(["--list", "--quiet"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let Ok(out) = cmd.output() else {
        return Vec::new();
    };
    let text = if out.stdout.len() >= 2
        && out
            .stdout
            .iter()
            .skip(1)
            .step_by(2)
            .take(8)
            .all(|b| *b == 0)
    {
        let units: Vec<u16> = out
            .stdout
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    text.lines()
        .map(|l| {
            l.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}' || c == '\0')
                .to_string()
        })
        .filter(|l| !l.is_empty() && !l.starts_with("docker-desktop"))
        .collect()
}

fn git_bash() -> Option<PathBuf> {
    let candidates = [
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files (x86)\Git\bin\bash.exe",
    ];
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
        .or_else(|| {
            let git = which("git.exe")?;
            let bash = git.parent()?.parent()?.join("bin").join("bash.exe");
            bash.is_file().then_some(bash)
        })
}

fn pwsh() -> Option<PathBuf> {
    which("pwsh.exe").or_else(|| {
        let p = PathBuf::from(r"C:\Program Files\PowerShell\7\pwsh.exe");
        p.is_file().then_some(p)
    })
}

pub fn detect_shells() -> Vec<DetectedShell> {
    let mut out = Vec::new();
    if cfg!(windows) {
        if let Some(p) = pwsh() {
            out.push(DetectedShell {
                label: "PowerShell".into(),
                shell: Shell::Pwsh,
                program: p.display().to_string(),
            });
        }
        let ps = system32(r"WindowsPowerShell\v1.0\powershell.exe");
        if ps.is_file() {
            out.push(DetectedShell {
                label: "Windows PowerShell".into(),
                shell: Shell::WindowsPowershell,
                program: ps.display().to_string(),
            });
        }
        let cmd = system32("cmd.exe");
        if cmd.is_file() {
            out.push(DetectedShell {
                label: "Prompt de Comando".into(),
                shell: Shell::Cmd,
                program: cmd.display().to_string(),
            });
        }
        for distro in wsl_distros() {
            out.push(DetectedShell {
                label: distro.clone(),
                shell: Shell::Wsl { distro },
                program: system32("wsl.exe").display().to_string(),
            });
        }
        if let Some(bash) = git_bash() {
            out.push(DetectedShell {
                label: "Git Bash".into(),
                shell: Shell::GitBash,
                program: bash.display().to_string(),
            });
        }
    } else {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        out.push(DetectedShell {
            label: "Shell".into(),
            shell: Shell::Custom {
                program: shell.clone(),
            },
            program: shell,
        });
    }
    out
}

/// Resolves a synced shell description to a program and arguments on this machine.
pub fn resolve(shell: &Shell) -> Result<(String, Vec<String>), String> {
    let missing = |what: &str| Err(format!("{what} não está instalado neste computador"));
    match shell {
        Shell::Pwsh => match pwsh() {
            Some(p) => Ok((p.display().to_string(), vec!["-NoLogo".into()])),
            None => {
                let ps = system32(r"WindowsPowerShell\v1.0\powershell.exe");
                if ps.is_file() {
                    Ok((ps.display().to_string(), vec!["-NoLogo".into()]))
                } else {
                    missing("PowerShell")
                }
            }
        },
        Shell::WindowsPowershell => {
            let ps = system32(r"WindowsPowerShell\v1.0\powershell.exe");
            if ps.is_file() {
                Ok((ps.display().to_string(), vec!["-NoLogo".into()]))
            } else {
                missing("Windows PowerShell")
            }
        }
        Shell::Cmd => Ok((
            std::env::var("ComSpec").unwrap_or_else(|_| system32("cmd.exe").display().to_string()),
            vec![],
        )),
        Shell::Wsl { distro } => {
            if !wsl_distros().iter().any(|d| d == distro) {
                return Err(format!(
                    "a distribuição WSL \"{distro}\" não existe neste computador"
                ));
            }
            Ok((
                system32("wsl.exe").display().to_string(),
                vec!["-d".into(), distro.clone(), "--cd".into(), "~".into()],
            ))
        }
        Shell::GitBash => match git_bash() {
            Some(p) => Ok((p.display().to_string(), vec!["--login".into(), "-i".into()])),
            None => missing("Git Bash"),
        },
        Shell::Custom { program } => Ok((program.clone(), vec![])),
    }
}

/// Spawns a profile in a PTY and runs it until it exits or the session is closed.
pub fn spawn(
    profile: &TerminalProfile,
    cols: u16,
    rows: u16,
    backend: SessionBackend,
) -> Result<(), String> {
    let (program, mut args) = resolve(&profile.shell)?;
    args.extend(profile.args.iter().cloned());

    let pty = native_pty_system()
        .openpty(PtySize {
            rows: rows.max(2),
            cols: cols.max(10),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("não foi possível abrir o terminal: {e}"))?;
    let mut cmd = CommandBuilder::new(&program);
    cmd.args(&args);
    let cwd = if profile.cwd.trim().is_empty() {
        dirs::home_dir()
    } else {
        Some(PathBuf::from(profile.cwd.trim()))
    };
    if let Some(cwd) = cwd.filter(|c| c.is_dir()) {
        cmd.cwd(cwd);
    }
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM_PROGRAM", "Vessel");
    for (k, v) in &profile.env {
        cmd.env(k, v);
    }
    let mut child = pty
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("não foi possível iniciar {program}: {e}"))?;
    drop(pty.slave);

    let mut reader = pty.master.try_clone_reader().map_err(|e| e.to_string())?;
    let mut writer = pty.master.take_writer().map_err(|e| e.to_string())?;
    let master = pty.master;
    let SessionBackend { session, mut input } = backend;

    let out_session = session.clone();
    std::thread::Builder::new()
        .name("pty-reader".into())
        .spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => out_session.push_output(&buf[..n]),
                }
            }
        })
        .map_err(|e| e.to_string())?;

    let mut killer = child.clone_killer();
    std::thread::Builder::new()
        .name("pty-writer".into())
        .spawn(move || {
            while let Some(msg) = input.blocking_recv() {
                match msg {
                    Input::Data(bytes) => {
                        if writer
                            .write_all(&bytes)
                            .and_then(|_| writer.flush())
                            .is_err()
                        {
                            break;
                        }
                    }
                    Input::Resize { cols, rows } => {
                        let _ = master.resize(PtySize {
                            rows: rows.max(2),
                            cols: cols.max(10),
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                    }
                    Input::Close => break,
                }
            }
            let _ = killer.kill();
            // Dropping the master closes ConPTY and ends the reader thread.
            drop(master);
        })
        .map_err(|e| e.to_string())?;

    std::thread::Builder::new()
        .name("pty-wait".into())
        .spawn(move || {
            let message = match child.wait() {
                Ok(status) if status.success() => "processo encerrado".to_string(),
                Ok(status) => format!("processo encerrado com código {}", status.exit_code()),
                Err(e) => format!("processo encerrado: {e}"),
            };
            session.mark_exited(&message);
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}
