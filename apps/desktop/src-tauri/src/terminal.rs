//! Terminal sessions (local PTY or SSH) behind one interface: input goes in through a channel,
//! output is fanned out to UI subscribers and kept in a ring buffer for re-attach and for agents.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, Emitter};
use tokio::sync::{Notify, mpsc};
use uuid::Uuid;

/// Bytes of output kept per session.
const RING_CAPACITY: usize = 512 * 1024;

pub enum Input {
    Data(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Close,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionKind {
    Local {
        profile_id: Option<Uuid>,
        shell: String,
    },
    Ssh {
        host_id: Uuid,
        address: String,
    },
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionInfo {
    pub id: Uuid,
    pub title: String,
    pub kind: SessionKind,
    pub alive: bool,
    pub created_at: i64,
    /// Opened by an AI agent through MCP.
    pub by_agent: bool,
}

pub struct Session {
    pub id: Uuid,
    pub title: String,
    pub kind: SessionKind,
    pub created_at: i64,
    pub by_agent: bool,
    input: mpsc::UnboundedSender<Input>,
    ring: Mutex<VecDeque<u8>>,
    /// Total bytes ever produced; lets readers ask for "output since offset N".
    produced: AtomicU64,
    subscribers: Mutex<Vec<Channel<InvokeResponseBody>>>,
    alive: AtomicBool,
    pub output: Notify,
}

impl Session {
    pub fn info(&self) -> SessionInfo {
        SessionInfo {
            id: self.id,
            title: self.title.clone(),
            kind: self.kind.clone(),
            alive: self.alive.load(Ordering::Relaxed),
            created_at: self.created_at,
            by_agent: self.by_agent,
        }
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    pub fn send(&self, input: Input) -> bool {
        self.is_alive() && self.input.send(input).is_ok()
    }

    pub fn write(&self, data: impl Into<Vec<u8>>) -> bool {
        self.send(Input::Data(data.into()))
    }

    /// Called by the backend with new output.
    pub fn push_output(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        {
            let mut ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
            ring.extend(bytes);
            let overflow = ring.len().saturating_sub(RING_CAPACITY);
            ring.drain(..overflow);
        }
        self.produced
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        let mut subs = self.subscribers.lock().unwrap_or_else(|e| e.into_inner());
        subs.retain(|c| c.send(InvokeResponseBody::Raw(bytes.to_vec())).is_ok());
        self.output.notify_waiters();
    }

    pub fn mark_exited(&self, message: &str) {
        if self.alive.swap(false, Ordering::Relaxed) {
            self.push_output(format!("\r\n\x1b[2m[{message}]\x1b[0m\r\n").as_bytes());
        }
    }

    /// Attaches a UI channel, replaying the buffered output first.
    pub fn attach(&self, channel: Channel<InvokeResponseBody>) {
        let backlog: Vec<u8> = self
            .ring
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .copied()
            .collect();
        if !backlog.is_empty() {
            let _ = channel.send(InvokeResponseBody::Raw(backlog));
        }
        self.subscribers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(channel);
    }

    pub fn produced(&self) -> u64 {
        self.produced.load(Ordering::Relaxed)
    }

    /// Output produced after absolute offset `since` (clamped to what is still buffered).
    pub fn output_since(&self, since: u64) -> Vec<u8> {
        let ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        let produced = self.produced.load(Ordering::Relaxed);
        let start_of_ring = produced.saturating_sub(ring.len() as u64);
        let skip = since.saturating_sub(start_of_ring) as usize;
        ring.iter().skip(skip).copied().collect()
    }
}

/// Plain text of terminal output: ANSI sequences removed, line endings normalized.
pub fn to_plain_text(bytes: &[u8]) -> String {
    let stripped = strip_ansi_escapes::strip(row_moves_to_newlines(bytes));
    String::from_utf8_lossy(&stripped)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
}

/// ConPTY often moves to the next line with a cursor-position sequence (`ESC[row;colH`)
/// instead of CR LF. For plain text, turn every move to a different row into a newline.
/// The output may start mid-session (row unknown): then a row move after visible text on
/// the current line also counts as a line break.
fn row_moves_to_newlines(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut row: Option<u32> = None;
    let mut line_has_text = false;
    let mut i = 0;
    while i < bytes.len() {
        // OSC (e.g. window title): copy through, it is not visible text.
        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b']') {
            let mut j = i + 2;
            while j < bytes.len()
                && bytes[j] != 0x07
                && !(bytes[j] == 0x1b && bytes.get(j + 1) == Some(&b'\\'))
            {
                j += 1;
            }
            let end = if bytes.get(j) == Some(&0x1b) {
                j + 2
            } else {
                j + 1
            };
            let end = end.min(bytes.len());
            out.extend_from_slice(&bytes[i..end]);
            i = end;
            continue;
        }
        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'[') {
            let mut j = i + 2;
            while j < bytes.len() && (0x30..=0x3f).contains(&bytes[j]) {
                j += 1;
            }
            if let Some(&fin) = bytes.get(j) {
                let params = std::str::from_utf8(&bytes[i + 2..j]).unwrap_or("");
                let first = params
                    .split(';')
                    .next()
                    .and_then(|p| p.parse::<u32>().ok())
                    .unwrap_or(1);
                let new_row = match fin {
                    b'H' | b'f' | b'd' => Some(first),
                    b'E' => Some(row.unwrap_or(1) + first.max(1)),
                    _ => None,
                };
                if let Some(r) = new_row {
                    let breaks = match row {
                        Some(old) => old != r,
                        None => line_has_text,
                    };
                    if breaks {
                        out.push(b'\n');
                        line_has_text = false;
                    }
                    row = Some(r);
                    i = j + 1;
                    continue;
                }
                out.extend_from_slice(&bytes[i..=j]);
                i = j + 1;
                continue;
            }
        }
        if bytes[i] == b'\n' {
            row = row.map(|r| r + 1);
            line_has_text = false;
        } else if bytes[i] >= 0x20 && bytes[i] != 0x7f {
            line_has_text = true;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod plain_text_tests {
    use super::to_plain_text;

    #[test]
    fn cursor_moves_to_another_row_become_newlines() {
        let conpty = b"\x1b[1;1Hciclo 1 do agente\x1b[2;1HC:\\Users\\ana>\x1b[2;17H";
        assert_eq!(
            to_plain_text(conpty),
            "ciclo 1 do agente\nC:\\Users\\ana>"
        );
        assert_eq!(to_plain_text(b"a\r\nb\x1b[32mc\x1b[0m"), "a\nbc");
    }

    #[test]
    fn real_conpty_output_starting_mid_session() {
        // Captured from cmd.exe under ConPTY after typing `echo ola do agente`.
        let raw =
            b"echo ola do agente\r\n\x1b[?25lola do agente\x1b[7;1HC:\\Users\\ana>\x1b[?25h";
        assert_eq!(
            to_plain_text(raw),
            "echo ola do agente\nola do agente\nC:\\Users\\ana>"
        );
        // Escape-only lines and window titles do not count as text.
        let start = b"\x1b[?25l\x1b]0;cmd.exe\x07\x1b[4;1HC:\\>";
        assert_eq!(to_plain_text(start), "C:\\>");
    }
}

/// The other half of a session, held by the backend that runs it.
pub struct SessionBackend {
    pub session: Arc<Session>,
    pub input: mpsc::UnboundedReceiver<Input>,
}

#[derive(Default)]
pub struct Terminals {
    sessions: Mutex<HashMap<Uuid, Arc<Session>>>,
}

impl Terminals {
    pub fn create(
        &self,
        app: &AppHandle,
        title: String,
        kind: SessionKind,
        by_agent: bool,
    ) -> SessionBackend {
        let (tx, rx) = mpsc::unbounded_channel();
        let session = Arc::new(Session {
            id: Uuid::now_v7(),
            title,
            kind,
            created_at: vessel_client_now(),
            by_agent,
            input: tx,
            ring: Mutex::new(VecDeque::with_capacity(16 * 1024)),
            produced: AtomicU64::new(0),
            subscribers: Mutex::new(Vec::new()),
            alive: AtomicBool::new(true),
            output: Notify::new(),
        });
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session.id, session.clone());
        let _ = app.emit("terminal://changed", ());
        SessionBackend { session, input: rx }
    }

    pub fn get(&self, id: &Uuid) -> Option<Arc<Session>> {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    pub fn list(&self) -> Vec<SessionInfo> {
        let mut out: Vec<SessionInfo> = self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|s| s.info())
            .collect();
        out.sort_by_key(|s| s.created_at);
        out
    }

    /// Closes the session and forgets it.
    pub fn close(&self, app: &AppHandle, id: &Uuid) -> bool {
        let removed = self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        if let Some(s) = &removed {
            let _ = s.input.send(Input::Close);
            s.alive.store(false, Ordering::Relaxed);
            let _ = app.emit("terminal://changed", ());
        }
        removed.is_some()
    }

    pub fn close_all(&self) {
        let sessions: Vec<_> = self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
            .collect();
        for (_, s) in sessions {
            let _ = s.input.send(Input::Close);
        }
    }
}

fn vessel_client_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
