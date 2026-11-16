//! Tauri commands called by the UI.

use serde::Serialize;
use tauri::State;
use tauri::ipc::{Channel, InvokeResponseBody};
use uuid::Uuid;
use vessel_client::model::{Entity, EntityKind, Shell, VaultMeta};
use vessel_client::{
    ConflictView, DeviceInfo, InviteResponse, Item, MembersView, Role, ServerInfo, Status,
    SyncReport, VaultView,
};

use crate::background::run_sync;
use crate::error::{CmdError, CmdResult};
use crate::forward::ForwardStatus;
use crate::local::{self, DetectedShell};
use crate::mcp;
use crate::ops::{self, LocalTarget};
use crate::state::{Activity, AppState, LocalSettings, device_name};
use crate::terminal::SessionInfo;

type S<'a> = State<'a, AppState>;

// -- account ----------------------------------------------------------------------------------

#[tauri::command]
pub async fn status(state: S<'_>) -> CmdResult<Status> {
    Ok(state.vessel.lock().await.status())
}

#[tauri::command]
pub async fn local_settings(state: S<'_>) -> CmdResult<LocalSettings> {
    Ok(state.settings())
}

#[tauri::command]
pub async fn server_info(url: String) -> CmdResult<ServerInfo> {
    Ok(vessel_client::server_info(&url).await?)
}

async fn after_auth(state: &AppState, url: &str, email: &str) {
    let _ = state.update_settings(|s| {
        s.last_server_url = url.into();
        s.last_email = email.into();
    });
    if let Err(e) = ops::ensure_defaults(state).await {
        tracing::warn!("could not create default profiles: {e}");
    }
    mcp::apply(state).await;
}

#[tauri::command]
pub async fn signup(
    state: S<'_>,
    url: String,
    email: String,
    password: String,
    invite: Option<String>,
) -> CmdResult<String> {
    let recovery = state
        .vessel
        .lock()
        .await
        .signup(
            &url,
            &email,
            &password,
            &device_name(),
            invite.filter(|i| !i.trim().is_empty()),
        )
        .await?;
    after_auth(&state, &url, &email).await;
    Ok(recovery)
}

#[tauri::command]
pub async fn login(
    state: S<'_>,
    url: String,
    email: String,
    password: String,
) -> CmdResult<Status> {
    let mut vessel = state.vessel.lock().await;
    vessel
        .login(&url, &email, &password, &device_name())
        .await?;
    let status = vessel.status();
    drop(vessel);
    after_auth(&state, &url, &email).await;
    Ok(status)
}

#[tauri::command]
pub async fn recover(
    state: S<'_>,
    url: String,
    email: String,
    recovery_key: String,
    new_password: String,
) -> CmdResult<String> {
    let mut vessel = state.vessel.lock().await;
    // The used recovery key stops working: the new one is shown once.
    let new_recovery = vessel
        .recover(&url, &email, &recovery_key, &new_password, &device_name())
        .await?;
    drop(vessel);
    after_auth(&state, &url, &email).await;
    Ok(new_recovery)
}

// -- local mode and migrations ------------------------------------------------------------------

/// What a migration did, plus where the safety backup was written.
#[derive(Serialize)]
pub struct MigrationOutcome {
    #[serde(flatten)]
    pub report: vessel_client::MigrationReport,
    pub backup_path: String,
    pub status: Status,
}

/// Encrypted copy of the local database, taken before every migration.
fn backup(state: &AppState, vessel: &vessel_client::Vessel, reason: &str) -> CmdResult<String> {
    let stamp = backup_stamp();
    let path = state
        .data_dir
        .join("backups")
        .join(format!("vessel-{stamp}-{reason}.db"));
    vessel.backup(&path)?;
    Ok(path.display().to_string())
}

fn backup_stamp() -> String {
    // Seconds since the epoch: unique enough and sortable, without a date library.
    crate::state::now().to_string()
}

#[tauri::command]
pub async fn create_local(state: S<'_>, name: String, password: String) -> CmdResult<String> {
    let recovery = state.vessel.lock().await.create_local(&name, &password)?;
    if let Err(e) = ops::ensure_defaults(&state).await {
        tracing::warn!("could not create default profiles: {e}");
    }
    mcp::apply(&state).await;
    Ok(recovery)
}

#[tauri::command]
pub async fn recover_local(
    state: S<'_>,
    recovery_key: String,
    new_password: String,
) -> CmdResult<Status> {
    let mut vessel = state.vessel.lock().await;
    vessel.recover_local(&recovery_key, &new_password)?;
    let status = vessel.status();
    drop(vessel);
    mcp::apply(&state).await;
    Ok(status)
}

#[tauri::command]
pub async fn connect_new_account(
    state: S<'_>,
    url: String,
    email: String,
    password: String,
    invite: Option<String>,
) -> CmdResult<MigrationOutcome> {
    let mut vessel = state.vessel.lock().await;
    let backup_path = backup(&state, &vessel, "antes-de-conectar")?;
    let report = vessel
        .connect_new_account(
            &url,
            &email,
            &password,
            &device_name(),
            invite.filter(|i| !i.trim().is_empty()),
        )
        .await?;
    let status = vessel.status();
    drop(vessel);
    after_auth(&state, &url, &email).await;
    state.changed();
    Ok(MigrationOutcome {
        report,
        backup_path,
        status,
    })
}

#[tauri::command]
pub async fn connect_existing_account(
    state: S<'_>,
    url: String,
    email: String,
    password: String,
) -> CmdResult<MigrationOutcome> {
    let mut vessel = state.vessel.lock().await;
    let backup_path = backup(&state, &vessel, "antes-de-conectar")?;
    let report = vessel
        .connect_existing_account(&url, &email, &password, &device_name())
        .await?;
    let status = vessel.status();
    drop(vessel);
    after_auth(&state, &url, &email).await;
    state.changed();
    Ok(MigrationOutcome {
        report,
        backup_path,
        status,
    })
}

#[tauri::command]
pub async fn disconnect_to_local(state: S<'_>) -> CmdResult<MigrationOutcome> {
    state.terms.close_all();
    state.forwards.stop_all();
    let mut vessel = state.vessel.lock().await;
    let backup_path = backup(&state, &vessel, "antes-de-desconectar")?;
    let report = vessel.disconnect_to_local().await?;
    let status = vessel.status();
    drop(vessel);
    mcp::apply(&state).await;
    state.changed();
    Ok(MigrationOutcome {
        report,
        backup_path,
        status,
    })
}

#[tauri::command]
pub async fn unlock(state: S<'_>, password: String) -> CmdResult<Status> {
    let status = {
        let mut vessel = state.vessel.lock().await;
        vessel.unlock(&password)?;
        vessel.status()
    };
    state.sync_now.notify_one();
    mcp::apply(&state).await;
    Ok(status)
}

#[tauri::command]
pub async fn lock(state: S<'_>) -> CmdResult<Status> {
    state.terms.close_all();
    state.forwards.stop_all();
    let mut vessel = state.vessel.lock().await;
    vessel.lock();
    Ok(vessel.status())
}

#[tauri::command]
pub async fn logout(state: S<'_>) -> CmdResult<Status> {
    state.terms.close_all();
    state.forwards.stop_all();
    let mut vessel = state.vessel.lock().await;
    vessel.logout().await?;
    Ok(vessel.status())
}

#[tauri::command]
pub async fn change_password(state: S<'_>, current: String, new: String) -> CmdResult<()> {
    Ok(state
        .vessel
        .lock()
        .await
        .change_password(&current, &new)
        .await?)
}

#[tauri::command]
pub async fn sync_now(state: S<'_>) -> CmdResult<SyncReport> {
    let report = state.vessel.lock().await.sync().await?;
    let _ = tauri::Emitter::emit(&state.app, "vessel://synced", &report);
    let _ = tauri::Emitter::emit(&state.app, "vessel://changed", ());
    Ok(report)
}

/// Background sync triggered by the UI without waiting for the result.
#[tauri::command]
pub async fn sync_background(state: S<'_>) -> CmdResult<()> {
    run_sync(&state).await;
    Ok(())
}

#[tauri::command]
pub async fn devices(state: S<'_>) -> CmdResult<Vec<DeviceInfo>> {
    Ok(state.vessel.lock().await.devices().await?)
}

#[tauri::command]
pub async fn revoke_device(state: S<'_>, device_id: Uuid) -> CmdResult<()> {
    Ok(state.vessel.lock().await.revoke_device(device_id).await?)
}

/// Deletes the account on the server and everything on this computer.
#[tauri::command]
pub async fn delete_account(state: S<'_>, password: String) -> CmdResult<Status> {
    state.terms.close_all();
    state.forwards.stop_all();
    let mut vessel = state.vessel.lock().await;
    vessel.delete_account(&password).await?;
    Ok(vessel.status())
}

#[tauri::command]
pub async fn admin_users(state: S<'_>) -> CmdResult<Vec<vessel_client::AdminUser>> {
    Ok(state.vessel.lock().await.admin_users().await?)
}

#[tauri::command]
pub async fn admin_update_user(
    state: S<'_>,
    user_id: Uuid,
    update: vessel_client::AdminUserUpdate,
) -> CmdResult<()> {
    Ok(state
        .vessel
        .lock()
        .await
        .admin_update_user(user_id, &update)
        .await?)
}

#[tauri::command]
pub async fn admin_delete_user(state: S<'_>, user_id: Uuid) -> CmdResult<()> {
    Ok(state.vessel.lock().await.admin_delete_user(user_id).await?)
}

#[tauri::command]
pub async fn admin_stats(state: S<'_>) -> CmdResult<vessel_client::ServerStats> {
    Ok(state.vessel.lock().await.admin_stats().await?)
}

#[tauri::command]
pub async fn signup_invite(state: S<'_>, email: Option<String>) -> CmdResult<InviteResponse> {
    Ok(state
        .vessel
        .lock()
        .await
        .create_signup_invite(email.filter(|e| !e.trim().is_empty()))
        .await?)
}

// -- items ------------------------------------------------------------------------------------

#[tauri::command]
pub async fn items(state: S<'_>, kind: Option<EntityKind>) -> CmdResult<Vec<Item>> {
    Ok(state.vessel.lock().await.items(kind)?)
}

#[tauri::command]
pub async fn save_item(
    state: S<'_>,
    vault_id: Uuid,
    id: Option<Uuid>,
    entity: Entity,
) -> CmdResult<Uuid> {
    let id = state.vessel.lock().await.save(vault_id, id, entity)?;
    state.changed();
    Ok(id)
}

#[tauri::command]
pub async fn delete_item(state: S<'_>, vault_id: Uuid, id: Uuid) -> CmdResult<()> {
    state.vessel.lock().await.delete(vault_id, id)?;
    state.changed();
    Ok(())
}

#[tauri::command]
pub async fn move_item(state: S<'_>, from: Uuid, to: Uuid, id: Uuid) -> CmdResult<Uuid> {
    let id = state.vessel.lock().await.move_item(from, to, id)?;
    state.changed();
    Ok(id)
}

#[tauri::command]
pub async fn conflicts(state: S<'_>) -> CmdResult<Vec<ConflictView>> {
    Ok(state.vessel.lock().await.conflicts()?)
}

#[tauri::command]
pub async fn dismiss_conflict(state: S<'_>, id: i64) -> CmdResult<()> {
    Ok(state.vessel.lock().await.dismiss_conflict(id)?)
}

// -- vaults -----------------------------------------------------------------------------------

#[tauri::command]
pub async fn vaults(state: S<'_>) -> CmdResult<Vec<VaultView>> {
    Ok(state.vessel.lock().await.vaults()?)
}

#[tauri::command]
pub async fn create_vault(state: S<'_>, name: String) -> CmdResult<Uuid> {
    let id = state.vessel.lock().await.create_vault(name.trim()).await?;
    state.changed();
    Ok(id)
}

#[tauri::command]
pub async fn update_vault(
    state: S<'_>,
    vault_id: Uuid,
    name: String,
    description: String,
    color: String,
) -> CmdResult<()> {
    state
        .vessel
        .lock()
        .await
        .update_vault(
            vault_id,
            VaultMeta {
                name,
                description,
                color,
                ..Default::default()
            },
        )
        .await?;
    state.changed();
    Ok(())
}

#[tauri::command]
pub async fn delete_vault(state: S<'_>, vault_id: Uuid) -> CmdResult<()> {
    state.vessel.lock().await.delete_vault(vault_id).await?;
    state.changed();
    Ok(())
}

#[tauri::command]
pub async fn members(state: S<'_>, vault_id: Uuid) -> CmdResult<MembersView> {
    Ok(state.vessel.lock().await.members(vault_id).await?)
}

#[tauri::command]
pub async fn invite(
    state: S<'_>,
    vault_id: Uuid,
    role: Role,
    email: Option<String>,
) -> CmdResult<InviteResponse> {
    Ok(state
        .vessel
        .lock()
        .await
        .invite(vault_id, role, email.filter(|e| !e.trim().is_empty()))
        .await?)
}

#[tauri::command]
pub async fn accept_invite(state: S<'_>, token: String) -> CmdResult<Option<Uuid>> {
    Ok(state.vessel.lock().await.accept_invite(&token).await?)
}

#[tauri::command]
pub async fn approve_member(
    state: S<'_>,
    vault_id: Uuid,
    user_id: Uuid,
    fingerprint: String,
) -> CmdResult<()> {
    // The fingerprint the user typed is checked again here, against the keys that actually
    // receive the vault key.
    state
        .vessel
        .lock()
        .await
        .approve_member_verified(vault_id, user_id, &fingerprint)
        .await?;
    state.changed();
    Ok(())
}

#[tauri::command]
pub async fn change_role(state: S<'_>, vault_id: Uuid, user_id: Uuid, role: Role) -> CmdResult<()> {
    Ok(state
        .vessel
        .lock()
        .await
        .change_role(vault_id, user_id, role)
        .await?)
}

#[tauri::command]
pub async fn remove_member(state: S<'_>, vault_id: Uuid, user_id: Uuid) -> CmdResult<()> {
    state
        .vessel
        .lock()
        .await
        .remove_member(vault_id, user_id)
        .await?;
    state.changed();
    Ok(())
}

#[tauri::command]
pub async fn leave_vault(state: S<'_>, vault_id: Uuid) -> CmdResult<()> {
    state.vessel.lock().await.leave_vault(vault_id).await?;
    state.changed();
    Ok(())
}

#[tauri::command]
pub async fn rotate_vault(state: S<'_>, vault_id: Uuid) -> CmdResult<()> {
    Ok(state.vessel.lock().await.rotate_vault(vault_id).await?)
}

#[tauri::command]
pub async fn fingerprint(state: S<'_>) -> CmdResult<String> {
    Ok(state.vessel.lock().await.fingerprint()?)
}

// -- terminals --------------------------------------------------------------------------------

#[tauri::command]
pub async fn detect_shells() -> CmdResult<Vec<DetectedShell>> {
    tokio::task::spawn_blocking(local::detect_shells)
        .await
        .map_err(|e| CmdError::from(e.to_string()))
}

#[tauri::command]
pub async fn open_local(
    state: S<'_>,
    profile_id: Option<Uuid>,
    shell: Option<Shell>,
    cols: u16,
    rows: u16,
    confirmed: Option<bool>,
) -> CmdResult<SessionInfo> {
    let target = match (profile_id, shell) {
        (Some(id), _) => LocalTarget::Profile(id),
        (None, Some(shell)) => LocalTarget::Shell(shell),
        (None, None) => LocalTarget::Shell(Shell::Pwsh),
    };
    ops::open_local(
        &state,
        target,
        cols,
        rows,
        false,
        confirmed.unwrap_or(false),
    )
    .await
}

#[tauri::command]
pub async fn open_ssh(
    state: S<'_>,
    host_id: Uuid,
    cols: u16,
    rows: u16,
    trust_new: bool,
) -> CmdResult<SessionInfo> {
    ops::open_ssh(&state, host_id, cols, rows, trust_new, false).await
}

#[tauri::command]
pub async fn terminals(state: S<'_>) -> CmdResult<Vec<SessionInfo>> {
    Ok(state.terms.list())
}

#[tauri::command]
pub async fn attach(state: S<'_>, id: Uuid, on_data: Channel<InvokeResponseBody>) -> CmdResult<()> {
    let session = state
        .terms
        .get(&id)
        .ok_or_else(|| CmdError::new("not_found", "Sessão encerrada."))?;
    session.attach(on_data);
    Ok(())
}

#[tauri::command]
pub async fn write(state: S<'_>, id: Uuid, data: String) -> CmdResult<()> {
    if let Some(s) = state.terms.get(&id) {
        s.write(data.into_bytes());
    }
    Ok(())
}

#[tauri::command]
pub async fn resize(state: S<'_>, id: Uuid, cols: u16, rows: u16) -> CmdResult<()> {
    if let Some(s) = state.terms.get(&id) {
        s.send(crate::terminal::Input::Resize { cols, rows });
    }
    Ok(())
}

#[tauri::command]
pub async fn close_terminal(state: S<'_>, id: Uuid) -> CmdResult<()> {
    state.terms.close(&state.app, &id);
    Ok(())
}

// -- port forwarding --------------------------------------------------------------------------

#[tauri::command]
pub async fn forwards(state: S<'_>) -> CmdResult<Vec<ForwardStatus>> {
    Ok(state.forwards.list())
}

#[tauri::command]
pub async fn start_forward(
    state: S<'_>,
    rule_id: Uuid,
    trust_new: bool,
) -> CmdResult<ForwardStatus> {
    ops::start_forward(&state, rule_id, trust_new, true).await
}

#[tauri::command]
pub async fn stop_forward(state: S<'_>, rule_id: Uuid) -> CmdResult<()> {
    state.forwards.stop(&rule_id).await;
    Ok(())
}

// -- MCP --------------------------------------------------------------------------------------

#[derive(Serialize)]
pub struct McpInfo {
    pub enabled: bool,
    pub running: bool,
    pub url: String,
    pub token: String,
    pub allow_terminal_input: bool,
    pub allow_writes: bool,
    pub error: Option<String>,
    pub bridge_path: Option<String>,
}

fn mcp_info(state: &AppState) -> McpInfo {
    let s = state.settings();
    let bridge = std::env::current_exe()
        .ok()
        .and_then(|exe| {
            exe.parent().map(|d| {
                d.join(if cfg!(windows) {
                    "vessel-mcp.exe"
                } else {
                    "vessel-mcp"
                })
            })
        })
        .filter(|p| p.is_file())
        .map(|p| p.display().to_string());
    McpInfo {
        enabled: s.mcp_enabled,
        running: state.mcp.is_running(),
        url: format!("http://127.0.0.1:{}/mcp", s.mcp_port),
        token: s.mcp_token,
        allow_terminal_input: s.mcp_allow_terminal_input,
        allow_writes: s.mcp_allow_writes,
        error: state.mcp.last_error(),
        bridge_path: bridge,
    }
}

#[tauri::command]
pub async fn mcp_status(state: S<'_>) -> CmdResult<McpInfo> {
    Ok(mcp_info(&state))
}

#[tauri::command]
pub async fn mcp_configure(
    state: S<'_>,
    enabled: bool,
    port: u16,
    allow_terminal_input: bool,
    allow_writes: bool,
) -> CmdResult<McpInfo> {
    state.update_settings(|s| {
        s.mcp_enabled = enabled;
        s.mcp_port = port.max(1024);
        s.mcp_allow_terminal_input = allow_terminal_input;
        s.mcp_allow_writes = allow_writes;
        if s.mcp_token.is_empty() {
            s.mcp_token = mcp::new_token();
        }
    })?;
    mcp::restart(&state).await;
    Ok(mcp_info(&state))
}

#[tauri::command]
pub async fn mcp_regenerate_token(state: S<'_>) -> CmdResult<McpInfo> {
    state.update_settings(|s| s.mcp_token = mcp::new_token())?;
    mcp::restart(&state).await;
    Ok(mcp_info(&state))
}

#[tauri::command]
pub async fn mcp_activity(state: S<'_>) -> CmdResult<Vec<Activity>> {
    Ok(state
        .activity
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .cloned()
        .collect())
}

// -- updates ----------------------------------------------------------------------------------

#[tauri::command]
pub async fn update_check(app: tauri::AppHandle) -> CmdResult<crate::update::UpdateStatus> {
    crate::update::check(&app).await
}

/// Installs the version the user confirmed. Terminals and forwards end: the app restarts.
#[tauri::command]
pub async fn update_install(state: S<'_>, version: String) -> CmdResult<()> {
    state.terms.close_all();
    state.forwards.stop_all();
    crate::update::install(&state.app, &version).await
}
