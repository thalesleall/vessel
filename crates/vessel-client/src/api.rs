//! HTTP client for the agent API, with automatic, device-signed session refresh.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::Mutex;
use uuid::Uuid;
use vessel_crypto::identity::DeviceKey;
use vessel_protocol::{
    ApiError, Bytes, ErrorCode, RefreshRequest, RefreshResponse, refresh_signing_bytes,
};

use crate::error::{ClientError, Result};

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Default)]
struct Session {
    device_id: Option<Uuid>,
    device_key: Option<DeviceKey>,
    access: Option<String>,
    access_expires: i64,
    refresh: Option<String>,
    /// Set when the refresh token rotated and must be persisted.
    refresh_changed: bool,
}

pub struct Api {
    http: reqwest::Client,
    base: String,
    session: Mutex<Session>,
}

impl Api {
    pub fn new(base_url: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("vessel/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self {
            http,
            base: normalize_url(base_url),
            session: Mutex::new(Session::default()),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    pub async fn set_session(
        &self,
        device_id: Uuid,
        device_key: DeviceKey,
        refresh: String,
        access: Option<(String, i64)>,
    ) {
        let mut s = self.session.lock().await;
        s.device_id = Some(device_id);
        s.device_key = Some(device_key);
        s.refresh = Some(refresh);
        (s.access, s.access_expires) = access.map_or((None, 0), |(a, e)| (Some(a), e));
        s.refresh_changed = false;
    }

    pub async fn clear_session(&self) {
        *self.session.lock().await = Session::default();
    }

    /// Returns the rotated refresh token once, if it changed since the last call.
    pub async fn take_changed_refresh(&self) -> Option<String> {
        let mut s = self.session.lock().await;
        if s.refresh_changed {
            s.refresh_changed = false;
            s.refresh.clone()
        } else {
            None
        }
    }

    /// A valid access token, refreshing it with the device key when needed.
    pub async fn access_token(&self) -> Result<String> {
        let mut s = self.session.lock().await;
        if let Some(access) = &s.access
            && s.access_expires > now() + 30
        {
            return Ok(access.clone());
        }
        self.refresh_locked(&mut s).await
    }

    async fn refresh_locked(&self, s: &mut Session) -> Result<String> {
        let (Some(device_id), Some(device_key), Some(refresh)) =
            (s.device_id, &s.device_key, s.refresh.clone())
        else {
            return Err(ClientError::NotSignedIn);
        };
        let timestamp = now();
        let signature = device_key.sign(&refresh_signing_bytes(&device_id, &refresh, timestamp));
        let req = RefreshRequest {
            device_id,
            refresh_token: refresh,
            timestamp,
            signature: Bytes(signature.to_vec()),
        };
        let resp: RefreshResponse = self
            .send(Method::POST, "/v1/auth/refresh", Some(&req), None)
            .await?;
        s.access = Some(resp.access_token.clone());
        s.access_expires = resp.access_expires_at;
        s.refresh = Some(resp.refresh_token);
        s.refresh_changed = true;
        Ok(resp.access_token)
    }

    async fn send<B: Serialize, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        token: Option<&str>,
    ) -> Result<T> {
        let mut req = self.http.request(method, format!("{}{path}", self.base));
        if let Some(token) = token {
            req = req.bearer_auth(token);
        }
        if let Some(body) = body {
            req = req.json(body);
        }
        let resp = req.send().await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp.json().await?);
        }
        let text = resp.text().await.unwrap_or_default();
        let (code, message) = match serde_json::from_str::<ApiError>(&text) {
            Ok(e) => (e.code, e.message),
            Err(_) => (
                if status == StatusCode::UNAUTHORIZED {
                    ErrorCode::Unauthorized
                } else {
                    ErrorCode::Internal
                },
                format!("server returned {status}"),
            ),
        };
        Err(ClientError::Api {
            status: status.as_u16(),
            code,
            message,
        })
    }

    /// Unauthenticated request.
    pub async fn public<B: Serialize, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<T> {
        self.send(method, path, body, None).await
    }

    /// Authenticated request; retries once after refreshing if the access token was rejected.
    pub async fn call<B: Serialize, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<T> {
        let token = self.access_token().await?;
        match self.send(method.clone(), path, body, Some(&token)).await {
            Err(e) if e.is_unauthorized() => {
                let token = {
                    let mut s = self.session.lock().await;
                    s.access = None;
                    self.refresh_locked(&mut s).await?
                };
                self.send(method, path, body, Some(&token)).await
            }
            other => other,
        }
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.call::<(), T>(Method::GET, path, None).await
    }

    pub async fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        self.call(Method::POST, path, Some(body)).await
    }

    pub async fn put<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        self.call(Method::PUT, path, Some(body)).await
    }

    pub async fn delete<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.call::<(), T>(Method::DELETE, path, None).await
    }
}

/// `https://Host:443/` → `https://host:443`; adds `https://` when no scheme is given.
pub fn normalize_url(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    if url.starts_with("http://") || url.starts_with("https://") {
        url.to_string()
    } else {
        format!("https://{url}")
    }
}
