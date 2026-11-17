//! The inventory against a fake AWS: an HTTP server that answers like EC2 (query protocol, XML)
//! and Session Manager (JSON 1.1), per region, with pagination, throttling, access errors and a
//! region that never answers.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use super::cli::{AwsError, Credentials, ErrorKind};
use super::inventory::{self, Cancel, Job, Limits, ProfileNode, Progress, RegionNode, SsmStatus};

#[derive(Default)]
struct Fake {
    in_flight: AtomicUsize,
    peak: AtomicUsize,
    calls: Mutex<HashMap<String, usize>>,
}

const BIG: usize = 5000;

fn region_of(headers: &HeaderMap) -> String {
    // SigV4: Credential=AKID/20261117/us-east-1/ec2/aws4_request
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    auth.split("Credential=")
        .nth(1)
        .and_then(|c| c.split('/').nth(2))
        .unwrap_or("?")
        .to_string()
}

fn form(body: &[u8]) -> HashMap<String, String> {
    String::from_utf8_lossy(body)
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_string(), v.replace("%3D", "=").replace('+', " ")))
        .collect()
}

fn ec2_error(status: StatusCode, code: &str, message: &str) -> Response {
    let body = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><Response><Errors><Error><Code>{code}</Code><Message>{message}</Message></Error></Errors><RequestID>req</RequestID></Response>"#
    );
    (status, [("content-type", "text/xml")], body).into_response()
}

fn ssm_error(code: &str, message: &str) -> Response {
    let body = serde_json::json!({ "__type": code, "message": message }).to_string();
    (
        StatusCode::BAD_REQUEST,
        [("content-type", "application/x-amz-json-1.1")],
        body,
    )
        .into_response()
}

fn instance_count(region: &str) -> usize {
    match region {
        "us-east-1" => BIG,
        "sa-east-1" => 3,
        "eu-west-1" => 2,
        _ => 0,
    }
}

async fn handle(fake: Arc<Fake>, headers: HeaderMap, body: Bytes) -> Response {
    let now = fake.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    fake.peak.fetch_max(now, Ordering::SeqCst);
    let resp = answer(&fake, &headers, &body).await;
    fake.in_flight.fetch_sub(1, Ordering::SeqCst);
    resp
}

async fn answer(fake: &Fake, headers: &HeaderMap, body: &[u8]) -> Response {
    let region = region_of(headers);
    if region == "ap-south-1" {
        // Never answers in time: only a cancel ends it.
        tokio::time::sleep(Duration::from_secs(600)).await;
    }
    let target = headers
        .get("x-amz-target")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if target.ends_with("DescribeInstanceInformation") {
        if region == "eu-west-1" {
            return ssm_error(
                "AccessDeniedException",
                "User: arn:aws:sts::222222222222:assumed-role/R/x is not authorized to perform: ssm:DescribeInstanceInformation",
            );
        }
        let req: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
        let start: usize = req["NextToken"]
            .as_str()
            .and_then(|t| t.parse().ok())
            .unwrap_or(0);
        // Every other instance is registered; a quarter of those are offline.
        let total = instance_count(&region);
        let ids: Vec<usize> = (0..total).filter(|i| i % 2 == 0).collect();
        let page: Vec<_> = ids
            .iter()
            .skip(start)
            .take(50)
            .map(|i| {
                serde_json::json!({
                    "InstanceId": format!("i-{i:08x}"),
                    "PingStatus": if i % 8 == 0 { "ConnectionLost" } else { "Online" },
                    "PlatformName": "Amazon Linux",
                    "PlatformVersion": "2023",
                })
            })
            .collect();
        let mut out = serde_json::json!({ "InstanceInformationList": page });
        if start + 50 < ids.len() {
            out["NextToken"] = serde_json::Value::String((start + 50).to_string());
        }
        if region == "sa-east-1" {
            // A hybrid node, registered with Session Manager but not in EC2.
            out["InstanceInformationList"].as_array_mut().unwrap().push(serde_json::json!({
                "InstanceId": "mi-0123456789abcdef0", "PingStatus": "Online", "ComputerName": "onprem-box",
                "PlatformName": "Ubuntu", "PlatformVersion": "24.04", "IPAddress": "192.168.1.5",
            }));
        }
        return (
            StatusCode::OK,
            [("content-type", "application/x-amz-json-1.1")],
            out.to_string(),
        )
            .into_response();
    }

    let f = form(body);
    let action = f.get("Action").cloned().unwrap_or_default();
    {
        let mut calls = fake.calls.lock().unwrap();
        *calls.entry(format!("{region}:{action}")).or_default() += 1;
    }
    match action.as_str() {
        "DescribeRegions" => {
            let items: String = ["us-east-1", "sa-east-1", "eu-west-1"]
                .iter()
                .map(|r| format!("<item><regionName>{r}</regionName><regionEndpoint>ec2.{r}.amazonaws.com</regionEndpoint><optInStatus>opt-in-not-required</optInStatus></item>"))
                .collect();
            let body = format!(
                r#"<DescribeRegionsResponse xmlns="http://ec2.amazonaws.com/doc/2016-11-15/"><requestId>r</requestId><regionInfo>{items}</regionInfo></DescribeRegionsResponse>"#
            );
            (StatusCode::OK, [("content-type", "text/xml")], body).into_response()
        }
        "DescribeInstances" => {
            if region == "sa-east-1" {
                let n = fake.calls.lock().unwrap()[&format!("{region}:{action}")];
                if n <= 2 {
                    return ec2_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "RequestLimitExceeded",
                        "Request limit exceeded.",
                    );
                }
            }
            let start: usize = f.get("NextToken").and_then(|t| t.parse().ok()).unwrap_or(0);
            let total = instance_count(&region);
            let mut items = String::new();
            for i in (start..total).take(1000) {
                let state = if i % 3 == 0 { "stopped" } else { "running" };
                items.push_str(&format!(
                    "<item><instanceId>i-{i:08x}</instanceId><instanceState><code>16</code><name>{state}</name></instanceState><privateIpAddress>10.0.{}.{}</privateIpAddress><platformDetails>Linux/UNIX</platformDetails><tagSet><item><key>Name</key><value>web-{i}</value></item></tagSet></item>",
                    i / 256 % 256,
                    i % 256
                ));
            }
            // One terminated instance, which the inventory leaves out.
            if start == 0 && total > 0 {
                items.push_str("<item><instanceId>i-deadbeef00</instanceId><instanceState><code>48</code><name>terminated</name></instanceState></item>");
            }
            let next = if start + 1000 < total {
                format!("<nextToken>{}</nextToken>", start + 1000)
            } else {
                String::new()
            };
            let body = format!(
                r#"<DescribeInstancesResponse xmlns="http://ec2.amazonaws.com/doc/2016-11-15/"><requestId>r</requestId><reservationSet><item><reservationId>r-1</reservationId><instancesSet>{items}</instancesSet></item></reservationSet>{next}</DescribeInstancesResponse>"#
            );
            (StatusCode::OK, [("content-type", "text/xml")], body).into_response()
        }
        _ => ec2_error(StatusCode::BAD_REQUEST, "InvalidAction", "unknown"),
    }
}

async fn serve(fake: Arc<Fake>) -> String {
    let app = axum::Router::new()
        .fallback(move |headers: HeaderMap, body: Bytes| handle(fake.clone(), headers, body));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

struct FixedCreds;

impl inventory::CredentialSource for FixedCreds {
    fn credentials(
        &self,
        profile: &str,
    ) -> impl std::future::Future<Output = Result<Credentials, AwsError>> + Send {
        let denied = profile == "denied";
        async move {
            if denied {
                return Err(AwsError::new(
                    ErrorKind::AssumeRoleDenied,
                    "Sem permissão para assumir arn:aws:iam::333333333333:role/X.",
                ));
            }
            Ok(serde_json::from_str(r#"{"AccessKeyId":"AKIDEXAMPLE","SecretAccessKey":"secret","SessionToken":"token"}"#).unwrap())
        }
    }
}

#[derive(Default, Clone)]
struct Collect {
    progress: Arc<Mutex<Vec<Progress>>>,
    regions: Arc<Mutex<Vec<(String, String)>>>,
    profiles: Arc<Mutex<Vec<String>>>,
}

impl inventory::Sink for Collect {
    fn progress(&self, p: &Progress) {
        self.progress.lock().unwrap().push(p.clone());
    }
    fn profile(&self, node: &ProfileNode) {
        self.profiles.lock().unwrap().push(node.profile.clone());
    }
    fn region(&self, profile: &str, node: &RegionNode) {
        self.regions
            .lock()
            .unwrap()
            .push((profile.into(), node.region.clone()));
    }
}

fn job(profile: &str, regions: Option<Vec<&str>>) -> Job {
    Job {
        profile: profile.into(),
        account: Some("222222222222".into()),
        role_arn: None,
        home_region: "us-east-1".into(),
        regions: regions.map(|r| r.into_iter().map(str::to_string).collect()),
        alternates: Vec::new(),
    }
}

fn limits(endpoint: &str) -> Limits {
    Limits {
        endpoint: Some(endpoint.into()),
        initial_backoff: Duration::from_millis(20),
        // Low here, so the test sees the cap hold.
        concurrency: 4,
        ..Limits::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn maps_paginates_retries_and_reports_errors_per_region() {
    let fake = Arc::new(Fake::default());
    let url = serve(fake.clone()).await;
    let sink = Collect::default();
    let inv = inventory::map(
        vec![job("prod", None), job("denied", None)],
        FixedCreds,
        sink.clone(),
        limits(&url),
        Cancel::default(),
        inventory::shared_http_client(),
    )
    .await;

    assert!(!inv.partial);
    let prod = inv.profiles.iter().find(|p| p.profile == "prod").unwrap();
    assert!(prod.error.is_none());
    let regions: Vec<_> = prod.regions.iter().map(|r| r.region.as_str()).collect();
    assert_eq!(
        regions,
        ["eu-west-1", "sa-east-1", "us-east-1"],
        "from DescribeRegions, sorted"
    );

    // 5000 instances over 5 EC2 pages and 50 SSM pages; terminated ones left out.
    let us = prod
        .regions
        .iter()
        .find(|r| r.region == "us-east-1")
        .unwrap();
    assert!(us.error.is_none());
    assert_eq!(us.instances.len(), BIG);
    let i0 = us.instances.iter().find(|i| i.id == "i-00000000").unwrap();
    assert_eq!(
        (i0.name.as_str(), i0.state.as_str(), i0.ssm),
        ("web-0", "stopped", SsmStatus::Offline)
    );
    let i2 = us.instances.iter().find(|i| i.id == "i-00000002").unwrap();
    assert_eq!(
        (i2.ssm, i2.platform.as_str(), i2.private_ip.as_str()),
        (SsmStatus::Online, "Amazon Linux 2023", "10.0.0.2")
    );
    let i1 = us.instances.iter().find(|i| i.id == "i-00000001").unwrap();
    assert_eq!(
        (i1.ssm, i1.platform.as_str()),
        (SsmStatus::Unmanaged, "Linux/UNIX")
    );
    assert!(!us.instances.iter().any(|i| i.id == "i-deadbeef00"));
    assert_eq!(fake.calls.lock().unwrap()["us-east-1:DescribeInstances"], 5);

    // Throttled twice, then the adaptive retry gets through.
    let sa = prod
        .regions
        .iter()
        .find(|r| r.region == "sa-east-1")
        .unwrap();
    assert!(sa.error.is_none(), "{:?}", sa.error);
    assert_eq!(sa.instances.len(), 4, "3 EC2 instances and one hybrid node");
    let hybrid = sa
        .instances
        .iter()
        .find(|i| i.id.starts_with("mi-"))
        .unwrap();
    assert_eq!(
        (
            hybrid.name.as_str(),
            hybrid.private_ip.as_str(),
            hybrid.state.as_str()
        ),
        ("onprem-box", "192.168.1.5", "")
    );
    assert_eq!(fake.calls.lock().unwrap()["sa-east-1:DescribeInstances"], 3);

    // An access error stays on its region; the others are mapped.
    let eu = prod
        .regions
        .iter()
        .find(|r| r.region == "eu-west-1")
        .unwrap();
    let e = eu.error.as_ref().unwrap();
    assert_eq!(
        (e.kind, e.message.as_str()),
        (
            ErrorKind::AccessDenied,
            "Falta a permissão ssm:DescribeInstanceInformation."
        )
    );

    // A profile that cannot sign in fails alone.
    let denied = inv.profiles.iter().find(|p| p.profile == "denied").unwrap();
    assert_eq!(
        denied.error.as_ref().unwrap().kind,
        ErrorKind::AssumeRoleDenied
    );
    assert!(denied.regions.is_empty());

    // Never more than 4 queries at once, and results arrived region by region.
    assert!(
        fake.peak.load(Ordering::SeqCst) <= 4,
        "peak {}",
        fake.peak.load(Ordering::SeqCst)
    );
    assert_eq!(sink.regions.lock().unwrap().len(), 3);
    let last = sink.progress.lock().unwrap().last().cloned().unwrap();
    assert_eq!(
        (
            last.accounts_done,
            last.accounts_total,
            last.regions_done,
            last.regions_total
        ),
        (2, 2, 3, 3)
    );

    // The cached form stays small: about a hundred bytes per instance.
    let bytes = serde_json::to_vec(&inv).unwrap().len();
    assert!(bytes < BIG * 160, "{bytes} bytes for {BIG} instances");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_ends_a_stuck_mapping() {
    let fake = Arc::new(Fake::default());
    let url = serve(fake.clone()).await;
    let cancel = Cancel::default();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        c2.cancel();
    });
    let started = std::time::Instant::now();
    let inv = inventory::map(
        vec![job("prod", Some(vec!["ap-south-1", "sa-east-1"]))],
        FixedCreds,
        Collect::default(),
        limits(&url),
        cancel,
        inventory::shared_http_client(),
    )
    .await;
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "cancel returns promptly"
    );
    assert!(inv.partial);
    let prod = &inv.profiles[0];
    let stuck = prod
        .regions
        .iter()
        .find(|r| r.region == "ap-south-1")
        .unwrap();
    assert_eq!(stuck.error.as_ref().unwrap().kind, ErrorKind::Cancelled);
}

// -------------------------------------------------------------------------------------------
// Scale: hundreds of accounts, every enabled region.
// -------------------------------------------------------------------------------------------

/// Answers every account with 17 regions and one instance per region, after a short delay, and
/// records how many calls run at once overall and for the same account, region and service.
#[derive(Default)]
struct Wide {
    in_flight: AtomicUsize,
    peak: AtomicUsize,
    calls: AtomicUsize,
    per_key: Mutex<HashMap<String, usize>>,
    key_peak: AtomicUsize,
}

const WIDE_REGIONS: [&str; 17] = [
    "ap-northeast-1",
    "ap-northeast-2",
    "ap-northeast-3",
    "ap-south-1",
    "ap-southeast-1",
    "ap-southeast-2",
    "ca-central-1",
    "eu-central-1",
    "eu-north-1",
    "eu-west-1",
    "eu-west-2",
    "eu-west-3",
    "sa-east-1",
    "us-east-1",
    "us-east-2",
    "us-west-1",
    "us-west-2",
];

async fn wide(fake: Arc<Wide>, headers: HeaderMap, body: Bytes) -> Response {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let scope: Vec<&str> = auth
        .split("Credential=")
        .nth(1)
        .unwrap_or("")
        .split('/')
        .collect();
    let key = format!(
        "{}:{}:{}",
        scope.first().unwrap_or(&""),
        scope.get(2).unwrap_or(&""),
        scope.get(3).unwrap_or(&"")
    );
    let now = fake.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    fake.peak.fetch_max(now, Ordering::SeqCst);
    fake.calls.fetch_add(1, Ordering::SeqCst);
    {
        let mut m = fake.per_key.lock().unwrap();
        let c = m.entry(key.clone()).or_default();
        *c += 1;
        fake.key_peak.fetch_max(*c, Ordering::SeqCst);
    }
    tokio::time::sleep(Duration::from_millis(15)).await;
    let region = scope.get(2).unwrap_or(&"").to_string();
    let target = headers
        .get("x-amz-target")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let resp = if target.ends_with("DescribeInstanceInformation") {
        let body = serde_json::json!({ "InstanceInformationList": [{ "InstanceId": "i-00000001", "PingStatus": "Online" }] }).to_string();
        (
            StatusCode::OK,
            [("content-type", "application/x-amz-json-1.1")],
            body,
        )
            .into_response()
    } else if form(&body).get("Action").map(String::as_str) == Some("DescribeRegions") {
        let items: String = WIDE_REGIONS
            .iter()
            .map(|r| format!("<item><regionName>{r}</regionName></item>"))
            .collect();
        let b = format!(
            r#"<DescribeRegionsResponse xmlns="http://ec2.amazonaws.com/doc/2016-11-15/"><requestId>r</requestId><regionInfo>{items}</regionInfo></DescribeRegionsResponse>"#
        );
        (StatusCode::OK, [("content-type", "text/xml")], b).into_response()
    } else {
        let b = format!(
            r#"<DescribeInstancesResponse xmlns="http://ec2.amazonaws.com/doc/2016-11-15/"><requestId>r</requestId><reservationSet><item><reservationId>r-1</reservationId><instancesSet><item><instanceId>i-00000001</instanceId><instanceState><code>16</code><name>running</name></instanceState><tagSet><item><key>Name</key><value>{region}</value></item></tagSet></item></instancesSet></item></reservationSet></DescribeInstancesResponse>"#
        );
        (StatusCode::OK, [("content-type", "text/xml")], b).into_response()
    };
    fake.per_key
        .lock()
        .unwrap()
        .entry(key)
        .and_modify(|c| *c -= 1);
    fake.in_flight.fetch_sub(1, Ordering::SeqCst);
    resp
}

/// Each account signs with its own key id, so the fake can tell accounts apart.
struct PerAccountCreds;

impl inventory::CredentialSource for PerAccountCreds {
    fn credentials(
        &self,
        profile: &str,
    ) -> impl std::future::Future<Output = Result<Credentials, AwsError>> + Send {
        let id = format!("AKID{}", profile.replace('-', ""));
        async move {
            Ok(serde_json::from_value(serde_json::json!({ "AccessKeyId": id, "SecretAccessKey": "s", "SessionToken": "t" })).unwrap())
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn maps_hundreds_of_accounts_quickly() {
    const ACCOUNTS: usize = 250;
    let fake = Arc::new(Wide::default());
    let f2 = fake.clone();
    let app = axum::Router::new().fallback(move |h: HeaderMap, b: Bytes| wide(f2.clone(), h, b));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let jobs: Vec<Job> = (0..ACCOUNTS)
        .map(|i| Job {
            profile: format!("acct-{i:03}"),
            account: Some(format!("{:012}", 100_000_000_000u64 + i as u64)),
            role_arn: None,
            home_region: "us-east-1".into(),
            regions: None,
            alternates: Vec::new(),
        })
        .collect();
    let started = std::time::Instant::now();
    let inv = inventory::map(
        jobs,
        PerAccountCreds,
        Collect::default(),
        Limits {
            endpoint: Some(url),
            ..Limits::default()
        },
        Cancel::default(),
        inventory::shared_http_client(),
    )
    .await;
    let took = started.elapsed();

    assert_eq!(inv.instance_count(), ACCOUNTS * WIDE_REGIONS.len());
    assert!(
        inv.profiles
            .iter()
            .all(|p| p.error.is_none() && p.regions.iter().all(|r| r.error.is_none()))
    );
    // One DescribeRegions per account, then one SSM and one EC2 call per region.
    assert_eq!(
        fake.calls.load(Ordering::SeqCst),
        ACCOUNTS * (1 + 2 * WIDE_REGIONS.len())
    );
    assert_eq!(
        fake.key_peak.load(Ordering::SeqCst),
        1,
        "never two calls at once for the same account, region and service"
    );
    let peak = fake.peak.load(Ordering::SeqCst);
    assert!(peak <= Limits::default().concurrency, "peak {peak}");
    // 8,750 calls of 15 ms: one at a time would take over two minutes.
    assert!(
        took < Duration::from_secs(30),
        "took {took:?} with peak {peak}"
    );
    eprintln!(
        "{ACCOUNTS} accounts, {} calls, peak {peak}, {took:?}",
        fake.calls.load(Ordering::SeqCst)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn assumes_roles_without_the_cli() {
    use axum::response::IntoResponse;
    let app = axum::Router::new().fallback(|body: Bytes| async move {
        let f = form(&body);
        assert_eq!(f.get("Action").map(String::as_str), Some("AssumeRole"));
        let role = f.get("RoleArn").cloned().unwrap_or_default().replace("%3A", ":").replace("%2F", "/");
        if role.contains("Denied") {
            let b = format!("<ErrorResponse><Error><Type>Sender</Type><Code>AccessDenied</Code><Message>User: x is not authorized to perform: sts:AssumeRole on resource: {role}</Message></Error><RequestId>r</RequestId></ErrorResponse>");
            return (StatusCode::FORBIDDEN, [("content-type", "text/xml")], b).into_response();
        }
        assert_eq!(f.get("ExternalId").map(String::as_str), Some("ext-1"));
        let b = "<AssumeRoleResponse xmlns=\"https://sts.amazonaws.com/doc/2011-06-15/\"><AssumeRoleResult><Credentials><AccessKeyId>ASIAOK</AccessKeyId><SecretAccessKey>s</SecretAccessKey><SessionToken>t</SessionToken><Expiration>2030-01-01T00:00:00Z</Expiration></Credentials></AssumeRoleResult><ResponseMetadata><RequestId>r</RequestId></ResponseMetadata></AssumeRoleResponse>";
        (StatusCode::OK, [("content-type", "text/xml")], b).into_response()
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let profiles = super::config::parse(
        "[profile root]\nregion = us-east-1\n[profile ok]\nrole_arn = arn:aws:iam::222222222222:role/Admin\nsource_profile = root\nexternal_id = ext-1\n[profile no]\nrole_arn = arn:aws:iam::333333333333:role/Denied\nsource_profile = root\n",
    );
    let from: Credentials = serde_json::from_str(
        r#"{"AccessKeyId":"AKIDROOT","SecretAccessKey":"s","SessionToken":"t"}"#,
    )
    .unwrap();
    let http = inventory::shared_http_client();
    let get = |n: &str| profiles.iter().find(|p| p.name == n).unwrap();
    let ok = super::creds::assume(http.clone(), Some(&url), &from, get("ok"), "us-east-1")
        .await
        .unwrap();
    assert_eq!(ok.access_key_id, "ASIAOK");
    let no = super::creds::assume(http, Some(&url), &from, get("no"), "us-east-1")
        .await
        .unwrap_err();
    assert_eq!(
        (no.kind, no.message.as_str()),
        (
            ErrorKind::AssumeRoleDenied,
            "Sem permissão para assumir arn:aws:iam::333333333333:role/Denied."
        )
    );
}
