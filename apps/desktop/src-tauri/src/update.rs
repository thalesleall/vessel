//! App updates from the project's GitHub releases.
//!
//! Every update is signed with Vessel's own updater key, independent of any Windows code-signing
//! certificate. The public key is in `tauri.conf.json` (`plugins.updater.pubkey`); the updater
//! plugin refuses a download whose signature does not match it, and never installs a version
//! older than or equal to the running one. Nothing is installed without the user confirming the
//! version they were shown.

use serde::Serialize;
use tauri::AppHandle;
use tauri_plugin_updater::UpdaterExt;

use crate::error::{CmdError, CmdResult};

/// A newer version, as shown to the user before they confirm.
#[derive(Clone, Debug, Serialize)]
pub struct AvailableUpdate {
    pub version: String,
    pub notes: Option<String>,
    pub date: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct UpdateStatus {
    pub current: String,
    pub available: Option<AvailableUpdate>,
}

fn failed(what: &str, e: impl std::fmt::Display) -> CmdError {
    CmdError::new("update_failed", format!("{what}: {e}"))
}

fn updater(app: &AppHandle) -> CmdResult<tauri_plugin_updater::Updater> {
    let mut builder = app.updater_builder();
    // Development builds can point at a local release feed to try the flow; the signature check
    // against the embedded public key still applies.
    if cfg!(debug_assertions)
        && let Ok(url) = std::env::var("VESSEL_UPDATE_ENDPOINT")
    {
        let url = url.parse().map_err(|e| failed("endereço de atualização inválido", e))?;
        builder = builder
            .endpoints(vec![url])
            .map_err(|e| failed("endereço de atualização inválido", e))?;
    }
    builder
        .build()
        .map_err(|e| failed("o atualizador não está configurado", e))
}

/// Asks the release feed whether a newer version exists.
pub async fn check(app: &AppHandle) -> CmdResult<UpdateStatus> {
    let current = app.package_info().version.to_string();
    let update = updater(app)?
        .check()
        .await
        .map_err(|e| failed("não foi possível procurar atualizações", e))?;
    Ok(UpdateStatus {
        current,
        available: update.map(|u| AvailableUpdate {
            version: u.version.clone(),
            notes: u.body.clone(),
            date: u.date.map(|d| d.to_string()),
        }),
    })
}

/// Downloads, verifies and installs `version`, the one the user confirmed. If the feed now offers
/// a different version, nothing is installed and the user is asked again.
pub async fn install(app: &AppHandle, version: &str) -> CmdResult<()> {
    let update = updater(app)?
        .check()
        .await
        .map_err(|e| failed("não foi possível procurar atualizações", e))?
        .ok_or_else(|| CmdError::new("update_failed", "Não há atualização disponível."))?;
    if update.version != version {
        return Err(CmdError::new(
            "update_changed",
            format!(
                "A atualização disponível mudou para a versão {}. Confira e confirme de novo.",
                update.version
            ),
        ));
    }
    tracing::info!(version, "installing update");
    // The signature is checked against the embedded public key before the installer runs.
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| failed("a atualização foi recusada ou falhou", e))?;
    app.restart()
}
