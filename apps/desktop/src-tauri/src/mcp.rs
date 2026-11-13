//! Native MCP server: lets AI agents (Claude Code, Claude Desktop, Cursor...) drive Vessel —
//! hosts, groups, snippets, profiles, workspaces, port forwards, terminals — but never read or
//! write credentials (passwords, private keys, passphrases, tokens, recovery key).
//!
//! Transport: MCP Streamable HTTP (JSON responses) on `127.0.0.1`, bearer-token protected, with
//! an Origin check against DNS rebinding. Clients that only speak stdio use the `vessel-mcp`
//! bridge binary. Agents only work while the app is unlocked, and every call is logged in the app.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Map, Value, json};
use tokio::task::JoinHandle;
use uuid::Uuid;
use vessel_client::model::{Entity, EntityKind, Shell};

use crate::error::CmdError;
use crate::ops::{self, LocalTarget};
use crate::state::AppState;
use crate::terminal::to_plain_text;

const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// Fields agents can never see or set.
const SECRET_FIELDS: &[&str] = &["password", "private_key", "passphrase"];

/// Entity kinds agents may create, edit and delete. Identities and SSH keys are credentials;
/// known hosts decide which servers are trusted. Agents can read their metadata and reference
/// them by id, but not change them.
const WRITABLE_KINDS: &[EntityKind] = &[
    EntityKind::Host,
    EntityKind::Group,
    EntityKind::Snippet,
    EntityKind::PortForward,
    EntityKind::TerminalProfile,
    EntityKind::Workspace,
    EntityKind::Settings,
];

#[derive(Default)]
pub struct McpControl {
    task: Mutex<Option<JoinHandle<()>>>,
    error: Mutex<Option<String>>,
}

impl McpControl {
    pub fn is_running(&self) -> bool {
        self.task
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|t| !t.is_finished())
    }

    pub fn last_error(&self) -> Option<String> {
        self.error.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn stop(&self) {
        if let Some(t) = self.task.lock().unwrap_or_else(|e| e.into_inner()).take() {
            t.abort();
        }
    }
}

pub fn new_token() -> String {
    let bytes = vessel_crypto::secret::random_bytes::<32>()
        .map(|b| b.to_vec())
        .unwrap_or_else(|_| [*Uuid::new_v4().as_bytes(), *Uuid::new_v4().as_bytes()].concat());
    format!("vsl_{}", URL_SAFE_NO_PAD.encode(bytes))
}

/// Starts the server if enabled and not running.
pub async fn apply(state: &AppState) {
    if state.settings().mcp_enabled && !state.mcp.is_running() {
        start(state).await;
    }
}

pub async fn restart(state: &AppState) {
    state.mcp.stop();
    tokio::time::sleep(Duration::from_millis(100)).await;
    apply(state).await;
}

async fn start(state: &AppState) {
    let settings = state.settings();
    if settings.mcp_token.is_empty() {
        let _ = state.update_settings(|s| s.mcp_token = new_token());
    }
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", settings.mcp_port)).await {
        Ok(l) => l,
        Err(e) => {
            *state.mcp.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(format!(
                "não foi possível usar a porta {}: {e}",
                settings.mcp_port
            ));
            return;
        }
    };
    *state.mcp.error.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let app = Router::new()
        .route(
            "/mcp",
            post(handle)
                .get(|| async { StatusCode::METHOD_NOT_ALLOWED })
                .delete(|| async { StatusCode::OK }),
        )
        .with_state(state.clone());
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    *state.mcp.task.lock().unwrap_or_else(|e| e.into_inner()) = Some(task);
    tracing::info!(
        port = settings.mcp_port,
        "MCP server listening on 127.0.0.1"
    );
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn handle(State(state): State<AppState>, headers: HeaderMap, body: String) -> Response {
    if let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) {
        let local = [
            "http://127.0.0.1",
            "http://localhost",
            "https://127.0.0.1",
            "https://localhost",
        ];
        if !local
            .iter()
            .any(|p| origin == *p || origin.starts_with(&format!("{p}:")))
        {
            return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
        }
    }
    let token = state.settings().mcp_token;
    let presented = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    if token.is_empty() || !constant_time_eq(presented.trim().as_bytes(), token.as_bytes()) {
        return (StatusCode::UNAUTHORIZED, "invalid MCP token").into_response();
    }

    let parsed: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return json_response(rpc_error(Value::Null, -32700, &format!("parse error: {e}")));
        }
    };
    let responses: Vec<Value> = match parsed {
        Value::Array(batch) => {
            let mut out = Vec::new();
            for msg in batch {
                if let Some(r) = dispatch(&state, msg).await {
                    out.push(r);
                }
            }
            out
        }
        msg => dispatch(&state, msg).await.into_iter().collect(),
    };
    match responses.len() {
        0 => StatusCode::ACCEPTED.into_response(),
        1 => json_response(responses.into_iter().next().unwrap_or(Value::Null)),
        _ => json_response(Value::Array(responses)),
    }
}

fn json_response(v: Value) -> Response {
    ([("content-type", "application/json")], v.to_string()).into_response()
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

async fn dispatch(state: &AppState, msg: Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let id = id?; // notifications get no response
    let result = match method.as_str() {
        "initialize" => {
            let requested = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(PROTOCOL_VERSIONS[0]);
            let version = PROTOCOL_VERSIONS
                .iter()
                .find(|v| **v == requested)
                .unwrap_or(&PROTOCOL_VERSIONS[0]);
            json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "vessel", "title": "Vessel", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tool_definitions() }),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let started = Instant::now();
            let outcome = call_tool(state, &name, &args).await;
            let summary = summarize(&args);
            tracing::info!(tool = %name, ok = outcome.is_ok(), ms = started.elapsed().as_millis() as u64, "MCP tool call");
            state.log_activity(&name, summary, outcome.as_ref().err().map(String::as_str));
            match outcome {
                Ok(value) => json!({
                    "content": [{ "type": "text", "text": serde_json::to_string_pretty(&value).unwrap_or_default() }],
                    "structuredContent": if value.is_object() { value.clone() } else { json!({ "result": value }) },
                    "isError": false,
                }),
                Err(message) => {
                    json!({ "content": [{ "type": "text", "text": message }], "isError": true })
                }
            }
        }
        "resources/list" => json!({ "resources": [] }),
        "prompts/list" => json!({ "prompts": [] }),
        _ => {
            return Some(rpc_error(
                id,
                -32601,
                &format!("method not found: {method}"),
            ));
        }
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

const INSTRUCTIONS: &str = "Vessel is the user's terminal and SSH manager. Use these tools to inspect and organize \
hosts, groups, snippets, terminal profiles, workspaces and port forwards, to open local terminals (PowerShell, CMD, \
WSL) or SSH sessions and run commands in them. Credentials (passwords, private keys, passphrases) are never exposed \
and cannot be set: reference existing identities and SSH keys by id (list_items kind=identity / ssh_key). Prefer \
terminal_run for commands whose output you need; it waits until the output settles. Ask the user before running \
destructive commands on remote servers.";

fn summarize(args: &Value) -> String {
    let pick = |k: &str| {
        args.get(k).and_then(|v| {
            v.as_str()
                .map(str::to_string)
                .or_else(|| (!v.is_null()).then(|| v.to_string()))
        })
    };
    let mut parts = Vec::new();
    for key in [
        "kind",
        "id",
        "host_id",
        "session_id",
        "profile_id",
        "shell",
        "rule_id",
        "command",
        "text",
    ] {
        if let Some(v) = pick(key) {
            let v: String = v.chars().take(120).collect();
            parts.push(format!("{key}={v}"));
        }
    }
    parts.join(" ")
}

/// A short Portuguese explanation of a tool error, for the activity list.
pub fn failure_reason(message: &str) -> String {
    let m = message.to_ascii_lowercase();
    let reason = if m.contains("is locked") {
        "Vessel bloqueado"
    } else if m.contains("editing items is disabled") {
        "Edição desligada para agentes"
    } else if m.contains("typing into terminals is disabled")
        || m.contains("with typing disabled")
        || m.contains("running commands is disabled")
    {
        "Digitação/execução desligada para agentes"
    } else if m.contains("is a credential") {
        "Credenciais são proibidas para agentes"
    } else if m.contains("agents cannot") || m.contains("can only be edited by the user") {
        "Só você pode alterar esse tipo de item"
    } else if m.contains("host key")
        || m.contains("impressão digital")
        || m.contains("chave do servidor")
    {
        "Chave do servidor precisa de confirmação"
    } else if m.contains("autenticação recusada") {
        "Autenticação SSH recusada"
    } else if m.contains("not found") || m.contains("no such") {
        "Não encontrado"
    } else {
        return message
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(140)
            .collect();
    };
    reason.to_string()
}

// -------------------------------------------------------------------------------------------------
// Tool definitions
// -------------------------------------------------------------------------------------------------

fn kinds_enum() -> Value {
    json!([
        "host",
        "group",
        "identity",
        "ssh_key",
        "known_host",
        "snippet",
        "port_forward",
        "terminal_profile",
        "workspace",
        "settings"
    ])
}

fn writable_kinds_enum() -> Value {
    json!([
        "host",
        "group",
        "snippet",
        "port_forward",
        "terminal_profile",
        "workspace",
        "settings"
    ])
}

fn tool(
    name: &str,
    title: &str,
    description: &str,
    properties: Value,
    required: &[&str],
    read_only: bool,
    destructive: bool,
) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required, "additionalProperties": false },
        "annotations": { "readOnlyHint": read_only, "destructiveHint": destructive, "openWorldHint": !read_only },
    })
}

fn tool_definitions() -> Vec<Value> {
    let id = json!({ "type": "string", "description": "Item id (UUID)." });
    let session = json!({ "type": "string", "description": "Terminal session id from open_terminal / connect_ssh / list_terminals." });
    vec![
        tool(
            "vessel_status",
            "Vessel status",
            "Whether Vessel is unlocked, the signed-in account, vaults and open terminals.",
            json!({}),
            &[],
            true,
            false,
        ),
        tool(
            "list_vaults",
            "List vaults",
            "Vaults (personal and shared) with your role in each.",
            json!({}),
            &[],
            true,
            false,
        ),
        tool(
            "list_vault_members",
            "List vault members",
            "Members of a shared vault and their roles.",
            json!({ "vault_id": id }),
            &["vault_id"],
            true,
            false,
        ),
        tool(
            "list_items",
            "List items",
            "List hosts, groups, identities, SSH keys, known hosts, snippets, port forwards, terminal profiles, workspaces or settings. Secrets are redacted (has_password etc.).",
            json!({
                "kind": { "type": "string", "enum": kinds_enum() },
                "vault_id": { "type": "string", "description": "Only items of this vault." },
                "query": { "type": "string", "description": "Case-insensitive text filter on label, address and tags." }
            }),
            &[],
            true,
            false,
        ),
        tool(
            "get_item",
            "Get item",
            "One item by id, secrets redacted.",
            json!({ "id": id }),
            &["id"],
            true,
            false,
        ),
        tool(
            "create_item",
            "Create item",
            "Create a host, group, snippet, port forward, terminal profile, workspace or settings record. `data` uses the same fields list_items returns. To give a host credentials, set identity_id or ssh_key_id to an existing identity/key; passwords and keys cannot be set by agents.",
            json!({
                "kind": { "type": "string", "enum": writable_kinds_enum() },
                "data": { "type": "object", "description": "Fields of the item, e.g. host: {label, address, port, username, group_id, identity_id, ssh_key_id, jump_host_id, tags, notes}." },
                "vault_id": { "type": "string", "description": "Target vault; defaults to the personal vault." }
            }),
            &["kind", "data"],
            false,
            false,
        ),
        tool(
            "update_item",
            "Update item",
            "Change fields of an item. Only the given fields change; a field set to null is cleared. Credentials are kept untouched.",
            json!({ "id": id, "data": { "type": "object" } }),
            &["id", "data"],
            false,
            false,
        ),
        tool(
            "delete_item",
            "Delete item",
            "Delete an item (synced to every device and vault member).",
            json!({ "id": id }),
            &["id"],
            false,
            true,
        ),
        tool(
            "move_item",
            "Move item",
            "Move an item to another vault.",
            json!({ "id": id, "to_vault_id": { "type": "string" } }),
            &["id", "to_vault_id"],
            false,
            false,
        ),
        tool(
            "sync_now",
            "Sync now",
            "Sync with the server immediately.",
            json!({}),
            &[],
            false,
            false,
        ),
        tool(
            "list_conflicts",
            "List sync conflicts",
            "Field values that lost a concurrent edit.",
            json!({}),
            &[],
            true,
            false,
        ),
        tool(
            "list_shells",
            "List local shells",
            "Shells installed on this computer (PowerShell, CMD, WSL distros, Git Bash).",
            json!({}),
            &[],
            true,
            false,
        ),
        tool(
            "list_terminals",
            "List terminals",
            "Open terminal sessions.",
            json!({}),
            &[],
            true,
            false,
        ),
        tool(
            "open_terminal",
            "Open local terminal",
            "Open a local terminal from a terminal profile or a shell. It appears as a tab in the app.",
            json!({
                "profile_id": { "type": "string" },
                "shell": {
                    "description": "A shell name, or a `shell` object exactly as list_shells returns it.",
                    "oneOf": [
                        { "type": "string", "enum": ["pwsh", "windows_powershell", "cmd", "git_bash", "wsl"] },
                        { "type": "object" }
                    ]
                },
                "distro": { "type": "string", "description": "WSL distribution, with shell=wsl." }
            }),
            &[],
            false,
            false,
        ),
        tool(
            "connect_ssh",
            "Connect to host",
            "Open an SSH session to a saved host using its stored credentials. Unknown host keys are refused unless trust_new_host_key is true; changed host keys are always refused.",
            json!({ "host_id": id, "trust_new_host_key": { "type": "boolean", "default": false } }),
            &["host_id"],
            false,
            false,
        ),
        tool(
            "terminal_run",
            "Run command",
            "Type a command into a terminal, press Enter and return the output once it stops changing (or on timeout).",
            json!({
                "session_id": session,
                "command": { "type": "string" },
                "timeout_ms": { "type": "integer", "default": 20000, "minimum": 500, "maximum": 600000 },
                "idle_ms": { "type": "integer", "default": 1200, "description": "Output is considered done after this long without changes." }
            }),
            &["session_id", "command"],
            false,
            true,
        ),
        tool(
            "terminal_write",
            "Type into terminal",
            "Send raw text to a terminal (e.g. answers to prompts, Ctrl+C as \\u0003). Set press_enter to add Enter.",
            json!({ "session_id": session, "text": { "type": "string" }, "press_enter": { "type": "boolean", "default": false } }),
            &["session_id", "text"],
            false,
            true,
        ),
        tool(
            "terminal_read",
            "Read terminal",
            "Recent output of a terminal as plain text.",
            json!({ "session_id": session, "max_chars": { "type": "integer", "default": 8000, "maximum": 100000 } }),
            &["session_id"],
            true,
            false,
        ),
        tool(
            "terminal_close",
            "Close terminal",
            "Close a terminal session.",
            json!({ "session_id": session }),
            &["session_id"],
            false,
            true,
        ),
        tool(
            "run_snippet",
            "Run snippet",
            "Run a saved snippet in a terminal and return the output.",
            json!({ "snippet_id": id, "session_id": session, "timeout_ms": { "type": "integer", "default": 20000 } }),
            &["snippet_id", "session_id"],
            false,
            true,
        ),
        tool(
            "list_port_forwards",
            "List port forwards",
            "Saved port forwarding rules and which are running.",
            json!({}),
            &[],
            true,
            false,
        ),
        tool(
            "start_port_forward",
            "Start port forward",
            "Start a saved port forwarding rule.",
            json!({ "rule_id": id, "trust_new_host_key": { "type": "boolean", "default": false } }),
            &["rule_id"],
            false,
            false,
        ),
        tool(
            "stop_port_forward",
            "Stop port forward",
            "Stop a running port forwarding rule.",
            json!({ "rule_id": id }),
            &["rule_id"],
            false,
            false,
        ),
    ]
}

// -------------------------------------------------------------------------------------------------
// Redaction and sanitization
// -------------------------------------------------------------------------------------------------

fn kind_name(kind: EntityKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn parse_kind(s: &str) -> Result<EntityKind, String> {
    serde_json::from_value(Value::String(s.into())).map_err(|_| format!("unknown kind: {s}"))
}

/// Entity data with every credential removed, replaced by `has_*` flags.
pub fn redact(entity: &Entity) -> Value {
    let mut data = serde_json::to_value(entity)
        .ok()
        .and_then(|mut v| v.get_mut("data").map(Value::take))
        .unwrap_or(Value::Null);
    redact_value(&mut data);
    data
}

fn is_secret_key(key: &str) -> bool {
    SECRET_FIELDS.iter().any(|f| f.eq_ignore_ascii_case(key))
}

/// Replaces credential-named fields at any depth with `has_*` flags.
fn redact_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            let secret_keys: Vec<String> =
                map.keys().filter(|k| is_secret_key(k)).cloned().collect();
            for key in secret_keys {
                let present = match map.remove(&key) {
                    Some(Value::Null) | None => false,
                    Some(Value::String(s)) => !s.is_empty(),
                    Some(_) => true,
                };
                map.insert(format!("has_{key}"), Value::Bool(present));
            }
            map.values_mut().for_each(redact_value);
        }
        Value::Array(items) => items.iter_mut().for_each(redact_value),
        _ => {}
    }
}

/// The first credential-named field at any depth.
fn find_secret(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => map.iter().find_map(|(k, v)| {
            if is_secret_key(k) {
                Some(k.clone())
            } else {
                find_secret(v)
            }
        }),
        Value::Array(items) => items.iter().find_map(find_secret),
        _ => None,
    }
}

fn item_json(item: &vessel_client::Item) -> Value {
    json!({
        "id": item.id,
        "vault_id": item.vault_id,
        "kind": kind_name(item.entity.kind()),
        "label": item.entity.label(),
        "pending_sync": item.pending,
        "data": redact(&item.entity),
    })
}

fn reject_secrets(data: &Map<String, Value>) -> Result<(), String> {
    // Checked at any depth so nothing credential-shaped is stored where the redaction or a
    // future field could expose it.
    match data.iter().find_map(|(k, v)| {
        if is_secret_key(k) {
            Some(k.clone())
        } else {
            find_secret(v)
        }
    }) {
        Some(field) => Err(format!(
            "\"{field}\" is a credential: agents cannot read or set credentials in Vessel. Ask the user to set it in the app, or reference an existing identity/SSH key by id."
        )),
        None => Ok(()),
    }
}

fn strip_derived(data: &mut Map<String, Value>) {
    for field in SECRET_FIELDS {
        data.remove(&format!("has_{field}"));
    }
}

// -------------------------------------------------------------------------------------------------
// Tool implementations
// -------------------------------------------------------------------------------------------------

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn arg_uuid(args: &Value, key: &str) -> Result<Uuid, String> {
    arg_str(args, key)
        .ok_or_else(|| format!("missing {key}"))?
        .parse()
        .map_err(|_| format!("{key} is not a valid id"))
}

fn arg_u64(args: &Value, key: &str, default: u64) -> u64 {
    args.get(key).and_then(Value::as_u64).unwrap_or(default)
}

fn err(e: impl Into<CmdError>) -> String {
    e.into().message
}

async fn require_unlocked(state: &AppState) -> Result<(), String> {
    if state.vessel.lock().await.status().unlocked {
        Ok(())
    } else {
        Err("Vessel is locked. Ask the user to unlock the Vessel app.".into())
    }
}

/// With terminal input disabled an agent may still open a terminal to read it, but nothing
/// that runs a command by itself: no custom programs and no profiles with arguments or
/// environment (e.g. `powershell -Command ...`).
async fn check_plain_shell(state: &AppState, target: &LocalTarget) -> Result<(), String> {
    const DENIED: &str = "With typing disabled for agents (Vessel → Agentes de IA), only plain built-in shells can be opened: no custom programs, arguments or environment variables.";
    let profile;
    let (shell, plain) = match target {
        LocalTarget::Shell(s) => (s, true),
        LocalTarget::Profile(id) => {
            let item = state.vessel.lock().await.find(*id).map_err(err)?;
            let Some(Entity::TerminalProfile(p)) = item.map(|i| i.entity) else {
                return Err("terminal profile not found".into());
            };
            profile = p;
            (
                &profile.shell,
                profile.args.is_empty() && profile.env.is_empty(),
            )
        }
    };
    if matches!(shell, Shell::Custom { .. }) || !plain {
        return Err(DENIED.into());
    }
    Ok(())
}

fn require_input(state: &AppState) -> Result<(), String> {
    if state.settings().mcp_allow_terminal_input {
        Ok(())
    } else {
        Err(
            "Typing into terminals is disabled for agents in Vessel settings (Agentes de IA)."
                .into(),
        )
    }
}

fn require_writes(state: &AppState) -> Result<(), String> {
    if state.settings().mcp_allow_writes {
        Ok(())
    } else {
        Err("Editing items is disabled for agents in Vessel settings (Agentes de IA).".into())
    }
}

async fn call_tool(state: &AppState, name: &str, args: &Value) -> Result<Value, String> {
    if name != "vessel_status" {
        require_unlocked(state).await?;
    }
    match name {
        "vessel_status" => {
            let vessel = state.vessel.lock().await;
            let status = vessel.status();
            let vaults = if status.unlocked {
                vessel.vaults().map(|v| v.len()).unwrap_or(0)
            } else {
                0
            };
            Ok(json!({
                "unlocked": status.unlocked,
                "signed_in": status.signed_in,
                "email": status.email,
                "server": status.server_url,
                "vaults": vaults,
                "open_terminals": state.terms.list().len(),
                "agent_can_type_in_terminals": state.settings().mcp_allow_terminal_input,
                "agent_can_edit_items": state.settings().mcp_allow_writes,
            }))
        }
        "list_vaults" => {
            let vaults = state.vessel.lock().await.vaults().map_err(err)?;
            Ok(json!({ "vaults": vaults }))
        }
        "list_vault_members" => {
            let vault = arg_uuid(args, "vault_id")?;
            let members = state
                .vessel
                .lock()
                .await
                .members(vault)
                .await
                .map_err(err)?;
            Ok(json!({
                "members": members.members.iter().map(|m| json!({ "user_id": m.user_id, "email": m.email, "role": m.role, "is_me": m.is_me })).collect::<Vec<_>>(),
                "pending": members.pending.iter().map(|m| json!({ "user_id": m.user_id, "email": m.email, "role": m.role })).collect::<Vec<_>>(),
            }))
        }
        "list_items" => {
            let kind = arg_str(args, "kind").map(parse_kind).transpose()?;
            let vault = arg_str(args, "vault_id")
                .map(|v| v.parse::<Uuid>())
                .transpose()
                .map_err(|_| "invalid vault_id")?;
            let query = arg_str(args, "query").map(str::to_lowercase);
            let items = state.vessel.lock().await.items(kind).map_err(err)?;
            let out: Vec<Value> = items
                .iter()
                .filter(|i| i.entity.kind() != EntityKind::VaultMeta)
                .filter(|i| vault.is_none_or(|v| v == i.vault_id))
                .map(item_json)
                .filter(|v| {
                    query
                        .as_ref()
                        .is_none_or(|q| v.to_string().to_lowercase().contains(q))
                })
                .collect();
            Ok(json!({ "count": out.len(), "items": out }))
        }
        "get_item" => {
            let id = arg_uuid(args, "id")?;
            let item = state
                .vessel
                .lock()
                .await
                .find(id)
                .map_err(err)?
                .ok_or("item not found")?;
            Ok(item_json(&item))
        }
        "create_item" => {
            require_writes(state)?;
            let kind = parse_kind(arg_str(args, "kind").ok_or("missing kind")?)?;
            if !WRITABLE_KINDS.contains(&kind) {
                return Err(format!("agents cannot create {} items", kind_name(kind)));
            }
            let mut data = args
                .get("data")
                .and_then(Value::as_object)
                .cloned()
                .ok_or("data must be an object")?;
            reject_secrets(&data)?;
            strip_derived(&mut data);
            let entity: Entity =
                serde_json::from_value(json!({ "type": kind_name(kind), "data": data }))
                    .map_err(|e| format!("invalid data: {e}"))?;
            let mut vessel = state.vessel.lock().await;
            let vault = match arg_str(args, "vault_id") {
                Some(v) => v.parse().map_err(|_| "invalid vault_id")?,
                None => vessel.personal_vault().map_err(err)?,
            };
            let id = vessel.save(vault, None, entity).map_err(err)?;
            let item = vessel
                .item(vault, id)
                .map_err(err)?
                .ok_or("created item vanished")?;
            drop(vessel);
            state.changed();
            Ok(item_json(&item))
        }
        "update_item" => {
            require_writes(state)?;
            let id = arg_uuid(args, "id")?;
            let mut patch = args
                .get("data")
                .and_then(Value::as_object)
                .cloned()
                .ok_or("data must be an object")?;
            reject_secrets(&patch)?;
            strip_derived(&mut patch);
            let mut vessel = state.vessel.lock().await;
            let item = vessel.find(id).map_err(err)?.ok_or("item not found")?;
            let kind = item.entity.kind();
            if !WRITABLE_KINDS.contains(&kind) {
                return Err(format!(
                    "{} items hold credentials or trust decisions and can only be edited by the user",
                    kind_name(kind)
                ));
            }
            let mut value = serde_json::to_value(&item.entity).map_err(|e| e.to_string())?;
            let data = value
                .get_mut("data")
                .and_then(Value::as_object_mut)
                .ok_or("item has no data")?;
            let secrets: Vec<(String, Value)> = SECRET_FIELDS
                .iter()
                .filter_map(|f| data.get(*f).map(|v| (f.to_string(), v.clone())))
                .collect();
            for (k, v) in patch {
                if v.is_null() {
                    data.remove(&k);
                } else {
                    data.insert(k, v);
                }
            }
            for (k, v) in secrets {
                data.insert(k, v);
            }
            let entity: Entity =
                serde_json::from_value(value).map_err(|e| format!("invalid data: {e}"))?;
            vessel.save(item.vault_id, Some(id), entity).map_err(err)?;
            let updated = vessel
                .item(item.vault_id, id)
                .map_err(err)?
                .ok_or("item vanished")?;
            drop(vessel);
            state.changed();
            Ok(item_json(&updated))
        }
        "delete_item" => {
            require_writes(state)?;
            let id = arg_uuid(args, "id")?;
            let mut vessel = state.vessel.lock().await;
            let item = vessel.find(id).map_err(err)?.ok_or("item not found")?;
            if !WRITABLE_KINDS.contains(&item.entity.kind()) {
                return Err(format!(
                    "agents cannot delete {} items",
                    kind_name(item.entity.kind())
                ));
            }
            vessel.delete(item.vault_id, id).map_err(err)?;
            drop(vessel);
            state.changed();
            Ok(json!({ "deleted": id }))
        }
        "move_item" => {
            require_writes(state)?;
            let id = arg_uuid(args, "id")?;
            let to = arg_uuid(args, "to_vault_id")?;
            let mut vessel = state.vessel.lock().await;
            let item = vessel.find(id).map_err(err)?.ok_or("item not found")?;
            if !WRITABLE_KINDS.contains(&item.entity.kind()) {
                return Err(format!(
                    "agents cannot move {} items",
                    kind_name(item.entity.kind())
                ));
            }
            let new_id = vessel.move_item(item.vault_id, to, id).map_err(err)?;
            drop(vessel);
            state.changed();
            Ok(json!({ "id": new_id, "vault_id": to }))
        }
        "sync_now" => {
            let report = state.vessel.lock().await.sync().await.map_err(err)?;
            Ok(serde_json::to_value(report).unwrap_or_default())
        }
        "list_conflicts" => {
            let conflicts = state.vessel.lock().await.conflicts().map_err(err)?;
            // Conflicting values may include credentials: show field names only for those.
            let out: Vec<Value> = conflicts
                .iter()
                .map(|c| {
                    let fields: Vec<Value> = c
                        .conflicts
                        .iter()
                        .map(|f| {
                            if is_secret_key(&f.field) || f.field == "*" {
                                json!({ "field": f.field })
                            } else {
                                let mut kept = serde_json::to_value(&f.local).unwrap_or_default();
                                let mut discarded = serde_json::to_value(&f.remote).unwrap_or_default();
                                redact_value(&mut kept);
                                redact_value(&mut discarded);
                                json!({ "field": f.field, "kept": kept, "discarded": discarded })
                            }
                        })
                        .collect();
                    json!({ "record_id": c.record_id, "label": c.label, "at": c.at, "fields": fields })
                })
                .collect();
            Ok(json!({ "conflicts": out }))
        }
        "list_shells" => {
            let shells = tokio::task::spawn_blocking(crate::local::detect_shells)
                .await
                .map_err(|e| e.to_string())?;
            Ok(json!({ "shells": shells }))
        }
        "list_terminals" => Ok(json!({ "terminals": state.terms.list() })),
        "open_terminal" => {
            let target = if let Some(id) = arg_str(args, "profile_id") {
                LocalTarget::Profile(id.parse().map_err(|_| "invalid profile_id")?)
            } else if let Some(obj @ Value::Object(_)) = args.get("shell") {
                // The shape list_shells returns, e.g. {"kind":"wsl","distro":"Ubuntu"}.
                LocalTarget::Shell(
                    serde_json::from_value(obj.clone())
                        .map_err(|e| format!("invalid shell: {e}"))?,
                )
            } else {
                let shell_arg = match args.get("shell") {
                    None | Some(Value::Null) => "pwsh",
                    Some(Value::String(s)) => s.as_str(),
                    Some(_) => {
                        return Err(
                            "shell must be a string or a shell object from list_shells".into()
                        );
                    }
                };
                let shell = match shell_arg {
                    "pwsh" => Shell::Pwsh,
                    "windows_powershell" => Shell::WindowsPowershell,
                    "cmd" => Shell::Cmd,
                    "git_bash" => Shell::GitBash,
                    "wsl" => Shell::Wsl {
                        distro: match arg_str(args, "distro") {
                            Some(d) => d.to_string(),
                            None => crate::local::wsl_distros()
                                .into_iter()
                                .next()
                                .ok_or("no WSL distribution installed")?,
                        },
                    },
                    other => return Err(format!("unknown shell {other}")),
                };
                LocalTarget::Shell(shell)
            };
            if require_input(state).is_err() {
                check_plain_shell(state, &target).await?;
            }
            let info = ops::open_local(state, target, 120, 32, true)
                .await
                .map_err(err)?;
            // Wait for the first prompt so a following terminal_run is not typed into a shell
            // that is still starting.
            wait_for_quiet(
                state,
                info.id,
                0,
                Duration::from_millis(600),
                Duration::from_secs(15),
            )
            .await;
            Ok(serde_json::to_value(info).unwrap_or_default())
        }
        "connect_ssh" => {
            let host_id = arg_uuid(args, "host_id")?;
            let trust = args
                .get("trust_new_host_key")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if require_input(state).is_err() {
                let host = state.vessel.lock().await.find(host_id).map_err(err)?;
                if let Some(Entity::Host(h)) = host.map(|i| i.entity)
                    && h.startup_snippet_id.is_some()
                {
                    return Err("This host runs a snippet on connect, and running commands is disabled for agents in Vessel settings (Agentes de IA).".into());
                }
            }
            match ops::open_ssh(state, host_id, 120, 32, trust, true).await {
                Ok(info) => {
                    wait_for_quiet(
                        state,
                        info.id,
                        0,
                        Duration::from_millis(800),
                        Duration::from_secs(8),
                    )
                    .await;
                    let session = state.terms.get(&info.id).ok_or("session closed")?;
                    let banner = tail(&to_plain_text(&session.output_since(0)), 2000);
                    Ok(json!({ "session": info, "output": banner }))
                }
                Err(e) if e.code == "host_key_unknown" => Err(format!(
                    "{} Fingerprint: {}. Confirm it with the user, then call again with trust_new_host_key=true.",
                    e.message,
                    e.details
                        .as_ref()
                        .and_then(|d| d.get("fingerprint"))
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                )),
                Err(e) => Err(e.message),
            }
        }
        "terminal_run" => {
            require_input(state)?;
            let id = arg_uuid(args, "session_id")?;
            let command = arg_str(args, "command").ok_or("missing command")?;
            let session = state.terms.get(&id).ok_or("no such terminal session")?;
            let timeout =
                Duration::from_millis(arg_u64(args, "timeout_ms", 20_000).clamp(500, 600_000));
            let idle = Duration::from_millis(arg_u64(args, "idle_ms", 1200).clamp(200, 30_000));
            let start = session.produced();
            if !session.write(format!("{command}\r").into_bytes()) {
                return Err("the terminal session has ended".into());
            }
            let finished = wait_for_quiet(state, id, start, idle, timeout).await;
            let output = to_plain_text(&session.output_since(start));
            Ok(json!({
                "output": tail(&output, 20_000),
                "completed": finished,
                "session_alive": session.is_alive(),
            }))
        }
        "terminal_write" => {
            require_input(state)?;
            let id = arg_uuid(args, "session_id")?;
            let mut text = args
                .get("text")
                .and_then(Value::as_str)
                .ok_or("missing text")?
                .to_string();
            if args
                .get("press_enter")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                text.push('\r');
            }
            let session = state.terms.get(&id).ok_or("no such terminal session")?;
            if !session.write(text.into_bytes()) {
                return Err("the terminal session has ended".into());
            }
            Ok(json!({ "written": true }))
        }
        "terminal_read" => {
            let id = arg_uuid(args, "session_id")?;
            let session = state.terms.get(&id).ok_or("no such terminal session")?;
            let max = arg_u64(args, "max_chars", 8000).min(100_000) as usize;
            Ok(
                json!({ "output": tail(&to_plain_text(&session.output_since(0)), max), "session_alive": session.is_alive() }),
            )
        }
        "terminal_close" => {
            let id = arg_uuid(args, "session_id")?;
            Ok(json!({ "closed": state.terms.close(&state.app, &id) }))
        }
        "run_snippet" => {
            require_input(state)?;
            let snippet_id = arg_uuid(args, "snippet_id")?;
            let script = match state
                .vessel
                .lock()
                .await
                .find(snippet_id)
                .map_err(err)?
                .map(|i| i.entity)
            {
                Some(Entity::Snippet(s)) => s.script,
                _ => return Err("snippet not found".into()),
            };
            let mut run_args = args.clone();
            run_args["command"] = Value::String(script.trim_end().replace('\n', "\r"));
            Box::pin(call_tool(state, "terminal_run", &run_args)).await
        }
        "list_port_forwards" => {
            let rules = state
                .vessel
                .lock()
                .await
                .items(Some(EntityKind::PortForward))
                .map_err(err)?;
            let running = state.forwards.list();
            let out: Vec<Value> = rules
                .iter()
                .map(|i| {
                    let status = running.iter().find(|r| r.rule_id == i.id);
                    let mut v = item_json(i);
                    v["running"] = Value::Bool(status.is_some_and(|s| s.running));
                    v["error"] = status
                        .and_then(|s| s.error.clone())
                        .map_or(Value::Null, Value::String);
                    v
                })
                .collect();
            Ok(json!({ "rules": out }))
        }
        "start_port_forward" => {
            let id = arg_uuid(args, "rule_id")?;
            let trust = args
                .get("trust_new_host_key")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let status = ops::start_forward(state, id, trust).await.map_err(err)?;
            Ok(serde_json::to_value(status).unwrap_or_default())
        }
        "stop_port_forward" => {
            let id = arg_uuid(args, "rule_id")?;
            Ok(json!({ "stopped": state.forwards.stop(&id).await }))
        }
        other => Err(format!("unknown tool: {other}")),
    }
}

/// Waits until a session produced no output for `idle`, or `timeout` passed. Returns whether it
/// went quiet (true) rather than timing out.
/// Waits until the session has produced output beyond `since` and then stayed quiet for `idle`.
/// The idle clock only starts with the first new output, so a slow-starting shell (WSL cold
/// start) is not mistaken for a finished command.
async fn wait_for_quiet(
    state: &AppState,
    id: Uuid,
    since: u64,
    idle: Duration,
    timeout: Duration,
) -> bool {
    let Some(session) = state.terms.get(&id) else {
        return true;
    };
    let started = Instant::now();
    let mut last = since;
    let mut last_change = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let now = session.produced();
        if now != last {
            last = now;
            last_change = Instant::now();
        }
        if !session.is_alive() || (last != since && last_change.elapsed() >= idle) {
            return true;
        }
        if started.elapsed() >= timeout {
            return false;
        }
    }
}

fn tail(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    let skipped = count - max_chars;
    format!(
        "…[{skipped} caracteres omitidos]\n{}",
        text.chars().skip(skipped).collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use vessel_client::model::{Host, Identity, SshKey};

    #[test]
    fn credentials_are_redacted() {
        let host = Entity::Host(Host {
            address: "a".into(),
            password: Some("hunter2".into()),
            ..Default::default()
        });
        let v = redact(&host);
        assert!(!v.to_string().contains("hunter2"));
        assert_eq!(v["has_password"], true);

        let key = Entity::SshKey(SshKey {
            private_key: "-----BEGIN OPENSSH PRIVATE KEY-----".into(),
            passphrase: Some("pp".into()),
            public_key: "ssh-ed25519 AAAA".into(),
            ..Default::default()
        });
        let v = redact(&key);
        assert!(!v.to_string().contains("BEGIN") && !v.to_string().contains("\"pp\""));
        assert_eq!(v["public_key"], "ssh-ed25519 AAAA");

        let identity = Entity::Identity(Identity {
            username: "root".into(),
            password: None,
            ..Default::default()
        });
        assert_eq!(redact(&identity)["has_password"], false);
    }

    #[test]
    fn secret_fields_cannot_be_written() {
        let mut data = Map::new();
        data.insert("address".into(), json!("x"));
        assert!(reject_secrets(&data).is_ok());
        data.insert("password".into(), json!("p"));
        assert!(reject_secrets(&data).is_err());

        // Nested or differently-cased credential fields are refused too.
        let nested = json!({ "extra": { "Password": "p" } });
        assert!(reject_secrets(nested.as_object().unwrap()).is_err());
        let in_array = json!({ "list": [{ "passphrase": "p" }] });
        assert!(reject_secrets(in_array.as_object().unwrap()).is_err());
    }

    #[test]
    fn nested_credentials_are_redacted() {
        let mut v =
            json!({ "extra": { "password": "hunter2", "ok": 1 }, "l": [{ "private_key": "k" }] });
        redact_value(&mut v);
        let s = v.to_string();
        assert!(!s.contains("hunter2") && !s.contains("\"k\""));
        assert_eq!(v["extra"]["has_password"], true);
        assert_eq!(v["extra"]["ok"], 1);
    }

    #[test]
    fn tool_list_is_well_formed() {
        let tools = tool_definitions();
        assert!(tools.len() >= 20);
        for t in tools {
            assert!(t["name"].is_string() && t["inputSchema"]["type"] == "object");
        }
    }
}
