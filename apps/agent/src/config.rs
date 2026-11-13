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
