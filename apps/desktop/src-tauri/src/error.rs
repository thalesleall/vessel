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
            ClientError::Api { code: vessel_client::ErrorCode::Suspended, .. } => {
                "Esta conta foi suspensa pelo operador do servidor.".to_string()
            }
            ClientError::Api { code: vessel_client::ErrorCode::QuotaExceeded, message, .. } => {
                if message.contains("vaults") {
                    "Esta conta chegou ao limite de cofres compartilhados.".to_string()
                } else if message.contains("invites") {
                    "Há convites abertos demais neste cofre. Espere alguns expirarem.".to_string()
                } else {
                    "O espaço do dono do cofre acabou. As alterações ficam guardadas neste computador até haver espaço.".to_string()
                }
            }
            ClientError::Api { code: vessel_client::ErrorCode::InvalidInvite, .. } => {
                "Convite ou código de instalação inválido.".to_string()
            }
            ClientError::Api { message, .. } => server_message_pt(message)
                .map_or_else(|| message.clone(), str::to_string),
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

/// Portuguese for the agent's messages that reach people (the agent speaks English to every
/// client; protocol-level messages that only a bug would show stay as they are).
fn server_message_pt(message: &str) -> Option<&'static str> {
    Some(match message {
        "a rotation is already in progress" => "Já há uma troca de chave em andamento neste cofre.",
        "admins only" => "Só administradores do cofre podem fazer isso.",
        "an account with this e-mail already exists" => "Já existe uma conta com este e-mail.",
        "authentication required" => "Sua sessão expirou. Entre de novo.",
        "cannot invite an owner" => "Não dá para convidar alguém como dono.",
        "clock skew too large; check the system time" => "O relógio deste computador está muito diferente do servidor. Acerte a data e a hora.",
        "delete your own account from your account settings" => "Para excluir a sua própria conta, use Configurações → Conta.",
        "device not found" => "Dispositivo não encontrado.",
        "finish the key rotation first" => "Termine a troca de chave do cofre antes.",
        "invalid e-mail address" => "E-mail inválido.",
        "invite is invalid or expired" => "Convite inválido ou expirado.",
        "leave the shared vaults you do not own first" => "Saia antes dos cofres compartilhados de outras pessoas.",
        "login expired, start again" | "registration expired, start again" => "A operação demorou demais. Tente de novo.",
        "make another account server admin first" => "Torne outra conta administradora do servidor antes de excluir a sua.",
        "no such account" => "Conta não encontrada.",
        "only the owner can delete a shared vault" => "Só o dono pode excluir um cofre compartilhado.",
        "only the owner can invite admins" => "Só o dono pode convidar administradores.",
        "read-only access to this vault" => "Você só tem acesso de leitura a este cofre.",
        "server admin only" => "Só administradores do servidor podem fazer isso.",
        "sign-ups require an invite" => "Este servidor só aceita contas novas com convite.",
        "the first account needs the installation code from the server log" => "A primeira conta precisa do código de instalação que aparece no log do servidor.",
        "the personal vault cannot be shared" => "O cofre pessoal não pode ser compartilhado.",
        "the role is higher than the one in the invite" => "O papel é maior que o do convite.",
        "the server is busy, try again in a minute" => "O servidor está ocupado. Tente de novo em um minuto.",
        "the server needs at least one admin" => "O servidor precisa de pelo menos um administrador.",
        "the user has not accepted an invite to this vault" => "Essa pessoa ainda não aceitou um convite para este cofre.",
        "this vault's membership history is full" => "O histórico de membros deste cofre chegou ao limite.",
        "too many attempts, try again in a minute" => "Tentativas demais. Espere um minuto.",
        "vault not found" => "Cofre não encontrado.",
        "you are already a member of this vault" => "Você já é membro deste cofre.",
        "you cannot suspend your own account" => "Você não pode suspender a sua própria conta.",
        _ => return None,
    })
}
