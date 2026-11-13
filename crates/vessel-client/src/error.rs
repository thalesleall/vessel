use vessel_protocol::ErrorCode;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("{message}")]
    Api {
        status: u16,
        code: ErrorCode,
        message: String,
    },
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("websocket error: {0}")]
    WebSocket(#[from] Box<tokio_tungstenite::tungstenite::Error>),
    #[error("cryptographic failure: {0}")]
    Crypto(#[from] vessel_crypto::CryptoError),
    #[error("local database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not signed in")]
    NotSignedIn,
    /// The feature needs a sync server and this device is in local-only mode.
    #[error("this needs a sync server; connect one in Settings to use it")]
    LocalMode,
    #[error("the vault is locked")]
    Locked,
    #[error("wrong password")]
    WrongPassword,
    #[error("the public keys of {email} changed; verify their identity before trusting them again")]
    KeyChanged { email: String },
    #[error("{0}")]
    Invalid(String),
}

impl ClientError {
    pub fn invalid(m: impl Into<String>) -> Self {
        Self::Invalid(m.into())
    }

    /// Whether the failure is a missing network or server, as opposed to a real error.
    pub fn is_offline(&self) -> bool {
        matches!(self, Self::Http(e) if e.is_connect() || e.is_timeout())
    }

    pub fn is_unauthorized(&self) -> bool {
        matches!(
            self,
            Self::Api {
                code: ErrorCode::Unauthorized,
                ..
            }
        )
    }
}

pub type Result<T> = std::result::Result<T, ClientError>;
