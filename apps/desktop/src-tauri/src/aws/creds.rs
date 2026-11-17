//! Credentials for a mapping, fast enough for hundreds of profiles.
//!
//! Starting the AWS CLI costs most of a second per profile. So the CLI is asked only for the
//! profiles it must resolve itself (the master: SSO, `aws login`, keys, a credential process,
//! and any role with MFA or a credential source), at most a few at a time. A plain assume-role
//! profile (`role_arn` + `source_profile`) is assumed here, with one STS call from its source's
//! credentials. Each profile is resolved once per mapping; everything lives in memory only and
//! is dropped with the resolver.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use aws_sdk_sts as sts;
use sts::error::ProvideErrorMetadata;
use tokio::sync::{Mutex, OnceCell, Semaphore};

use super::cli::{self, AwsError, Credentials, ErrorKind, Tools};
use super::config::Profile;
use super::inventory::{self, CredentialSource, Limits};

/// CLI processes at once: each one is a full interpreter start.
const CLI_AT_ONCE: usize = 6;
/// STS calls at once.
const STS_AT_ONCE: usize = 16;

type Cell = Arc<OnceCell<Result<Credentials, AwsError>>>;

pub struct Resolver {
    tools: Tools,
    profiles: HashMap<String, Profile>,
    /// Region of each profile (its own, else its master's), for the CLI and STS.
    regions: BTreeMap<String, String>,
    http: sts::config::SharedHttpClient,
    endpoint: Option<String>,
    cli: Semaphore,
    sts: Semaphore,
    cache: Mutex<HashMap<String, Cell>>,
}

impl Resolver {
    pub fn new(
        tools: Tools,
        profiles: &[Profile],
        regions: BTreeMap<String, String>,
        http: sts::config::SharedHttpClient,
        endpoint: Option<String>,
    ) -> Self {
        Self {
            tools,
            profiles: profiles
                .iter()
                .map(|p| (p.name.clone(), p.clone()))
                .collect(),
            regions,
            http,
            endpoint,
            cli: Semaphore::new(CLI_AT_ONCE),
            sts: Semaphore::new(STS_AT_ONCE),
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn region(&self, name: &str) -> String {
        self.regions
            .get(name)
            .cloned()
            .unwrap_or_else(|| "us-east-1".into())
    }

    /// The profile's credentials, resolved once and shared by everyone who asks.
    pub fn resolve(
        &self,
        name: String,
    ) -> Pin<Box<dyn Future<Output = Result<Credentials, AwsError>> + Send + '_>> {
        Box::pin(async move {
            let cell = self
                .cache
                .lock()
                .await
                .entry(name.clone())
                .or_default()
                .clone();
            cell.get_or_init(|| self.fetch(&name)).await.clone()
        })
    }

    async fn fetch(&self, name: &str) -> Result<Credentials, AwsError> {
        let region = self.region(name);
        match self.profiles.get(name) {
            Some(p) if p.simple_assume() => {
                let source = p.source_profile.clone().unwrap_or_default();
                let from = self.resolve(source).await?;
                let _slot = self.sts.acquire().await.expect("never closed");
                assume(
                    self.http.clone(),
                    self.endpoint.as_deref(),
                    &from,
                    p,
                    &region,
                )
                .await
            }
            _ => {
                let _slot = self.cli.acquire().await.expect("never closed");
                cli::export_credentials(&self.tools, name, &region).await
            }
        }
    }
}

impl CredentialSource for Resolver {
    fn credentials(
        &self,
        profile: &str,
    ) -> impl Future<Output = Result<Credentials, AwsError>> + Send {
        self.resolve(profile.to_string())
    }
}

/// Assumes the profile's role with the source's credentials, as the CLI would.
pub async fn assume(
    http: sts::config::SharedHttpClient,
    endpoint: Option<&str>,
    from: &Credentials,
    p: &Profile,
    region: &str,
) -> Result<Credentials, AwsError> {
    let role = p.role_arn.clone().unwrap_or_default();
    let limits = Limits::default();
    let mut b = sts::Config::builder()
        .behavior_version(sts::config::BehaviorVersion::latest())
        .region(sts::config::Region::new(region.to_string()))
        .credentials_provider(sts::config::Credentials::new(
            &from.access_key_id,
            &from.secret_access_key,
            from.session_token.clone(),
            None,
            "vessel-aws",
        ))
        .retry_config(inventory::retry(&limits))
        .timeout_config(inventory::timeouts(&limits))
        .http_client(http);
    if let Some(e) = endpoint {
        b = b.endpoint_url(e);
    }
    let client = sts::Client::from_conf(b.build());
    let session = p
        .assume
        .role_session_name
        .clone()
        .unwrap_or_else(|| format!("vessel-{}", std::process::id()));
    let mut req = client
        .assume_role()
        .role_arn(&role)
        .role_session_name(session);
    if let Some(x) = &p.assume.external_id {
        req = req.external_id(x);
    }
    if let Some(d) = p.assume.duration_seconds {
        req = req.duration_seconds(d);
    }
    match req.send().await {
        Ok(out) => {
            let c = out.credentials().ok_or_else(|| {
                AwsError::new(
                    ErrorKind::Other,
                    "A AWS não devolveu credenciais ao assumir o papel.",
                )
            })?;
            Ok(Credentials {
                access_key_id: c.access_key_id().to_string(),
                secret_access_key: c.secret_access_key().to_string(),
                session_token: Some(c.session_token().to_string()),
            })
        }
        Err(e) => {
            let code = e.as_service_error().and_then(|s| s.code()).unwrap_or("");
            if code == "AccessDenied" {
                return Err(AwsError::new(
                    ErrorKind::AssumeRoleDenied,
                    format!("Sem permissão para assumir {role}."),
                ));
            }
            Err(inventory::sdk_error(&e, "sts:AssumeRole"))
        }
    }
}
