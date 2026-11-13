//! Vessel desktop app: Tauri backend.

mod background;
mod commands;
mod error;
mod forward;
mod local;
mod mcp;
mod ops;
mod ssh;
mod state;
mod terminal;

use std::sync::{Arc, Mutex};

use tauri::Manager;
use vessel_client::Vessel;

use crate::state::{AppState, load_settings};

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("info,vessel_desktop_lib=debug")
            }),
        )
        .init();

    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let vessel = Vessel::open(&data_dir.join("vessel.db"))?;
            let state = AppState {
                app: app.handle().clone(),
                vessel: Arc::new(tokio::sync::Mutex::new(vessel)),
                terms: Arc::default(),
                forwards: Arc::default(),
                sync_now: Arc::default(),
                settings: Arc::new(Mutex::new(load_settings(&data_dir))),
                data_dir,
                mcp: Arc::default(),
                activity: Arc::default(),
            };
            background::spawn(state.clone());
            // Start MCP right away when enabled, so agents get "locked" instead of "connection refused".
            let mcp_state = state.clone();
            tauri::async_runtime::spawn(async move { mcp::apply(&mcp_state).await });
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::status,
            commands::local_settings,
            commands::server_info,
            commands::signup,
            commands::login,
            commands::recover,
            commands::unlock,
            commands::lock,
            commands::logout,
            commands::change_password,
            commands::sync_now,
            commands::sync_background,
            commands::devices,
            commands::revoke_device,
            commands::signup_invite,
            commands::items,
            commands::save_item,
            commands::delete_item,
            commands::move_item,
            commands::conflicts,
            commands::dismiss_conflict,
            commands::vaults,
            commands::create_vault,
            commands::update_vault,
            commands::delete_vault,
            commands::members,
            commands::invite,
            commands::accept_invite,
            commands::approve_member,
            commands::change_role,
            commands::remove_member,
            commands::leave_vault,
            commands::rotate_vault,
            commands::fingerprint,
            commands::detect_shells,
            commands::open_local,
            commands::open_ssh,
            commands::terminals,
            commands::attach,
            commands::write,
            commands::resize,
            commands::close_terminal,
            commands::forwards,
            commands::start_forward,
            commands::stop_forward,
            commands::mcp_status,
            commands::mcp_configure,
            commands::mcp_regenerate_token,
            commands::mcp_activity,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Vessel");
}
