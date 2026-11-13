use serde::Serialize;
use serde_json::Value;
use vessel_client::ClientError;

/// Error returned to the UI (and to MCP tools): a stable `code`, a human message in Portuguese
/// and optional structured details.
#[derive(Debug, Clone, Serialize)]
pub struct CmdError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

pub type CmdResult<T> = Result<T, CmdError>;

impl CmdError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

impl std::fmt::Display for CmdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<ClientError> for CmdError {
    fn from(e: ClientError) -> Self {
        let code = match &e {
            ClientError::Locked => "locked",
            ClientError::NotSignedIn => "signed_out",
            ClientError::LocalMode => "local_mode",
            ClientError::WrongPassword => "wrong_password",
            ClientError::KeyChanged { .. } => "key_changed",
            ClientError::Api { .. } if e.is_unauthorized() => "unauthorized",
            ClientError::Api { .. } => "server",
            ClientError::Http(_) if e.is_offline() => "offline",
            ClientError::Http(_) | ClientError::WebSocket(_) => "network",
            _ => "error",
        };
        let message = match &e {
            ClientError::Locked => "O Vessel está bloqueado.".to_string(),
            ClientError::NotSignedIn => "Nenhuma conta conectada.".to_string(),
            ClientError::LocalMode => {
                "Isso precisa de um servidor de sincronização. Conecte um em Configurações."
                    .to_string()
            }
            ClientError::WrongPassword => "Senha incorreta.".to_string(),
            ClientError::KeyChanged { email } => {
                format!(
                    "As chaves de {email} mudaram. Confirme a identidade dessa pessoa antes de continuar."
                )
            }
            ClientError::Http(_) if e.is_offline() => {
                "Servidor inacessível. Você está offline?".to_string()
            }
            other => other.to_string(),
        };
        Self::new(code, message)
    }
}

impl From<String> for CmdError {
    fn from(m: String) -> Self {
        Self::new("error", m)
    }
}

impl From<&str> for CmdError {
    fn from(m: &str) -> Self {
        Self::new("error", m)
    }
}

impl From<russh::Error> for CmdError {
    fn from(e: russh::Error) -> Self {
        Self::new("ssh", format!("SSH: {e}"))
    }
}

impl From<std::io::Error> for CmdError {
    fn from(e: std::io::Error) -> Self {
        Self::new("io", e.to_string())
    }
}

impl From<serde_json::Error> for CmdError {
    fn from(e: serde_json::Error) -> Self {
        Self::new("invalid", format!("dados inválidos: {e}"))
    }
}
