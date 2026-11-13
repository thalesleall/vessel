use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OptionalExtension};
use tokio::sync::broadcast;
use uuid::Uuid;
use vessel_crypto::opaque::{ServerLoginState, ServerSetup};
use vessel_protocol::ServerEvent;

use crate::config::Config;
use crate::db;
use crate::error::{AppError, AppResult};

/// Pending OPAQUE flows expire after this many seconds.
pub const FLOW_TTL_SECS: i64 = 120;
/// Most pending flows of each kind held at once: beyond it new flows are refused instead of
/// growing memory without bound.
pub const MAX_PENDING_FLOWS: usize = 20_000;
/// Most rate-limit buckets kept; beyond it (after dropping stale ones) attempts are refused.
const MAX_RATE_BUCKETS: usize = 200_000;
/// Attempts per account (e-mail or user) per minute, whatever the source address.
pub const ACCOUNT_RATE_PER_MIN: u32 = 20;
/// Open event sockets per user.
pub const MAX_SOCKETS_PER_USER: usize = 16;

/// What a rate limit counts.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum RateKey {
    /// An IPv4 address, or the /64 of an IPv6 one (a single host usually owns a whole /64).
    Net([u8; 16]),
    /// An account, whatever addresses the attempts come from.
    Account(Vec<u8>),
}

impl RateKey {
    pub fn ip(ip: IpAddr) -> Self {
        match ip {
            IpAddr::V4(v4) => Self::Net(v4.to_ipv6_mapped().octets()),
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => Self::Net(v4.to_ipv6_mapped().octets()),
                None => {
                    let mut o = v6.octets();
                    o[8..].fill(0);
                    Self::Net(o)
                }
            },
        }
    }

    pub fn account(id: &str) -> Self {
        use sha2::{Digest, Sha256};
        Self::Account(Sha256::digest(id.as_bytes()).to_vec())
    }
}

/// What an OPAQUE registration is for.
pub enum Registration {
    NewAccount {
        email: String,
        user_id: Uuid,
        invite_token: Option<String>,
    },
    PasswordChange {
        user_id: Uuid,
        /// Login with the current password, finished together with the registration.
        login: ServerLoginState,
    },
    Recovery {
        user_id: Uuid,
    },
}

pub struct PendingRegistration {
    pub kind: Registration,
    pub expires_at: i64,
}

pub struct PendingLogin {
    pub state: ServerLoginState,
    pub user_id: Option<Uuid>,
    pub expires_at: i64,
}

/// A real-time event addressed to a set of users.
#[derive(Clone, Debug)]
pub struct Notification {
    pub users: Vec<Uuid>,
    pub event: ServerEvent,
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    db: Arc<Mutex<Connection>>,
    pub setup: Arc<ServerSetup>,
    pub registrations: Arc<Mutex<HashMap<Uuid, PendingRegistration>>>,
    pub logins: Arc<Mutex<HashMap<Uuid, PendingLogin>>>,
    pub events: broadcast::Sender<Notification>,
    rate: Arc<Mutex<HashMap<RateKey, (i64, u32)>>>,
    sockets: Arc<Mutex<HashMap<Uuid, usize>>>,
}

/// Holds one of a user's event sockets; released on drop.
pub struct SocketSlot {
    sockets: Arc<Mutex<HashMap<Uuid, usize>>>,
    user: Uuid,
}

impl Drop for SocketSlot {
    fn drop(&mut self) {
        if let Ok(mut map) = self.sockets.lock()
            && let Some(n) = map.get_mut(&self.user)
        {
            *n -= 1;
            if *n == 0 {
                map.remove(&self.user);
            }
        }
    }
}

fn busy() -> AppError {
    AppError::new(
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        vessel_protocol::ErrorCode::RateLimited,
        "the server is busy, try again in a minute",
    )
}

impl AppState {
    pub fn open(config: Config) -> anyhow::Result<Self> {
        let conn = db::open(&config.database)?;
        let setup = load_or_create_setup(&conn)?;
        if config.bootstrap_generated
            && db::user_count(&conn).map_err(|e| anyhow::anyhow!("{e:?}"))? == 0
            && let Some(code) = &config.bootstrap_token
        {
            tracing::warn!(
                "no account yet: the first account (the server admin) must enter this installation code: {code}                  (set VESSEL_BOOTSTRAP_TOKEN to choose it)"
            );
        }
        let (events, _) = broadcast::channel(1024);
        Ok(Self {
            config: Arc::new(config),
            db: Arc::new(Mutex::new(conn)),
            setup: Arc::new(setup),
            registrations: Arc::default(),
            logins: Arc::default(),
            events,
            rate: Arc::default(),
            sockets: Arc::default(),
        })
    }

    /// Runs a closure against the database on the blocking pool.
    pub async fn db<T, F>(&self, f: F) -> AppResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> AppResult<T> + Send + 'static,
    {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = db
                .lock()
                .map_err(|_| AppError::internal("database lock poisoned"))?;
            f(&mut conn)
        })
        .await?
    }

    pub fn notify(&self, users: Vec<Uuid>, event: ServerEvent) {
        if !users.is_empty() {
            let _ = self.events.send(Notification { users, event });
        }
    }

    /// Fixed-window rate limit for authentication endpoints, per network (see [`RateKey::ip`]).
    pub fn check_rate(&self, ip: IpAddr) -> AppResult<()> {
        self.check_rate_key(RateKey::ip(ip), self.config.auth_rate_per_min)
    }

    /// Rate limit per account: spreading attempts over many addresses does not help.
    pub fn check_account_rate(&self, account: &str) -> AppResult<()> {
        self.check_rate_key(RateKey::account(account), ACCOUNT_RATE_PER_MIN)
    }

    fn check_rate_key(&self, key: RateKey, limit: u32) -> AppResult<()> {
        let now = db::now();
        let mut rate = self
            .rate
            .lock()
            .map_err(|_| AppError::internal("rate lock poisoned"))?;
        if rate.len() >= MAX_RATE_BUCKETS / 2 {
            rate.retain(|_, (start, _)| now - *start < 60);
        }
        if rate.len() >= MAX_RATE_BUCKETS && !rate.contains_key(&key) {
            return Err(busy());
        }
        let entry = rate.entry(key).or_insert((now, 0));
        if now - entry.0 >= 60 {
            *entry = (now, 0);
        }
        entry.1 = entry.1.saturating_add(1);
        if entry.1 > limit {
            return Err(AppError::new(
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                vessel_protocol::ErrorCode::RateLimited,
                "too many attempts, try again in a minute",
            ));
        }
        Ok(())
    }

    /// Reserves one of the user's event sockets, if under [`MAX_SOCKETS_PER_USER`].
    pub fn socket_slot(&self, user: Uuid) -> AppResult<SocketSlot> {
        let mut map = self
            .sockets
            .lock()
            .map_err(|_| AppError::internal("lock poisoned"))?;
        let n = map.entry(user).or_insert(0);
        if *n >= MAX_SOCKETS_PER_USER {
            return Err(busy());
        }
        *n += 1;
        Ok(SocketSlot {
            sockets: self.sockets.clone(),
            user,
        })
    }

    pub fn put_registration(&self, kind: Registration) -> AppResult<Uuid> {
        // Flow ids are bearer secrets until the flow finishes: fully random, not time-ordered.
        let id = Uuid::new_v4();
        let mut map = self
            .registrations
            .lock()
            .map_err(|_| AppError::internal("lock poisoned"))?;
        let now = db::now();
        if map.len() >= MAX_PENDING_FLOWS / 2 {
            map.retain(|_, p| p.expires_at > now);
        }
        if map.len() >= MAX_PENDING_FLOWS {
            return Err(busy());
        }
        map.insert(
            id,
            PendingRegistration {
                kind,
                expires_at: now + FLOW_TTL_SECS,
            },
        );
        Ok(id)
    }

    pub fn take_registration(&self, id: &Uuid) -> AppResult<Registration> {
        let mut map = self
            .registrations
            .lock()
            .map_err(|_| AppError::internal("lock poisoned"))?;
        match map.remove(id) {
            Some(p) if p.expires_at > db::now() => Ok(p.kind),
            _ => Err(AppError::bad_request("registration expired, start again")),
        }
    }

    pub fn put_login(&self, state: ServerLoginState, user_id: Option<Uuid>) -> AppResult<Uuid> {
        let id = Uuid::new_v4();
        let mut map = self
            .logins
            .lock()
            .map_err(|_| AppError::internal("lock poisoned"))?;
        let now = db::now();
        if map.len() >= MAX_PENDING_FLOWS / 2 {
            map.retain(|_, p| p.expires_at > now);
        }
        if map.len() >= MAX_PENDING_FLOWS {
            return Err(busy());
        }
        map.insert(
            id,
            PendingLogin {
                state,
                user_id,
                expires_at: now + FLOW_TTL_SECS,
            },
        );
        Ok(id)
    }

    pub fn take_login(&self, id: &Uuid) -> AppResult<PendingLogin> {
        let mut map = self
            .logins
            .lock()
            .map_err(|_| AppError::internal("lock poisoned"))?;
        match map.remove(id) {
            Some(p) if p.expires_at > db::now() => Ok(p),
            _ => Err(AppError::bad_request("login expired, start again")),
        }
    }
}

fn load_or_create_setup(conn: &Connection) -> anyhow::Result<ServerSetup> {
    let stored: Option<Vec<u8>> = conn
        .query_row(
            "SELECT value FROM server_meta WHERE key = 'opaque_setup'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(bytes) = stored {
        return ServerSetup::from_bytes(&bytes)
            .map_err(|e| anyhow::anyhow!("stored OPAQUE setup: {e}"));
    }
    let setup = ServerSetup::generate();
    conn.execute(
        "INSERT INTO server_meta (key, value) VALUES ('opaque_setup', ?1)",
        [setup.to_bytes().as_slice()],
    )?;
    Ok(setup)
}

#[cfg(test)]
mod tests {
    use super::RateKey;

    #[test]
    fn rate_buckets_group_an_ipv6_slash_64() {
        let a = RateKey::ip("2001:db8:1:2:aaaa::1".parse().unwrap());
        let b = RateKey::ip("2001:db8:1:2:ffff:1:2:3".parse().unwrap());
        let c = RateKey::ip("2001:db8:1:3::1".parse().unwrap());
        assert_eq!(a, b);
        assert_ne!(a, c);
        let v4 = RateKey::ip("203.0.113.7".parse().unwrap());
        let mapped = RateKey::ip("::ffff:203.0.113.7".parse().unwrap());
        assert_eq!(v4, mapped);
        assert_ne!(v4, RateKey::ip("203.0.113.8".parse().unwrap()));
    }
}
