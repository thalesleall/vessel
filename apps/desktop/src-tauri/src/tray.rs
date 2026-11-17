//! The tray icon, closing to the tray, one running copy of the app, and starting with Windows.
//!
//! Scheduled snippets only run while Vessel is open, so closing the window hides it instead of
//! quitting (the user can turn this off); "Sair" in the tray menu quits.

use std::sync::OnceLock;

use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Window, WindowEvent, Wry};

use crate::state::AppState;

/// Command-line flag used by the "start with Windows" entry: start in the tray.
pub const HIDDEN_FLAG: &str = "--hidden";

static PAUSE_ITEM: OnceLock<CheckMenuItem<Wry>> = OnceLock::new();

pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

pub fn setup(app: &tauri::App, state: &AppState) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Abrir o Vessel", true, None::<&str>)?;
    let pause = CheckMenuItem::with_id(
        app,
        "pause",
        "Pausar agendamentos",
        true,
        state.settings().schedules_paused,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, "quit", "Sair", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[&open, &pause, &PredefinedMenuItem::separator(app)?, &quit],
    )?;
    let _ = PAUSE_ITEM.set(pause);
    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("Vessel")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main(app),
            "pause" => {
                let state = app.state::<AppState>();
                let paused = !state.settings().schedules_paused;
                set_paused(&state, paused);
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;

    if !std::env::args().any(|a| a == HIDDEN_FLAG) {
        show_main(app.handle());
    }
    Ok(())
}

/// Pauses or resumes every schedule, from the app or the tray menu.
pub fn set_paused(state: &AppState, paused: bool) {
    let _ = state.update_settings(|s| s.schedules_paused = paused);
    if let Some(item) = PAUSE_ITEM.get() {
        let _ = item.set_checked(paused);
    }
    let _ = state.app.emit("snippets://paused", paused);
}

pub fn on_window_event(window: &Window, event: &WindowEvent) {
    if let WindowEvent::CloseRequested { api, .. } = event
        && window.label() == "main"
        && window
            .app_handle()
            .state::<AppState>()
            .settings()
            .close_to_tray
    {
        api.prevent_close();
        let _ = window.hide();
    }
}

// -- start with Windows ----------------------------------------------------------------------

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "Vessel";

#[cfg(windows)]
fn reg(args: &[&str]) -> std::io::Result<std::process::Output> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("reg")
        .args(args)
        .creation_flags(0x0800_0000)
        .output()
}

/// Whether Windows starts this copy of Vessel at sign-in.
pub fn autostart_enabled() -> bool {
    #[cfg(windows)]
    {
        let Ok(exe) = std::env::current_exe() else {
            return false;
        };
        reg(&["query", RUN_KEY, "/v", RUN_VALUE])
            .map(|o| {
                o.status.success()
                    && String::from_utf8_lossy(&o.stdout)
                        .to_lowercase()
                        .contains(&exe.display().to_string().to_lowercase())
            })
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    false
}

pub fn set_autostart(enabled: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        let out = if enabled {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let command = format!("\"{}\" {HIDDEN_FLAG}", exe.display());
            reg(&[
                "add", RUN_KEY, "/v", RUN_VALUE, "/t", "REG_SZ", "/d", &command, "/f",
            ])
        } else {
            reg(&["delete", RUN_KEY, "/v", RUN_VALUE, "/f"])
        }
        .map_err(|e| e.to_string())?;
        if out.status.success() || (!enabled && !autostart_enabled()) {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }
    #[cfg(not(windows))]
    {
        let _ = enabled;
        Err("Disponível só no Windows.".into())
    }
}
