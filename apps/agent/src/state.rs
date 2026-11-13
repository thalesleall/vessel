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

/// What an OPAQUE registration is for.
pub enum Registration {
    NewAccount {
        email: String,
        user_id: Uuid,
        invite_token: Option<String>,
    },
    PasswordChange {
        user_id: Uuid,
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
    rate: Arc<Mutex<HashMap<IpAddr, (i64, u32)>>>,
}

impl AppState {
    pub fn open(config: Config) -> anyhow::Result<Self> {
        let conn = db::open(&config.database)?;
        let setup = load_or_create_setup(&conn)?;
        let (events, _) = broadcast::channel(1024);
        Ok(Self {
            config: Arc::new(config),
            db: Arc::new(Mutex::new(conn)),
            setup: Arc::new(setup),
            registrations: Arc::default(),
            logins: Arc::default(),
            events,
            rate: Arc::default(),
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

    /// Fixed-window rate limit for authentication endpoints.
    pub fn check_rate(&self, ip: IpAddr) -> AppResult<()> {
        let now = db::now();
        let mut rate = self
            .rate
            .lock()
            .map_err(|_| AppError::internal("rate lock poisoned"))?;
        if rate.len() > 50_000 {
            rate.retain(|_, (start, _)| now - *start < 60);
        }
        let entry = rate.entry(ip).or_insert((now, 0));
        if now - entry.0 >= 60 {
            *entry = (now, 0);
        }
        entry.1 += 1;
        if entry.1 > self.config.auth_rate_per_min {
            return Err(AppError::new(
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                vessel_protocol::ErrorCode::RateLimited,
                "too many attempts, try again in a minute",
            ));
        }
        Ok(())
    }

    pub fn put_registration(&self, kind: Registration) -> AppResult<Uuid> {
        let id = Uuid::now_v7();
        let mut map = self
            .registrations
            .lock()
            .map_err(|_| AppError::internal("lock poisoned"))?;
        let now = db::now();
        map.retain(|_, p| p.expires_at > now);
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
        let id = Uuid::now_v7();
        let mut map = self
            .logins
            .lock()
            .map_err(|_| AppError::internal("lock poisoned"))?;
        let now = db::now();
        map.retain(|_, p| p.expires_at > now);
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
