//! Snippets that run on an AWS instance through Session Manager Run Command (`send-command`).
//!
//! The snippet itself syncs as usual, but which instance it runs on is kept on this device only
//! (sealed, never synced): nothing about AWS goes to the vault. On other computers the snippet
//! has no AWS destination.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use aws_sdk_ssm as ssm;
use serde::{Deserialize, Serialize};
use ssm::error::ProvideErrorMetadata;
use ssm::types::CommandInvocationStatus as Status;
use uuid::Uuid;
use vessel_client::Vessel;

use super::cli::{self, Tools};
use super::inventory::{self, Limits, SsmStatus};
use crate::error::{CmdError, CmdResult};
use crate::runs::TargetResult;
use crate::state::AppState;

const KEY: &str = "aws-snippet-targets";

/// An instance a snippet runs on, chosen on this device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AwsSnippetTarget {
    pub profile: String,
    pub region: String,
    pub instance_id: String,
    /// For the screen and the run history.
    pub label: String,
    /// Windows instances run the script with PowerShell, the others with sh.
    pub windows: bool,
}

pub fn targets(vessel: &Vessel) -> BTreeMap<Uuid, AwsSnippetTarget> {
    vessel.device_sealed(KEY).ok().flatten().unwrap_or_default()
}

pub fn target_of(vessel: &Vessel, snippet_id: Uuid) -> Option<AwsSnippetTarget> {
    targets(vessel).remove(&snippet_id)
}

#[tauri::command]
pub async fn aws_snippet_targets(
    state: tauri::State<'_, AppState>,
) -> CmdResult<BTreeMap<Uuid, AwsSnippetTarget>> {
    Ok(targets(&*state.vessel.lock().await))
}

/// Sets (or, with no instance, clears) the AWS destination of a snippet on this device. The
/// instance must be one the last mapping found, managed by Session Manager.
#[tauri::command]
pub async fn aws_set_snippet_target(
    state: tauri::State<'_, AppState>,
    snippet_id: Uuid,
    profile: Option<String>,
    region: Option<String>,
    instance_id: Option<String>,
) -> CmdResult<Option<AwsSnippetTarget>> {
    let target = match (profile, region, instance_id) {
        (Some(p), Some(r), Some(i)) => {
            let inv = super::cached_inventory(&state).await?.unwrap_or_default();
            let inst = inv
                .profiles
                .iter()
                .filter(|n| n.profile == p)
                .flat_map(|n| &n.regions)
                .filter(|n| n.region == r)
                .flat_map(|n| &n.instances)
                .find(|x| x.id == i)
                .ok_or_else(|| {
                    CmdError::new(
                        "not_found",
                        "Instância não encontrada no último mapeamento da AWS.",
                    )
                })?;
            if inst.ssm == SsmStatus::Unmanaged {
                return Err(CmdError::new(
                    "aws_unmanaged",
                    "Esta instância não é gerenciada pelo Session Manager.",
                ));
            }
            Some(AwsSnippetTarget {
                label: if inst.name.is_empty() {
                    inst.id.clone()
                } else {
                    inst.name.clone()
                },
                windows: inst.platform.to_ascii_lowercase().contains("windows"),
                profile: p,
                region: r,
                instance_id: i,
            })
        }
        (None, None, None) => None,
        _ => return Err(CmdError::new("invalid", "Destino AWS incompleto.")),
    };
    let vessel = state.vessel.lock().await;
    let mut all = targets(&vessel);
    // Destinations of snippets that no longer exist go away.
    all.retain(|id, _| vessel.find(*id).ok().flatten().is_some());
    match &target {
        Some(t) => all.insert(snippet_id, t.clone()),
        None => all.remove(&snippet_id),
    };
    vessel.set_device_sealed(KEY, &all)?;
    Ok(target)
}

fn cut(s: &str, cap: usize) -> (String, bool) {
    if s.len() <= cap {
        return (s.to_string(), false);
    }
    let mut end = s.len() - cap;
    while !s.is_char_boundary(end) {
        end += 1;
    }
    (s[end..].to_string(), true)
}

fn client(
    creds: &cli::Credentials,
    region: &str,
    http: ssm::config::SharedHttpClient,
) -> ssm::Client {
    let limits = Limits::default();
    let mut b = ssm::Config::builder()
        .behavior_version(ssm::config::BehaviorVersion::latest())
        .region(ssm::config::Region::new(region.to_string()))
        .credentials_provider(ssm::config::Credentials::new(
            &creds.access_key_id,
            &creds.secret_access_key,
            creds.session_token.clone(),
            None,
            "vessel-aws",
        ))
        .retry_config(inventory::retry(&limits))
        .timeout_config(inventory::timeouts(&limits))
        .http_client(http);
    if let Ok(e) = std::env::var("AWS_ENDPOINT_URL")
        && e.starts_with("http")
    {
        b = b.endpoint_url(e);
    }
    ssm::Client::from_conf(b.build())
}

/// Runs the script on the instance with Run Command and waits for it (or the time limit, which
/// cancels the command).
pub async fn run(
    tools: &Tools,
    http: ssm::config::SharedHttpClient,
    target: &AwsSnippetTarget,
    script: &str,
    timeout: Duration,
    cap: usize,
) -> TargetResult {
    let started = Instant::now();
    let mut result = TargetResult {
        target: target.label.clone(),
        ..Default::default()
    };
    let outcome: Result<(), String> = async {
        let creds = cli::export_credentials(tools, &target.profile, &target.region)
            .await
            .map_err(|e| e.message)?;
        let client = client(&creds, &target.region, http);
        drop(creds);
        let secs = timeout.as_secs().clamp(1, 172_800);
        let document = if target.windows {
            "AWS-RunPowerShellScript"
        } else {
            "AWS-RunShellScript"
        };
        let sent = client
            .send_command()
            .instance_ids(&target.instance_id)
            .document_name(document)
            .parameters("commands", vec![script.to_string()])
            .parameters("executionTimeout", vec![secs.to_string()])
            .comment("Vessel snippet")
            .send()
            .await
            .map_err(|e| inventory::sdk_error(&e, "ssm:SendCommand").message)?;
        let command_id = sent
            .command()
            .and_then(|c| c.command_id())
            .ok_or("A AWS não devolveu o ID do comando.")?
            .to_string();
        let deadline = Instant::now() + timeout + Duration::from_secs(15);
        loop {
            tokio::time::sleep(Duration::from_millis(1200)).await;
            if Instant::now() > deadline {
                let _ = client
                    .cancel_command()
                    .command_id(&command_id)
                    .instance_ids(&target.instance_id)
                    .send()
                    .await;
                result.timed_out = true;
                return Ok(());
            }
            let inv = match client
                .get_command_invocation()
                .command_id(&command_id)
                .instance_id(&target.instance_id)
                .send()
                .await
            {
                Ok(i) => i,
                // The invocation shows up a moment after the command.
                Err(e)
                    if e.as_service_error().and_then(|s| s.code())
                        == Some("InvocationDoesNotExist") =>
                {
                    continue;
                }
                Err(e) => return Err(inventory::sdk_error(&e, "ssm:GetCommandInvocation").message),
            };
            let status = inv.status().cloned().unwrap_or(Status::Pending);
            if matches!(
                status,
                Status::Pending | Status::InProgress | Status::Delayed | Status::Cancelling
            ) {
                continue;
            }
            let (stdout, t1) = cut(inv.standard_output_content().unwrap_or(""), cap);
            let (stderr, t2) = cut(inv.standard_error_content().unwrap_or(""), cap);
            result.stdout = stdout;
            result.stderr = stderr;
            result.truncated = t1 || t2;
            match status {
                Status::Success | Status::Failed => result.exit_code = Some(inv.response_code()),
                Status::TimedOut => result.timed_out = true,
                other => {
                    let details = inv.status_details().unwrap_or(other.as_str());
                    return Err(format!("O comando não rodou na instância ({details})."));
                }
            }
            return Ok(());
        }
    }
    .await;
    if let Err(e) = outcome {
        result.error = Some(e);
    }
    result.duration_ms = started.elapsed().as_millis() as u64;
    result
}

#[cfg(test)]
mod tests {
    use super::cut;

    #[test]
    fn output_keeps_its_end() {
        assert_eq!(cut("abc", 10), ("abc".into(), false));
        assert_eq!(cut("abcdef", 3), ("def".into(), true));
        let (s, t) = cut("ééé", 3);
        assert!(t && s == "é", "never splits a character: {s}");
    }
}
