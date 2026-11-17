//! Vessel desktop app: Tauri backend.

mod aws;
mod background;
mod commands;
mod cron;
mod error;
mod forward;
#[cfg(test)]
mod lab_tests;
mod local;
mod mcp;
mod ops;
mod runs;
mod scheduler;
mod ssh;
mod state;
mod terminal;
mod tray;
mod update;

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
        // A second launch (the Start menu while Vessel sits in the tray) shows the running copy:
        // two copies would share one database.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            tray::show_main(app)
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .on_window_event(tray::on_window_event)
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
                runs: Arc::default(),
                scheduler: Arc::default(),
                aws: Arc::default(),
            };
            runs::clean_tmp(&state.data_dir);
            background::spawn(state.clone());
            scheduler::spawn(state.clone());
            tray::setup(app, &state)?;
            // Start MCP right away when enabled, so agents get "locked" instead of "connection refused".
            let mcp_state = state.clone();
            tauri::async_runtime::spawn(async move { mcp::apply(&mcp_state).await });
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            aws::aws_overview,
            aws::aws_session_status,
            aws::aws_login,
            aws::aws_map,
            aws::aws_cancel,
            aws::aws_inventory,
            aws::aws_save_settings,
            aws::aws_set_agents,
            aws::aws_open_session,
            aws::aws_port_forward,
            aws::aws_ssh,
            aws::aws_forget_host_key,
            aws::snippets::aws_snippet_targets,
            aws::snippets::aws_set_snippet_target,
            commands::status,
            commands::local_settings,
            commands::server_info,
            commands::signup,
            commands::login,
            commands::recover,
            commands::create_local,
            commands::recover_local,
            commands::connect_new_account,
            commands::connect_existing_account,
            commands::disconnect_to_local,
            commands::unlock,
            commands::lock,
            commands::logout,
            commands::change_password,
            commands::sync_now,
            commands::sync_background,
            commands::devices,
            commands::revoke_device,
            commands::signup_invite,
            commands::delete_account,
            commands::admin_users,
            commands::admin_update_user,
            commands::admin_delete_user,
            commands::admin_stats,
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
            commands::update_check,
            commands::update_install,
            commands::run_snippet,
            commands::snippet_runs,
            commands::clear_snippet_runs,
            commands::scheduler_status,
            commands::set_schedules_paused,
            commands::cron_preview,
            commands::get_background_settings,
            commands::set_background_settings,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Vessel");
}
