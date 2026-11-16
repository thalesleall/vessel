use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio::sync::Notify;
use vessel_client::Vessel;

use crate::forward::Forwards;
use crate::mcp::McpControl;
use crate::terminal::Terminals;

/// Device-local preferences (not synced).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalSettings {
    pub last_server_url: String,
    pub last_email: String,
    pub mcp_enabled: bool,
    pub mcp_port: u16,
    pub mcp_token: String,
    /// Let agents type into terminals and run commands (they can always read them). Off until
    /// the user turns it on.
    pub mcp_allow_terminal_input: bool,
    /// Let agents create, edit and delete items.
    pub mcp_allow_writes: bool,
}

impl Default for LocalSettings {
    fn default() -> Self {
        Self {
            last_server_url: String::new(),
            last_email: String::new(),
            mcp_enabled: false,
            mcp_port: 47011,
            mcp_token: String::new(),
            mcp_allow_terminal_input: false,
            mcp_allow_writes: false,
        }
    }
}

/// One action taken by an AI agent, shown in the app so the user can follow along.
#[derive(Clone, Debug, Serialize)]
pub struct Activity {
    pub at: i64,
    pub tool: String,
    pub summary: String,
    pub ok: bool,
    /// Why it failed, short and in Portuguese, for the activity list.
    pub reason: Option<String>,
    /// The full error message the agent received.
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct AppState {
    pub app: AppHandle,
    pub vessel: Arc<tokio::sync::Mutex<Vessel>>,
    pub terms: Arc<Terminals>,
    pub forwards: Arc<Forwards>,
    pub sync_now: Arc<Notify>,
    pub settings: Arc<Mutex<LocalSettings>>,
    pub data_dir: PathBuf,
    pub mcp: Arc<McpControl>,
    pub activity: Arc<Mutex<VecDeque<Activity>>>,
}

impl AppState {
    pub fn settings(&self) -> LocalSettings {
        self.settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn update_settings(
        &self,
        f: impl FnOnce(&mut LocalSettings),
    ) -> std::io::Result<LocalSettings> {
        let mut s = self.settings.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut s);
        let json = serde_json::to_vec_pretty(&*s).map_err(std::io::Error::other)?;
        std::fs::write(self.data_dir.join("settings.json"), json)?;
        Ok(s.clone())
    }

    /// Local data changed: refresh the UI and push soon.
    pub fn changed(&self) {
        let _ = self.app.emit("vessel://changed", ());
        self.sync_now.notify_one();
    }

    pub fn log_activity(&self, tool: &str, summary: String, error: Option<&str>) {
        let entry = Activity {
            at: now(),
            tool: tool.into(),
            summary,
            ok: error.is_none(),
            reason: error.map(crate::mcp::failure_reason),
            error: error.map(|e| e.chars().take(500).collect()),
        };
        {
            let mut a = self.activity.lock().unwrap_or_else(|e| e.into_inner());
            a.push_front(entry.clone());
            a.truncate(200);
        }
        let _ = self.app.emit("mcp://activity", entry);
    }
}

pub fn load_settings(dir: &std::path::Path) -> LocalSettings {
    let Ok(bytes) = std::fs::read(dir.join("settings.json")) else {
        return LocalSettings::default();
    };
    // Tolerate a UTF-8 BOM (Windows editors and PowerShell 5 add one).
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    serde_json::from_slice(bytes).unwrap_or_else(|e| {
        tracing::warn!("ignoring unreadable settings.json: {e}");
        LocalSettings::default()
    })
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn device_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "Computador".into())
}
