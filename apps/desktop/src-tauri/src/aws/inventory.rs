//! Maps the EC2 instances (and their Session Manager status) of every profile and region, on
//! demand. One async runtime, a global cap on concurrent queries, at most one query per account
//! and region at a time, the SDK's adaptive retry against throttling, results streamed region by
//! region in a lean form, and a cancel that stops everything.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_sdk_ec2 as ec2;
use aws_sdk_ssm as ssm;
use ec2::error::{DisplayErrorContext, ProvideErrorMetadata};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Semaphore, watch};

use super::cli::{AwsError, Credentials, ErrorKind, classify};

/// Session Manager status of an instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SsmStatus {
    Online,
    /// Registered, but the agent is not answering (connection lost or inactive).
    Offline,
    /// Not registered with Session Manager: no SSM actions.
    Unmanaged,
}

/// Only the fields the screen uses: a large inventory stays small.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Instance {
    pub id: String,
    pub name: String,
    /// running, stopped, ...; empty for hybrid nodes (not EC2).
    pub state: String,
    pub ssm: SsmStatus,
    pub platform: String,
    pub private_ip: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegionNode {
    pub region: String,
    pub instances: Vec<Instance>,
    pub error: Option<AwsError>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProfileNode {
    pub profile: String,
    pub account: Option<String>,
    pub role_arn: Option<String>,
    pub regions: Vec<RegionNode>,
    /// Signing in or listing the regions failed: no region was mapped.
    pub error: Option<AwsError>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Inventory {
    /// Unix seconds of the mapping.
    pub mapped_at: i64,
    pub profiles: Vec<ProfileNode>,
    /// The mapping was cancelled before the end.
    pub partial: bool,
}

impl Inventory {
    pub fn instance_count(&self) -> usize {
        self.profiles
            .iter()
            .flat_map(|p| &p.regions)
            .map(|r| r.instances.len())
            .sum()
    }
}

impl<'de> Deserialize<'de> for AwsError {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            kind: ErrorKind,
            message: String,
        }
        let r = Raw::deserialize(d)?;
        Ok(AwsError::new(r.kind, r.message))
    }
}

impl<'de> Deserialize<'de> for ErrorKind {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(match s.as_str() {
            "cli_missing" => ErrorKind::CliMissing,
            "session_expired" => ErrorKind::SessionExpired,
            "assume_role_denied" => ErrorKind::AssumeRoleDenied,
            "access_denied" => ErrorKind::AccessDenied,
            "throttled" => ErrorKind::Throttled,
            "region_disabled" => ErrorKind::RegionDisabled,
            "network" => ErrorKind::Network,
            "cancelled" => ErrorKind::Cancelled,
            _ => ErrorKind::Other,
        })
    }
}

/// What to map: each profile, and its regions (None: every enabled region).
#[derive(Clone, Debug)]
pub struct Job {
    pub profile: String,
    pub account: Option<String>,
    pub role_arn: Option<String>,
    /// Region for the first call (DescribeRegions).
    pub home_region: String,
    pub regions: Option<Vec<String>>,
}

/// Progress for the bar: "conta 3/8 · região 12/17".
#[derive(Clone, Debug, Default, Serialize)]
pub struct Progress {
    pub accounts_done: usize,
    pub accounts_total: usize,
    pub regions_done: usize,
    pub regions_total: usize,
    pub elapsed_ms: u64,
}

/// Where results go while mapping (the window, or a test).
pub trait Sink: Send + Sync + 'static {
    fn progress(&self, p: &Progress);
    fn profile(&self, node: &ProfileNode);
    fn region(&self, profile: &str, node: &RegionNode);
}

/// Turns a profile into temporary credentials (the CLI in the app, a fixed pair in tests).
pub trait CredentialSource: Send + Sync + 'static {
    fn credentials(
        &self,
        profile: &str,
    ) -> impl Future<Output = Result<Credentials, AwsError>> + Send;
}

#[derive(Clone, Debug)]
pub struct Limits {
    /// Queries running at once, across all accounts.
    pub concurrency: usize,
    pub max_attempts: u32,
    pub initial_backoff: Duration,
    /// Per API call, including retries.
    pub call_timeout: Duration,
    /// Talk to this endpoint instead of AWS (tests).
    pub endpoint: Option<String>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            concurrency: 4,
            max_attempts: 6,
            initial_backoff: Duration::from_millis(500),
            call_timeout: Duration::from_secs(60),
            endpoint: None,
        }
    }
}

/// Stops a mapping: everything waiting or running gives up at its next step.
#[derive(Clone)]
pub struct Cancel(watch::Sender<bool>);

impl Default for Cancel {
    fn default() -> Self {
        Self(watch::channel(false).0)
    }
}

impl Cancel {
    pub fn cancel(&self) {
        let _ = self.0.send(true);
    }
    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
    async fn cancelled(&self) {
        let mut rx = self.0.subscribe();
        let _ = rx.wait_for(|c| *c).await;
    }
    /// Runs `f`, or gives up when cancelled.
    async fn run<T>(&self, f: impl Future<Output = T>) -> Option<T> {
        tokio::select! {
            biased;
            _ = self.cancelled() => None,
            v = f => Some(v),
        }
    }
}

/// A global cap on concurrent queries, and one query at a time per key (account and region).
pub struct Limiter {
    global: Semaphore,
    keys: std::sync::Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl Limiter {
    pub fn new(concurrency: usize) -> Self {
        Self {
            global: Semaphore::new(concurrency.max(1)),
            keys: Default::default(),
        }
    }

    /// Waits for a slot for `key`. Holding the guard holds the slot.
    pub async fn acquire(&self, key: &str) -> LimiterGuard<'_> {
        let lock = self
            .keys
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(key.to_string())
            .or_default()
            .clone();
        // The per-key lock first, so a busy key does not hold a global slot while it waits.
        let key_guard = lock.lock_owned().await;
        let permit = self.global.acquire().await.expect("never closed");
        LimiterGuard {
            _permit: permit,
            _key: key_guard,
        }
    }
}

pub struct LimiterGuard<'a> {
    _permit: tokio::sync::SemaphorePermit<'a>,
    _key: tokio::sync::OwnedMutexGuard<()>,
}

/// One HTTPS client for every query (one connection pool, one TLS setup).
pub fn shared_http_client() -> ec2::config::SharedHttpClient {
    use aws_smithy_http_client::{Builder, tls};
    Builder::new()
        .tls_provider(tls::Provider::Rustls(
            tls::rustls_provider::CryptoMode::AwsLc,
        ))
        .build_https()
}

struct Ctx<C, S> {
    creds: C,
    sink: S,
    limits: Limits,
    limiter: Limiter,
    cancel: Cancel,
    http: ec2::config::SharedHttpClient,
    started: Instant,
    progress: std::sync::Mutex<Progress>,
}

impl<C: CredentialSource, S: Sink> Ctx<C, S> {
    fn bump(&self, f: impl FnOnce(&mut Progress)) {
        let snapshot = {
            let mut p = self.progress.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut p);
            p.elapsed_ms = self.started.elapsed().as_millis() as u64;
            p.clone()
        };
        self.sink.progress(&snapshot);
    }

    fn ec2(&self, c: &Credentials, region: &str) -> ec2::Client {
        let mut b = ec2::Config::builder()
            .behavior_version(ec2::config::BehaviorVersion::latest())
            .region(ec2::config::Region::new(region.to_string()))
            .credentials_provider(ec2::config::Credentials::new(
                &c.access_key_id,
                &c.secret_access_key,
                c.session_token.clone(),
                None,
                "vessel-aws",
            ))
            .retry_config(retry(&self.limits))
            .timeout_config(timeouts(&self.limits))
            .http_client(self.http.clone());
        if let Some(e) = &self.limits.endpoint {
            b = b.endpoint_url(e);
        }
        ec2::Client::from_conf(b.build())
    }

    fn ssm(&self, c: &Credentials, region: &str) -> ssm::Client {
        let mut b = ssm::Config::builder()
            .behavior_version(ssm::config::BehaviorVersion::latest())
            .region(ssm::config::Region::new(region.to_string()))
            .credentials_provider(ssm::config::Credentials::new(
                &c.access_key_id,
                &c.secret_access_key,
                c.session_token.clone(),
                None,
                "vessel-aws",
            ))
            .retry_config(retry(&self.limits))
            .timeout_config(timeouts(&self.limits))
            .http_client(self.http.clone());
        if let Some(e) = &self.limits.endpoint {
            b = b.endpoint_url(e);
        }
        ssm::Client::from_conf(b.build())
    }
}

pub(crate) fn retry(l: &Limits) -> ec2::config::retry::RetryConfig {
    ec2::config::retry::RetryConfig::adaptive()
        .with_max_attempts(l.max_attempts)
        .with_initial_backoff(l.initial_backoff)
        .with_max_backoff(Duration::from_secs(20))
}

pub(crate) fn timeouts(l: &Limits) -> ec2::config::timeout::TimeoutConfig {
    ec2::config::timeout::TimeoutConfig::builder()
        .connect_timeout(Duration::from_secs(10))
        .operation_attempt_timeout(Duration::from_secs(30))
        .operation_timeout(l.call_timeout)
        .build()
}

/// Maps every job. Results stream to `sink` as each region finishes; the whole inventory is
/// returned at the end (marked partial when cancelled). Credentials are dropped when it ends.
pub async fn map<C: CredentialSource, S: Sink>(
    jobs: Vec<Job>,
    creds: C,
    sink: S,
    limits: Limits,
    cancel: Cancel,
    http: ec2::config::SharedHttpClient,
) -> Inventory {
    let ctx = Arc::new(Ctx {
        creds,
        sink,
        limiter: Limiter::new(limits.concurrency),
        limits,
        cancel: cancel.clone(),
        http,
        started: Instant::now(),
        progress: std::sync::Mutex::new(Progress {
            accounts_total: jobs.len(),
            ..Default::default()
        }),
    });
    ctx.bump(|_| {});
    let mut set = tokio::task::JoinSet::new();
    for job in jobs {
        let ctx = ctx.clone();
        set.spawn(async move { map_profile(&ctx, job).await });
    }
    let mut profiles = Vec::new();
    while let Some(r) = set.join_next().await {
        if let Ok(node) = r {
            profiles.push(node);
        }
    }
    profiles.sort_by(|a, b| a.profile.cmp(&b.profile));
    Inventory {
        mapped_at: now(),
        profiles,
        partial: cancel.is_cancelled(),
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn cancelled() -> AwsError {
    AwsError::new(ErrorKind::Cancelled, "Mapeamento cancelado.")
}

async fn map_profile<C: CredentialSource, S: Sink>(ctx: &Ctx<C, S>, job: Job) -> ProfileNode {
    let mut node = ProfileNode {
        profile: job.profile.clone(),
        account: job.account.clone(),
        role_arn: job.role_arn.clone(),
        regions: Vec::new(),
        error: None,
    };
    let fail = |mut node: ProfileNode, e: AwsError| {
        node.error = Some(e);
        ctx.sink.profile(&node);
        ctx.bump(|p| p.accounts_done += 1);
        node
    };

    // Signing in through the CLI is a query too (it may assume a role).
    let creds = {
        let _slot = ctx
            .limiter
            .acquire(&format!("profile:{}", job.profile))
            .await;
        match ctx.cancel.run(ctx.creds.credentials(&job.profile)).await {
            None => return fail(node, cancelled()),
            Some(Err(e)) => return fail(node, e),
            Some(Ok(c)) => c,
        }
    };
    let account_key = job
        .account
        .clone()
        .unwrap_or_else(|| format!("profile:{}", job.profile));

    let regions = match job.regions.clone() {
        Some(r) if !r.is_empty() => r,
        _ => {
            let _slot = ctx
                .limiter
                .acquire(&format!("{account_key}:{}", job.home_region))
                .await;
            let client = ctx.ec2(&creds, &job.home_region);
            match ctx.cancel.run(client.describe_regions().send()).await {
                None => return fail(node, cancelled()),
                Some(Err(e)) => return fail(node, sdk_error(&e, "ec2:DescribeRegions")),
                Some(Ok(out)) => {
                    let mut r: Vec<String> = out
                        .regions()
                        .iter()
                        .filter_map(|r| r.region_name().map(str::to_string))
                        .collect();
                    r.sort();
                    r
                }
            }
        }
    };
    ctx.sink.profile(&node);
    ctx.bump(|p| p.regions_total += regions.len());

    // All regions of the profile are polled together on this task (no thread or task per
    // query); the limiter decides how many actually talk to AWS.
    use futures_util::stream::{FuturesUnordered, StreamExt};
    let mut pending: FuturesUnordered<_> = regions
        .into_iter()
        .map(|region| {
            let key = format!("{account_key}:{region}");
            map_region(ctx, &creds, region, key)
        })
        .collect();
    let mut results = Vec::new();
    while let Some(r) = pending.next().await {
        ctx.sink.region(&job.profile, &r);
        ctx.bump(|p| p.regions_done += 1);
        results.push(r);
    }
    drop(pending);
    drop(creds);
    results.sort_by(|a, b| a.region.cmp(&b.region));
    node.regions = results;
    ctx.bump(|p| p.accounts_done += 1);
    node
}

async fn map_region<C: CredentialSource, S: Sink>(
    ctx: &Ctx<C, S>,
    creds: &Credentials,
    region: String,
    key: String,
) -> RegionNode {
    let mut node = RegionNode {
        region: region.clone(),
        instances: Vec::new(),
        error: None,
    };
    let result = ctx
        .cancel
        .run(async {
            let _slot = ctx.limiter.acquire(&key).await;
            let ssm_status = list_ssm(&ctx.ssm(creds, &region)).await?;
            let instances = list_ec2(&ctx.ec2(creds, &region), &ssm_status).await?;
            Ok::<_, AwsError>((instances, ssm_status))
        })
        .await;
    match result {
        None => node.error = Some(cancelled()),
        Some(Err(e)) => node.error = Some(e),
        Some(Ok((mut instances, ssm_status))) => {
            // Nodes registered with Session Manager that are not EC2 (on-premises, hybrid).
            let known: std::collections::HashSet<&str> =
                instances.iter().map(|i| i.id.as_str()).collect();
            let mut hybrid: Vec<Instance> = ssm_status
                .into_iter()
                .filter(|(id, _)| !known.contains(id.as_str()))
                .map(|(id, s)| Instance {
                    name: s.name,
                    state: String::new(),
                    ssm: s.status,
                    platform: s.platform,
                    private_ip: s.ip,
                    id,
                })
                .collect();
            instances.append(&mut hybrid);
            instances.sort_by(|a, b| {
                a.name
                    .to_lowercase()
                    .cmp(&b.name.to_lowercase())
                    .then(a.id.cmp(&b.id))
            });
            instances.shrink_to_fit();
            node.instances = instances;
        }
    }
    node
}

struct SsmInfo {
    status: SsmStatus,
    name: String,
    platform: String,
    ip: String,
}

async fn list_ssm(client: &ssm::Client) -> Result<BTreeMap<String, SsmInfo>, AwsError> {
    let mut out = BTreeMap::new();
    let mut pages = client
        .describe_instance_information()
        .max_results(50)
        .into_paginator()
        .send();
    while let Some(page) = pages.next().await {
        let page = page.map_err(|e| sdk_error(&e, "ssm:DescribeInstanceInformation"))?;
        for i in page.instance_information_list() {
            let Some(id) = i.instance_id() else { continue };
            let status = match i.ping_status() {
                Some(ssm::types::PingStatus::Online) => SsmStatus::Online,
                _ => SsmStatus::Offline,
            };
            let platform = [
                i.platform_name().unwrap_or(""),
                i.platform_version().unwrap_or(""),
            ]
            .join(" ")
            .trim()
            .to_string();
            out.insert(
                id.to_string(),
                SsmInfo {
                    status,
                    name: i.name().or(i.computer_name()).unwrap_or("").to_string(),
                    platform,
                    ip: i.ip_address().unwrap_or("").to_string(),
                },
            );
        }
    }
    Ok(out)
}

async fn list_ec2(
    client: &ec2::Client,
    ssm_status: &BTreeMap<String, SsmInfo>,
) -> Result<Vec<Instance>, AwsError> {
    let mut out = Vec::new();
    let mut pages = client
        .describe_instances()
        .max_results(1000)
        .into_paginator()
        .send();
    while let Some(page) = pages.next().await {
        let page = page.map_err(|e| sdk_error(&e, "ec2:DescribeInstances"))?;
        for r in page.reservations() {
            for i in r.instances() {
                let Some(id) = i.instance_id() else { continue };
                let state = i
                    .state()
                    .and_then(|s| s.name())
                    .map(|n| n.as_str().to_string())
                    .unwrap_or_default();
                if state == "terminated" {
                    continue;
                }
                let name = i
                    .tags()
                    .iter()
                    .find(|t| t.key() == Some("Name"))
                    .and_then(|t| t.value())
                    .unwrap_or("")
                    .to_string();
                let info = ssm_status.get(id);
                out.push(Instance {
                    id: id.to_string(),
                    name,
                    state,
                    ssm: info.map(|s| s.status).unwrap_or(SsmStatus::Unmanaged),
                    platform: info
                        .map(|s| s.platform.clone())
                        .filter(|p| !p.is_empty())
                        .or_else(|| i.platform_details().map(str::to_string))
                        .unwrap_or_default(),
                    private_ip: i.private_ip_address().unwrap_or("").to_string(),
                });
            }
        }
    }
    Ok(out)
}

/// An SDK error as the screen shows it: service errors by code and message, transport errors
/// by their chain of causes.
pub(crate) fn sdk_error<E, R>(e: &ec2::error::SdkError<E, R>, action: &str) -> AwsError
where
    E: ProvideErrorMetadata + std::error::Error + 'static,
    R: std::fmt::Debug,
{
    let text = match e.as_service_error() {
        Some(s) => format!(
            "{}: {}",
            s.code().unwrap_or("Erro"),
            s.message().unwrap_or("")
        ),
        None => DisplayErrorContext(e).to_string(),
    };
    let err = classify(&text, Some(action));
    // A throttle that survives the SDK's retries is final for this region only.
    if err.kind == ErrorKind::Throttled {
        return AwsError::new(
            ErrorKind::Throttled,
            "Limite da API atingido nesta região. Tente de novo depois.",
        );
    }
    err
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn limiter_caps_concurrency_and_serializes_each_key() {
        let limiter = Arc::new(Limiter::new(4));
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let per_key: Arc<std::sync::Mutex<HashMap<String, usize>>> = Default::default();
        let key_peak = Arc::new(AtomicUsize::new(0));
        let mut set = tokio::task::JoinSet::new();
        for i in 0..40 {
            let (limiter, running, peak, per_key, key_peak) = (
                limiter.clone(),
                running.clone(),
                peak.clone(),
                per_key.clone(),
                key_peak.clone(),
            );
            set.spawn(async move {
                let key = format!("acct:{}", i % 3);
                let _g = limiter.acquire(&key).await;
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                {
                    let mut m = per_key.lock().unwrap();
                    let c = m.entry(key.clone()).or_default();
                    *c += 1;
                    key_peak.fetch_max(*c, Ordering::SeqCst);
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
                per_key.lock().unwrap().entry(key).and_modify(|c| *c -= 1);
                running.fetch_sub(1, Ordering::SeqCst);
            });
        }
        while set.join_next().await.is_some() {}
        assert!(
            peak.load(Ordering::SeqCst) <= 3,
            "3 keys, one at a time each"
        );
        assert_eq!(
            key_peak.load(Ordering::SeqCst),
            1,
            "never two queries for the same account and region"
        );

        // With more keys than slots, the global cap is what limits.
        let limiter = Arc::new(Limiter::new(4));
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut set = tokio::task::JoinSet::new();
        for i in 0..40 {
            let (limiter, running, peak) = (limiter.clone(), running.clone(), peak.clone());
            set.spawn(async move {
                let _g = limiter.acquire(&format!("k{i}")).await;
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(5)).await;
                running.fetch_sub(1, Ordering::SeqCst);
            });
        }
        while set.join_next().await.is_some() {}
        assert_eq!(peak.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn cancel_stops_waiting_work() {
        let cancel = Cancel::default();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            c2.cancel();
        });
        let r = cancel
            .run(tokio::time::sleep(Duration::from_secs(30)))
            .await;
        assert!(r.is_none());
        assert!(cancel.is_cancelled());
    }

    #[test]
    fn errors_survive_the_cache() {
        let e = AwsError::new(
            ErrorKind::AccessDenied,
            "Falta a permissão ec2:DescribeInstances.",
        );
        let back: AwsError = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!((back.kind, back.message), (e.kind, e.message));
    }
}
