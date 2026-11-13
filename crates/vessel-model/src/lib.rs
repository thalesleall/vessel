//! Synced entities (`docs/ARCHITECTURE.md` §8) and the three-way merge used on sync conflicts.
//!
//! An entity is serialized to JSON (`{"type": ..., "data": {...}}`), then sealed into a record
//! envelope. Every struct uses `#[serde(default)]`, so older and newer clients can read each
//! other's records: unknown fields are kept in [`Extra`] and survive a round-trip.

mod merge;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use merge::{FieldConflict, MergeOutcome, merge};

/// Unknown fields written by newer clients. Preserved on edit.
pub type Extra = BTreeMap<String, serde_json::Value>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Entity {
    VaultMeta(VaultMeta),
    Host(Host),
    Group(Group),
    Identity(Identity),
    SshKey(SshKey),
    KnownHost(KnownHost),
    Snippet(Snippet),
    PortForward(PortForward),
    TerminalProfile(TerminalProfile),
    Workspace(Workspace),
    Settings(Settings),
    Trust(Trust),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    VaultMeta,
    Host,
    Group,
    Identity,
    SshKey,
    KnownHost,
    Snippet,
    PortForward,
    TerminalProfile,
    Workspace,
    Settings,
    Trust,
}

impl Entity {
    pub fn kind(&self) -> EntityKind {
        match self {
            Self::VaultMeta(_) => EntityKind::VaultMeta,
            Self::Host(_) => EntityKind::Host,
            Self::Group(_) => EntityKind::Group,
            Self::Identity(_) => EntityKind::Identity,
            Self::SshKey(_) => EntityKind::SshKey,
            Self::KnownHost(_) => EntityKind::KnownHost,
            Self::Snippet(_) => EntityKind::Snippet,
            Self::PortForward(_) => EntityKind::PortForward,
            Self::TerminalProfile(_) => EntityKind::TerminalProfile,
            Self::Workspace(_) => EntityKind::Workspace,
            Self::Settings(_) => EntityKind::Settings,
            Self::Trust(_) => EntityKind::Trust,
        }
    }

    /// Human-readable name for lists and search.
    pub fn label(&self) -> &str {
        match self {
            Self::VaultMeta(v) => &v.name,
            Self::Host(v) if v.label.is_empty() => &v.address,
            Self::Host(v) => &v.label,
            Self::Group(v) => &v.label,
            Self::Identity(v) => &v.label,
            Self::SshKey(v) => &v.label,
            Self::KnownHost(v) => &v.host,
            Self::Snippet(v) => &v.label,
            Self::PortForward(v) => &v.label,
            Self::TerminalProfile(v) => &v.label,
            Self::Workspace(v) => &v.label,
            Self::Settings(_) => "Settings",
            Self::Trust(_) => "Trust",
        }
    }

    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("entities always serialize")
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}

/// New record id: UUIDv7 (time-ordered, generated offline).
pub fn new_id() -> Uuid {
    Uuid::now_v7()
}

// -------------------------------------------------------------------------------------------------
// Entities
// -------------------------------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VaultMeta {
    pub name: String,
    pub description: String,
    pub color: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostProtocol {
    #[default]
    Ssh,
    Telnet,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Host {
    pub label: String,
    pub address: String,
    /// `None` uses the protocol default (22 for SSH).
    pub port: Option<u16>,
    pub protocol: HostProtocol,
    pub group_id: Option<Uuid>,
    /// Reusable credentials. Inline `username`/`password`/`ssh_key_id` override it.
    pub identity_id: Option<Uuid>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub ssh_key_id: Option<Uuid>,
    /// Connect through this host first (`ProxyJump`).
    pub jump_host_id: Option<Uuid>,
    pub tags: Vec<String>,
    pub os: String,
    pub color: String,
    pub notes: String,
    pub startup_snippet_id: Option<Uuid>,
    pub keepalive_secs: Option<u32>,
    pub agent_forwarding: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Host {
    pub fn effective_port(&self) -> u16 {
        self.port.unwrap_or(match self.protocol {
            HostProtocol::Ssh => 22,
            HostProtocol::Telnet => 23,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Group {
    pub label: String,
    pub parent_id: Option<Uuid>,
    /// Inherited by hosts of the group that do not set their own.
    pub identity_id: Option<Uuid>,
    pub color: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Identity {
    pub label: String,
    pub username: String,
    pub password: Option<String>,
    pub ssh_key_id: Option<Uuid>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SshKey {
    pub label: String,
    /// OpenSSH or PEM private key.
    pub private_key: String,
    pub public_key: String,
    pub passphrase: Option<String>,
    pub certificate: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KnownHost {
    /// `host:port`.
    pub host: String,
    pub key_type: String,
    /// `SHA256:` fingerprint of the server key.
    pub fingerprint: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snippet {
    pub label: String,
    pub script: String,
    pub tags: Vec<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForwardKind {
    /// `-L`: local port to a destination reached through the host.
    #[default]
    Local,
    /// `-R`: port on the host back to a destination reachable locally.
    Remote,
    /// `-D`: local SOCKS5 proxy through the host.
    Dynamic,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PortForward {
    pub label: String,
    pub host_id: Option<Uuid>,
    pub kind: ForwardKind,
    pub bind_address: String,
    pub bind_port: u16,
    pub dest_host: String,
    pub dest_port: u16,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Which local program a terminal profile starts. Synced; resolved per device.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Shell {
    /// PowerShell 7 (`pwsh.exe`).
    #[default]
    Pwsh,
    /// Windows PowerShell 5.1 (`powershell.exe`).
    WindowsPowershell,
    Cmd,
    Wsl {
        distro: String,
    },
    GitBash,
    Custom {
        program: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalProfile {
    pub label: String,
    pub shell: Shell,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    pub color: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TabTarget {
    Profile { profile_id: Uuid },
    Host { host_id: Uuid },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceTab {
    pub target: TabTarget,
    #[serde(default)]
    pub title: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Workspace {
    pub label: String,
    pub tabs: Vec<WorkspaceTab>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// What this account's devices verified about other people (their keys) and about vault
/// histories (how far each was verified), kept in the personal vault so a new device starts from
/// it instead of trusting the server's word. Only the app writes it; agents cannot.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Trust {
    pub users: BTreeMap<Uuid, TrustedUser>,
    pub anchors: BTreeMap<Uuid, TrustAnchor>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrustedUser {
    pub email: String,
    /// Public keys, hex.
    pub kem: String,
    pub sign: String,
}

/// A verified point in a vault's membership history.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrustAnchor {
    pub seq: u64,
    /// Hash chain head after event `seq - 1`, hex.
    pub head: String,
    pub creator: Option<Uuid>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: String,
    pub font_family: String,
    pub font_size: u16,
    pub line_height: f32,
    pub cursor_style: String,
    pub cursor_blink: bool,
    pub scrollback: u32,
    pub copy_on_select: bool,
    pub default_profile_id: Option<Uuid>,
    pub lock_after_minutes: u32,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "midnight".into(),
            font_family: "Cascadia Mono, Consolas, monospace".into(),
            font_size: 14,
            line_height: 1.2,
            cursor_style: "bar".into(),
            cursor_blink: true,
            scrollback: 10_000,
            copy_on_select: false,
            default_profile_id: None,
            lock_after_minutes: 15,
            extra: Extra::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_fields_survive_round_trip() {
        let json = br#"{"type":"host","data":{"address":"a","future_field":{"x":1}}}"#;
        let entity = Entity::from_json(json).unwrap();
        let Entity::Host(host) = &entity else {
            panic!()
        };
        assert_eq!(host.address, "a");
        assert_eq!(host.effective_port(), 22);
        let back: serde_json::Value = serde_json::from_slice(&entity.to_json()).unwrap();
        assert_eq!(back["data"]["future_field"]["x"], 1);
    }

    #[test]
    fn shell_variants_serialize_with_kind() {
        let p = Entity::TerminalProfile(TerminalProfile {
            label: "Ubuntu".into(),
            shell: Shell::Wsl {
                distro: "Ubuntu-24.04".into(),
            },
            ..Default::default()
        });
        let v: serde_json::Value = serde_json::from_slice(&p.to_json()).unwrap();
        assert_eq!(v["data"]["shell"]["kind"], "wsl");
        assert_eq!(Entity::from_json(&p.to_json()).unwrap(), p);
    }
}
