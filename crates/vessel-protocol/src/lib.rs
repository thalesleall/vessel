//! Wire types of the Vessel sync API (v1). See `docs/ARCHITECTURE.md` §9–§10.
//!
//! Binary values travel as unpadded base64url strings ([`Bytes`]). Every type here is safe to
//! show to the server: plaintext user data never appears in this crate.

use core::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

pub use vessel_crypto::grant::Grant;
pub use vessel_crypto::identity::UserPublicKeys;
pub use vessel_crypto::kdf::KdfParams;
pub use vessel_crypto::membership::{Action, Role, SignedEvent};

pub const API_VERSION: u32 = 1;

/// Largest accepted record envelope.
pub const MAX_BLOB_LEN: usize = 256 * 1024;
/// Largest accepted push batch, and default/maximum pull page.
pub const MAX_BATCH: usize = 500;

// -------------------------------------------------------------------------------------------------
// Primitives
// -------------------------------------------------------------------------------------------------

/// Opaque bytes, serialized as unpadded base64url.
#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub struct Bytes(pub Vec<u8>);

impl Bytes {
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl From<Vec<u8>> for Bytes {
    fn from(v: Vec<u8>) -> Self {
        Self(v)
    }
}

impl From<&[u8]> for Bytes {
    fn from(v: &[u8]) -> Self {
        Self(v.to_vec())
    }
}

impl fmt::Debug for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Bytes({} bytes)", self.0.len())
    }
}

impl Serialize for Bytes {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&URL_SAFE_NO_PAD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        URL_SAFE_NO_PAD
            .decode(s.as_bytes())
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

/// Error body returned with every non-2xx response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    TooLarge,
    SignupsClosed,
    InvalidInvite,
    RateLimited,
    Internal,
}

// -------------------------------------------------------------------------------------------------
// Server
// -------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SignupMode {
    Open,
    InviteOnly,
    Disabled,
}

/// `GET /v1/server/info`. The app fetches this when the user types a server URL.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    pub api_version: u32,
    /// OPAQUE server identity. Bound into every login transcript.
    pub server_id: String,
    pub signups: SignupMode,
    /// Whether nobody has registered yet (the first account becomes server admin).
    pub first_user: bool,
}

// -------------------------------------------------------------------------------------------------
// Auth
// -------------------------------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewDevice {
    pub name: String,
    /// Ed25519 public key that signs this device's refresh requests.
    pub public_key: Bytes,
}

/// Public and wrapped key material of an account (see `vessel_crypto::account`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountKeys {
    pub public_keys: UserPublicKeys,
    pub wrapped_account_key_master: Bytes,
    pub wrapped_account_key_recovery: Bytes,
    pub encrypted_private_keys: Bytes,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegisterStartRequest {
    pub email: String,
    pub request: Bytes,
    #[serde(default)]
    pub invite_token: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegisterStartResponse {
    pub registration_id: Uuid,
    pub user_id: Uuid,
    pub response: Bytes,
    pub kdf: KdfParams,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegisterFinishRequest {
    pub registration_id: Uuid,
    pub upload: Bytes,
    pub account: AccountKeys,
    /// `SHA-256` is applied by the server; this is `RecoveryKey::auth_token()`.
    pub recovery_auth_token: Bytes,
    pub personal_vault: CreateVaultRequest,
    pub device: NewDevice,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoginStartRequest {
    pub email: String,
    pub request: Bytes,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoginStartResponse {
    pub login_id: Uuid,
    pub response: Bytes,
    pub kdf: KdfParams,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoginFinishRequest {
    pub login_id: Uuid,
    pub finalization: Bytes,
    pub device: NewDevice,
}

/// Successful sign-up, login or recovery.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthResponse {
    pub user_id: Uuid,
    pub device_id: Uuid,
    pub access_token: String,
    /// Unix seconds.
    pub access_expires_at: i64,
    pub refresh_token: String,
    pub account: AccountKeys,
    pub kdf: KdfParams,
}

/// `POST /v1/auth/refresh`. The signature proves the request comes from the registered device.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RefreshRequest {
    pub device_id: Uuid,
    pub refresh_token: String,
    pub timestamp: i64,
    pub signature: Bytes,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RefreshResponse {
    pub access_token: String,
    pub access_expires_at: i64,
    pub refresh_token: String,
}

/// Bytes a device signs to refresh its session.
pub fn refresh_signing_bytes(device_id: &Uuid, refresh_token: &str, timestamp: i64) -> Vec<u8> {
    [
        b"vessel/v1/refresh".as_slice(),
        device_id.as_bytes(),
        &timestamp.to_be_bytes(),
        refresh_token.as_bytes(),
    ]
    .concat()
}

/// Maximum clock skew accepted on signed refresh requests, in seconds.
pub const REFRESH_SKEW_SECS: i64 = 300;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PasswordChangeStartRequest {
    /// OPAQUE registration request for the new password.
    pub request: Bytes,
    /// OPAQUE login request with the current password: a stolen access token alone cannot
    /// replace the password.
    pub login_request: Bytes,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PasswordChangeStartResponse {
    pub registration_id: Uuid,
    pub response: Bytes,
    pub kdf: KdfParams,
    pub login_response: Bytes,
    pub login_kdf: KdfParams,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PasswordChangeFinishRequest {
    pub registration_id: Uuid,
    pub upload: Bytes,
    pub wrapped_account_key_master: Bytes,
    /// Completes the login started with `login_request`.
    pub login_finalization: Bytes,
}

/// Forgotten password: prove possession of the recovery key, then re-register OPAQUE.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryStartRequest {
    pub email: String,
    pub recovery_auth_token: Bytes,
    pub request: Bytes,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryStartResponse {
    pub registration_id: Uuid,
    pub response: Bytes,
    pub kdf: KdfParams,
    pub account: AccountKeys,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryFinishRequest {
    pub registration_id: Uuid,
    pub upload: Bytes,
    pub wrapped_account_key_master: Bytes,
    pub device: NewDevice,
}

/// `GET /v1/account`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountResponse {
    pub user_id: Uuid,
    pub email: String,
    pub is_server_admin: bool,
    pub account: AccountKeys,
    pub kdf: KdfParams,
}

// -------------------------------------------------------------------------------------------------
// Devices and users
// -------------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: Uuid,
    pub name: String,
    pub created_at: i64,
    pub last_seen_at: i64,
    pub current: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserPublic {
    pub user_id: Uuid,
    pub email: String,
    pub public_keys: UserPublicKeys,
}

// -------------------------------------------------------------------------------------------------
// Vaults, membership, grants, invites
// -------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VaultState {
    Active,
    Rotating,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateVaultRequest {
    pub vault_id: Uuid,
    pub personal: bool,
    /// Vault metadata (its name) sealed as a record whose id is the vault id.
    pub meta: Bytes,
    pub creation_event: SignedEvent,
    pub grant: Grant,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultSummary {
    pub vault_id: Uuid,
    pub personal: bool,
    pub role: Role,
    pub current_key_version: u32,
    pub state: VaultState,
    pub meta: Bytes,
    pub meta_key_version: u32,
    pub last_seq: u64,
    /// A member left or was removed since the last key rotation started: a manager must rotate
    /// so data written from now on uses a key the former member never held.
    #[serde(default)]
    pub rotation_due: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingMember {
    pub user: UserPublic,
    pub role: Role,
    pub invited_by: Uuid,
    pub created_at: i64,
}

/// `GET /v1/vaults/{id}/membership`: everything a client needs to verify who is in a vault.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MembershipResponse {
    pub events: Vec<SignedEvent>,
    /// Public keys of every user that appears in `events` or `pending`.
    pub users: Vec<UserPublic>,
    pub pending: Vec<PendingMember>,
}

/// `POST /v1/vaults/{id}/membership`: append one signed event. `Add` must carry a grant for the
/// new member for every live key version.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MembershipChangeRequest {
    pub event: SignedEvent,
    #[serde(default)]
    pub grants: Vec<Grant>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateInviteRequest {
    pub role: Role,
    /// Restrict the invite to one address.
    #[serde(default)]
    pub email: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InviteResponse {
    pub token: String,
    pub url: String,
    pub expires_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AcceptInviteResponse {
    /// `None` for a server sign-up invite.
    pub vault_id: Option<Uuid>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GrantsResponse {
    pub grants: Vec<Grant>,
    /// Public keys of the granters, to verify the grants.
    pub granters: Vec<UserPublic>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RotateStartRequest {
    pub new_key_version: u32,
    /// One grant per current member, for the new key version.
    pub grants: Vec<Grant>,
    pub meta: Bytes,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UpdateVaultMetaRequest {
    pub meta: Bytes,
    pub meta_key_version: u32,
}

// -------------------------------------------------------------------------------------------------
// Records and sync
// -------------------------------------------------------------------------------------------------

/// A record as the agent sees it: identity, versioning and an envelope it cannot read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordDto {
    pub id: Uuid,
    pub version: u64,
    pub seq: u64,
    pub key_version: u32,
    pub deleted: bool,
    pub blob: Bytes,
    pub updated_at: i64,
    pub updated_by_device: Uuid,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChangesPage {
    pub changes: Vec<RecordDto>,
    pub next_seq: u64,
    pub has_more: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PushOp {
    pub id: Uuid,
    /// Version the client edited; `0` creates the record.
    pub base_version: u64,
    pub key_version: u32,
    pub deleted: bool,
    pub blob: Bytes,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PushRequest {
    pub ops: Vec<PushOp>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PushResult {
    Ok { id: Uuid, version: u64, seq: u64 },
    Conflict { id: Uuid, current: RecordDto },
    Rejected { id: Uuid, reason: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PushResponse {
    pub results: Vec<PushResult>,
}

// -------------------------------------------------------------------------------------------------
// Real-time events (WebSocket `/v1/events`)
// -------------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    /// Records changed; pull from your cursor.
    VaultChanged { vault_id: Uuid, seq: u64 },
    /// Membership, grants, metadata or key version changed; refresh the vault.
    VaultUpdated { vault_id: Uuid },
    /// You lost access to a vault.
    VaultRemoved { vault_id: Uuid },
    /// This device was revoked; drop local session data.
    DeviceRevoked { device_id: Uuid },
}

// -------------------------------------------------------------------------------------------------
// Helpers
// -------------------------------------------------------------------------------------------------

/// Normalizes an e-mail address for use as an identifier: trimmed and lower-cased.
pub fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_are_base64url() {
        let json = serde_json::to_string(&Bytes(vec![0xfb, 0xff, 0x01])).unwrap();
        assert_eq!(json, "\"-_8B\"");
        let back: Bytes = serde_json::from_str(&json).unwrap();
        assert_eq!(back.0, vec![0xfb, 0xff, 0x01]);
        assert!(serde_json::from_str::<Bytes>("\"***\"").is_err());
    }

    #[test]
    fn push_result_is_tagged() {
        let r = PushResult::Rejected {
            id: Uuid::nil(),
            reason: "x".into(),
        };
        let v: serde_json::Value = serde_json::to_value(&r).unwrap();
        assert_eq!(v["status"], "rejected");
    }
}
