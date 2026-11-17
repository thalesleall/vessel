//! The AWS CLI and the Session Manager Plugin of this computer. Vessel never reads AWS keys: it
//! asks the CLI for each profile's temporary credentials (`configure export-credentials`), which
//! resolves SSO, `aws login`, assume role and MFA itself. Arguments always go as a list, without a
//! shell.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::process::Command;

/// Where the tools are, and their versions.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Tools {
    pub cli: Option<String>,
    pub cli_version: Option<String>,
    pub plugin: Option<String>,
    pub plugin_version: Option<String>,
}

impl Tools {
    pub fn cli_path(&self) -> Option<&Path> {
        self.cli.as_deref().map(Path::new)
    }
}

/// Finds `aws` and `session-manager-plugin` on the PATH or in their default install folders.
pub async fn detect() -> Tools {
    let cli = find(
        &["aws.exe", "aws.cmd", "aws"],
        &[
            env_join("ProgramFiles", r"Amazon\AWSCLIV2\aws.exe"),
            env_join("LOCALAPPDATA", r"Programs\Amazon\AWSCLIV2\aws.exe"),
            Some(PathBuf::from("/usr/local/bin/aws")),
        ],
    );
    let plugin = find(
        &["session-manager-plugin.exe", "session-manager-plugin"],
        &[
            env_join(
                "ProgramFiles",
                r"Amazon\SessionManagerPlugin\bin\session-manager-plugin.exe",
            ),
            Some(PathBuf::from("/usr/local/bin/session-manager-plugin")),
            Some(PathBuf::from(
                "/usr/local/sessionmanagerplugin/bin/session-manager-plugin",
            )),
        ],
    );
    let mut tools = Tools {
        cli: cli.as_ref().map(|p| p.display().to_string()),
        plugin: plugin.as_ref().map(|p| p.display().to_string()),
        ..Default::default()
    };
    if let Some(cli) = &cli
        && let Ok(out) = run(cli, None, &["--version"], Duration::from_secs(20)).await
    {
        // "aws-cli/2.39.3 Python/3.14.8 Windows/11 ..."
        tools.cli_version = out
            .stdout
            .split_whitespace()
            .next()
            .map(|s| s.trim_start_matches("aws-cli/").to_string());
    }
    if let Some(plugin) = &plugin
        && let Ok(out) = run(plugin, None, &["--version"], Duration::from_secs(20)).await
    {
        tools.plugin_version = Some(out.stdout.trim().to_string()).filter(|s| !s.is_empty());
    }
    tools
}

fn env_join(var: &str, rest: &str) -> Option<PathBuf> {
    std::env::var_os(var).map(|v| PathBuf::from(v).join(rest))
}

fn find(names: &[&str], fallbacks: &[Option<PathBuf>]) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for n in names {
                let p = dir.join(n);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    fallbacks.iter().flatten().find(|p| p.is_file()).cloned()
}

/// The PATH for child processes: the plugin's folder first, so the CLI finds it even when the
/// plugin was installed without touching the PATH.
pub fn child_path(tools: &Tools) -> Option<std::ffi::OsString> {
    let dir = tools
        .plugin
        .as_deref()
        .and_then(|p| Path::new(p).parent())?;
    let mut parts = vec![dir.to_path_buf()];
    if let Some(path) = std::env::var_os("PATH") {
        parts.extend(std::env::split_paths(&path));
    }
    std::env::join_paths(parts).ok()
}

pub struct Output {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Runs a program with arguments (no shell), without a window, killed after `timeout`.
pub async fn run(
    program: &Path,
    path_env: Option<&std::ffi::OsStr>,
    args: &[&str],
    timeout: Duration,
) -> std::io::Result<Output> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        // Plain output in the CLI's own language-neutral form, and no pager.
        .env("AWS_PAGER", "")
        .env("AWS_CLI_AUTO_PROMPT", "off");
    if let Some(p) = path_env {
        cmd.env("PATH", p);
    }
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let child = cmd.spawn()?;
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(out) => {
            let out = out?;
            Ok(Output {
                status: out.status.code(),
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            })
        }
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "o AWS CLI não respondeu a tempo",
        )),
    }
}

/// Temporary credentials of one profile. They live only in memory, for one mapping, and are
/// never written or shown.
#[derive(Clone, Deserialize)]
pub struct Credentials {
    #[serde(rename = "AccessKeyId")]
    pub access_key_id: String,
    #[serde(rename = "SecretAccessKey")]
    pub secret_access_key: String,
    #[serde(rename = "SessionToken", default)]
    pub session_token: Option<String>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credentials(<hidden>)")
    }
}

/// Asks the CLI for `profile`'s credentials. `region` is needed by profiles that assume a role
/// without a region of their own (the CLI refuses them otherwise). Errors come back classified
/// for the screen.
pub async fn export_credentials(
    tools: &Tools,
    profile: &str,
    region: &str,
) -> Result<Credentials, AwsError> {
    let cli = tools.cli_path().ok_or_else(AwsError::cli_missing)?;
    let path = child_path(tools);
    let out = run(
        cli,
        path.as_deref(),
        &[
            "configure",
            "export-credentials",
            "--profile",
            profile,
            "--format",
            "process",
            "--region",
            region,
        ],
        Duration::from_secs(90),
    )
    .await
    .map_err(|e| {
        AwsError::new(
            ErrorKind::Other,
            format!("Não foi possível rodar o AWS CLI: {e}"),
        )
    })?;
    if out.status == Some(0) {
        serde_json::from_str::<Credentials>(out.stdout.trim()).map_err(|_| {
            AwsError::new(
                ErrorKind::Other,
                "O AWS CLI devolveu credenciais num formato inesperado.",
            )
        })
    } else {
        Err(classify(&out.stderr, None))
    }
}

/// What went wrong, in groups the screen treats differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    CliMissing,
    /// No session, or it expired: sign in again ("Entrar na AWS").
    SessionExpired,
    AssumeRoleDenied,
    AccessDenied,
    Throttled,
    RegionDisabled,
    Network,
    Cancelled,
    Other,
}

#[derive(Clone, Debug, Serialize)]
pub struct AwsError {
    pub kind: ErrorKind,
    pub message: String,
}

impl AwsError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
    pub fn cli_missing() -> Self {
        Self::new(
            ErrorKind::CliMissing,
            "O AWS CLI não foi encontrado neste computador.",
        )
    }
}

impl std::fmt::Display for AwsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Turns an AWS error (CLI stderr, or an SDK error code and message) into what the screen shows.
/// `action` is the API call that failed, for access errors that do not name it.
pub fn classify(text: &str, action: Option<&str>) -> AwsError {
    let t = text.trim();
    let lower = t.to_ascii_lowercase();
    let has = |s: &str| lower.contains(&s.to_ascii_lowercase());

    if has("assumerole") && (has("accessdenied") || has("not authorized")) {
        let role = find_after(t, "on resource: ").or_else(|| find_word(t, "arn:aws:iam::"));
        return AwsError::new(
            ErrorKind::AssumeRoleDenied,
            match role {
                Some(r) => format!("Sem permissão para assumir {r}."),
                None => "Sem permissão para assumir o papel deste perfil.".into(),
            },
        );
    }
    if has("token has expired")
        || has("session has expired")
        || has("expiredtoken")
        || has("requestexpired")
        || has("the sso session associated with this profile has expired")
        || has("unable to locate credentials")
        || has("aws login")
        || has("refresh failed")
        || has("error loading sso token")
        || has("invalidclienttokenid")
        || has("authfailure")
        || has("security token included in the request is invalid")
        || has("unrecognizedclientexception")
    {
        return AwsError::new(
            ErrorKind::SessionExpired,
            "Sessão expirada ou ausente. Entre na AWS de novo.",
        );
    }
    if has("throttl")
        || has("requestlimitexceeded")
        || has("rate exceeded")
        || has("toomanyrequests")
    {
        return AwsError::new(
            ErrorKind::Throttled,
            "Limite da API atingido. Tente de novo depois.",
        );
    }
    if has("unauthorizedoperation") || has("accessdenied") || has("not authorized to perform") {
        let missing = find_after(t, "perform: ").or_else(|| action.map(str::to_string));
        return AwsError::new(
            ErrorKind::AccessDenied,
            match missing {
                Some(a) => format!("Falta a permissão {a}."),
                None => "Sem permissão para esta consulta.".into(),
            },
        );
    }
    if has("optinrequired")
        || has("not subscribed")
        || has("region is disabled")
        || has("not enabled for this region")
    {
        return AwsError::new(
            ErrorKind::RegionDisabled,
            format!("Região desabilitada: {}", summary(t)),
        );
    }
    if has("dispatch failure")
        || has("timed out")
        || has("timeout")
        || has("could not connect")
        || has("dns error")
        || has("connection refused")
        || has("io error")
        || has("connector error")
    {
        return AwsError::new(
            ErrorKind::Network,
            format!("Sem conexão com a AWS: {}", summary(t)),
        );
    }
    if has("the config profile") && has("could not be found") {
        return AwsError::new(
            ErrorKind::Other,
            "Perfil não encontrado no arquivo de configuração da AWS.",
        );
    }
    AwsError::new(ErrorKind::Other, summary(t))
}

/// The first useful line, without the CLI's noise, at most 300 characters.
fn summary(t: &str) -> String {
    let line = t
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("Erro desconhecido do AWS CLI.");
    let line = ["aws: [ERROR]:", "aws: error:"]
        .iter()
        .fold(line, |l, p| l.strip_prefix(p).unwrap_or(l))
        .trim();
    let mut s: String = line.chars().take(300).collect();
    if line.chars().count() > 300 {
        s.push('…');
    }
    s
}

fn find_after(t: &str, marker: &str) -> Option<String> {
    let i = t.find(marker)? + marker.len();
    let word: String = t[i..]
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != ',' && *c != '"')
        .collect();
    let word = word.trim_end_matches(['.', ')']).to_string();
    (!word.is_empty()).then_some(word)
}

fn find_word(t: &str, prefix: &str) -> Option<String> {
    let i = t.find(prefix)?;
    let word: String = t[i..]
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != ',' && *c != '"')
        .collect();
    Some(word.trim_end_matches(['.', ')']).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_become_screen_messages() {
        let e = classify(
            "An error occurred (AccessDenied) when calling the AssumeRole operation: User: arn:aws:iam::111111111111:user/me is not authorized to perform: sts:AssumeRole on resource: arn:aws:iam::222222222222:role/Admin",
            None,
        );
        assert_eq!(e.kind, ErrorKind::AssumeRoleDenied);
        assert_eq!(
            e.message,
            "Sem permissão para assumir arn:aws:iam::222222222222:role/Admin."
        );

        let e = classify(
            "AccessDeniedException: User: arn:aws:sts::2:assumed-role/R/x is not authorized to perform: ssm:DescribeInstanceInformation because no identity-based policy allows",
            None,
        );
        assert_eq!(
            (e.kind, e.message.as_str()),
            (
                ErrorKind::AccessDenied,
                "Falta a permissão ssm:DescribeInstanceInformation."
            )
        );

        let e = classify(
            "UnauthorizedOperation: You are not authorized to perform this operation.",
            Some("ec2:DescribeInstances"),
        );
        assert_eq!(e.message, "Falta a permissão ec2:DescribeInstances.");

        for expired in [
            "Error when retrieving token from sso: Token has expired and refresh failed",
            "Your session has expired. Please reauthenticate using 'aws login'.",
            "Unable to locate credentials. You can configure credentials by running \"aws configure\".",
            "ExpiredToken: The security token included in the request is expired",
        ] {
            assert_eq!(
                classify(expired, None).kind,
                ErrorKind::SessionExpired,
                "{expired}"
            );
        }

        let e = classify("RequestLimitExceeded: Request limit exceeded.", None);
        assert_eq!(e.kind, ErrorKind::Throttled);
        assert_eq!(
            classify("ThrottlingException: Rate exceeded", None).kind,
            ErrorKind::Throttled
        );
        assert_eq!(
            classify(
                "OptInRequired: You are not subscribed to this service.",
                None
            )
            .kind,
            ErrorKind::RegionDisabled
        );
        assert_eq!(
            classify("dispatch failure: io error: connection refused", None).kind,
            ErrorKind::Network
        );

        let e = classify(
            "aws: [ERROR]: An error occurred (NoRegion): You must specify a region.",
            None,
        );
        assert_eq!(
            e.message,
            "An error occurred (NoRegion): You must specify a region."
        );
        let e = classify("\n\naws: error: something odd happened\nmore", None);
        assert_eq!(
            (e.kind, e.message.as_str()),
            (ErrorKind::Other, "something odd happened")
        );
        assert!(classify(&"x".repeat(1000), None).message.chars().count() <= 301);
    }

    #[test]
    fn credentials_never_print() {
        let c: Credentials = serde_json::from_str(r#"{"Version":1,"AccessKeyId":"AKIAEXAMPLE","SecretAccessKey":"secret","SessionToken":"tok","Expiration":"2026-11-17T12:00:00Z"}"#).unwrap();
        assert_eq!(format!("{c:?}"), "Credentials(<hidden>)");
        assert_eq!(c.session_token.as_deref(), Some("tok"));
    }
}
