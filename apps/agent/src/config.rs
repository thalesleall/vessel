use std::path::PathBuf;

use anyhow::{Context, bail};
use vessel_protocol::SignupMode;

/// Agent configuration, read from `VESSEL_*` environment variables.
#[derive(Clone, Debug)]
pub struct Config {
    /// `VESSEL_BIND`, default `0.0.0.0:8080`.
    pub bind: String,
    /// `VESSEL_PUBLIC_URL`, e.g. `https://vessel.example.com`. Used in invite links and as the
    /// OPAQUE server identity, so changing it invalidates every password: pick it once.
    pub public_url: String,
    /// `VESSEL_DATABASE`, default `/data/vessel.db`. `:memory:` for an ephemeral server.
    pub database: PathBuf,
    /// `VESSEL_SIGNUPS`: `open`, `invite-only` (default) or `disabled`. The first account can
    /// always register and becomes server admin.
    pub signups: SignupMode,
    /// `VESSEL_NAME`, shown in the app.
    pub name: String,
    /// `VESSEL_TRUST_PROXY`: how many reverse proxies stand in front of the agent, each appending
    /// to `X-Forwarded-For`. `true` or `1` for Caddy alone; `2` for a CDN or load balancer in
    /// front of Caddy. 0 (default) ignores the header.
    pub trust_proxy: usize,
    /// `VESSEL_AUTH_RATE_PER_MIN`, default 30. Auth requests per client IP per minute.
    pub auth_rate_per_min: u32,
    /// `VESSEL_BOOTSTRAP_TOKEN`: code the first account must present, so whoever reaches a
    /// fresh server first cannot become its admin. Generated and logged at start when unset.
    /// `None` only in tests.
    pub bootstrap_token: Option<String>,
    /// Whether `bootstrap_token` was generated (and must be shown in the log).
    pub bootstrap_generated: bool,
    /// `VESSEL_VAULT_INVITE_SIGNUPS`, default `true`: a vault invite also admits a new account
    /// on an invite-only server. Set `false` on a hosted service where accounts are sold, so
    /// only server invites (or open sign-ups) create accounts.
    pub vault_invite_signups: bool,
    /// `VESSEL_QUOTA_MB`: default storage per account (the vaults it owns), in MiB. 0 (default)
    /// is unlimited; the operator can set a different limit per account.
    pub quota_bytes: u64,
    /// `VESSEL_MAX_VAULTS`: shared vaults an account may own. 0 (default) is unlimited.
    pub max_vaults: u32,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let signups = match var("VESSEL_SIGNUPS").as_deref().unwrap_or("invite-only") {
            "open" => SignupMode::Open,
            "invite-only" => SignupMode::InviteOnly,
            "disabled" => SignupMode::Disabled,
            other => bail!("VESSEL_SIGNUPS must be open, invite-only or disabled, got {other:?}"),
        };
        let public_url = var("VESSEL_PUBLIC_URL").context("VESSEL_PUBLIC_URL is required")?;
        let (bootstrap_token, bootstrap_generated) = match var("VESSEL_BOOTSTRAP_TOKEN") {
            Some(t) if t.trim().len() >= 16 => (t.trim().to_string(), false),
            Some(_) => bail!("VESSEL_BOOTSTRAP_TOKEN must have at least 16 characters"),
            None => (
                crate::auth::new_token().map_err(|e| anyhow::anyhow!("{e:?}"))?,
                true,
            ),
        };
        Ok(Self {
            bind: var("VESSEL_BIND").unwrap_or_else(|| "0.0.0.0:8080".into()),
            public_url: public_url.trim_end_matches('/').to_string(),
            database: var("VESSEL_DATABASE")
                .unwrap_or_else(|| "/data/vessel.db".into())
                .into(),
            signups,
            name: var("VESSEL_NAME").unwrap_or_else(|| "Vessel".into()),
            trust_proxy: match var("VESSEL_TRUST_PROXY").as_deref() {
                None | Some("false") => 0,
                Some("true") => 1,
                Some(n) => n
                    .parse()
                    .context("VESSEL_TRUST_PROXY must be true, false or a number of proxies")?,
            },
            auth_rate_per_min: var("VESSEL_AUTH_RATE_PER_MIN")
                .map(|v| v.parse())
                .transpose()
                .context("VESSEL_AUTH_RATE_PER_MIN")?
                .unwrap_or(30),
            bootstrap_token: Some(bootstrap_token),
            bootstrap_generated,
            vault_invite_signups: var("VESSEL_VAULT_INVITE_SIGNUPS")
                .is_none_or(|v| v != "false" && v != "0"),
            quota_bytes: var("VESSEL_QUOTA_MB")
                .map(|v| v.parse::<u64>())
                .transpose()
                .context("VESSEL_QUOTA_MB")?
                .unwrap_or(0)
                .saturating_mul(1024 * 1024),
            max_vaults: var("VESSEL_MAX_VAULTS")
                .map(|v| v.parse())
                .transpose()
                .context("VESSEL_MAX_VAULTS")?
                .unwrap_or(0),
        })
    }

    /// Ephemeral, in-memory configuration with open sign-ups.
    pub fn for_tests() -> Self {
        Self {
            bind: "127.0.0.1:0".into(),
            public_url: "http://vessel.test".into(),
            database: ":memory:".into(),
            signups: SignupMode::Open,
            name: "Vessel test".into(),
            trust_proxy: 0,
            auth_rate_per_min: 10_000,
            bootstrap_token: None,
            bootstrap_generated: false,
            vault_invite_signups: true,
            quota_bytes: 0,
            max_vaults: 0,
        }
    }

    /// The OPAQUE server identity: the host part of the public URL.
    pub fn server_id(&self) -> String {
        let without_scheme = self
            .public_url
            .split("://")
            .nth(1)
            .unwrap_or(&self.public_url);
        without_scheme
            .split('/')
            .next()
            .unwrap_or(without_scheme)
            .to_lowercase()
    }
}
