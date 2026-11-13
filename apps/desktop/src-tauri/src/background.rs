//! Background work: periodic and event-driven sync, real-time notifications from the agent.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tauri::Emitter;
use vessel_client::{ServerEvent, events};

use crate::error::CmdError;
use crate::state::AppState;

const SYNC_INTERVAL: Duration = Duration::from_secs(60);

pub fn spawn(state: AppState) {
    tauri::async_runtime::spawn(sync_loop(state.clone()));
    tauri::async_runtime::spawn(events_loop(state));
}

async fn sync_loop(state: AppState) {
    loop {
        tokio::select! {
            _ = state.sync_now.notified() => {}
            _ = tokio::time::sleep(SYNC_INTERVAL) => {}
        }
        // Coalesce bursts of edits into one sync.
        tokio::time::sleep(Duration::from_millis(400)).await;
        run_sync(&state).await;
    }
}

pub async fn run_sync(state: &AppState) {
    let mut vessel = state.vessel.lock().await;
    if !vessel.status().unlocked {
        return;
    }
    let _ = state.app.emit("vessel://syncing", true);
    match vessel.sync().await {
        Ok(report) => {
            let _ = state.app.emit("vessel://synced", &report);
            if report.pulled > 0 || report.conflicts > 0 {
                let _ = state.app.emit("vessel://changed", ());
            }
        }
        Err(e) if e.is_unauthorized() => {
            tracing::warn!("session rejected by the server; signing out this device");
            state.terms.close_all();
            state.forwards.stop_all();
            let _ = vessel.forget();
            let _ = state.app.emit(
                "vessel://signed-out",
                "A sessão deste dispositivo foi encerrada pelo servidor.",
            );
        }
        Err(e) => {
            let _ = state.app.emit("vessel://sync-error", CmdError::from(e));
        }
    }
    let _ = state.app.emit("vessel://syncing", false);
}

async fn events_loop(state: AppState) {
    loop {
        let creds = {
            let mut vessel = state.vessel.lock().await;
            if vessel.status().unlocked {
                vessel.event_credentials().await.ok()
            } else {
                None
            }
        };
        let Some((server, token)) = creds else {
            tokio::time::sleep(Duration::from_secs(3)).await;
            continue;
        };
        let revoked = Arc::new(AtomicBool::new(false));
        let flag = revoked.clone();
        let notify = state.sync_now.clone();
        let _ = events::listen(&server, &token, move |event| match event {
            ServerEvent::DeviceRevoked { .. } => flag.store(true, Ordering::Relaxed),
            _ => notify.notify_one(),
        })
        .await;
        if revoked.load(Ordering::Relaxed) {
            state.terms.close_all();
            state.forwards.stop_all();
            let _ = state.vessel.lock().await.forget();
            let _ = state.app.emit(
                "vessel://signed-out",
                "Este dispositivo foi removido da sua conta.",
            );
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
