//! Vessel client core.
//!
//! [`Vessel`] owns the local store and the connection to an agent. It signs up, logs in and
//! unlocks accounts, keeps an encrypted local copy of every vault, syncs it, and manages vault
//! sharing. All user data is encrypted before it reaches the store or the network.

mod api;
mod error;
pub mod events;
mod items;
mod local;
mod store;
mod sync;
mod vaults;

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use reqwest::Method;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use vessel_crypto::SymmetricKey;
use vessel_crypto::account::{
    AccountKeyMaterial, LocalUnlock, RecoveryKey, UnlockedAccount, create_account,
    create_local_unlock, rewrap_for_master, unlock_local, unlock_with_master, unlock_with_recovery,
};
use vessel_crypto::identity::DeviceKey;
use vessel_crypto::kdf::KdfParams;
use vessel_crypto::opaque::{OpaqueIdentity, client_login_start, client_registration_start};
use vessel_crypto::wrap::{WrapPurpose, open_bytes, seal_bytes};
use vessel_protocol::*;

pub use api::normalize_url;
pub use error::{ClientError, Result};
pub use items::{ConflictView, Item};
pub use local::MigrationReport;
pub use sync::SyncReport;
pub use vaults::{MemberView, MembersView, PendingView, VaultView};
pub use vessel_model as model;
pub use vessel_protocol::{
    AdminUser, AdminUserUpdate, DeviceInfo, ErrorCode, InviteResponse, Role, ServerEvent, ServerInfo,
    ServerStats, SignupMode,
};

use crate::api::Api;
use crate::store::Store;

const PROFILE_KEY: &str = "profile";
const PINS_KEY: &str = "pins";
/// E-mail -> user id of every pinned user: a second account claiming an e-mail already pinned
/// to someone else is refused (a server substituting a person with an account it controls).
const PIN_EMAILS_KEY: &str = "pin_emails";

/// What a signed-in device remembers about its account. Secrets inside are sealed.
#[derive(Clone, Serialize, Deserialize)]
struct Profile {
    server_url: String,
    server_id: String,
    email: String,
    user_id: Uuid,
    device_id: Uuid,
    material: AccountKeyMaterial,
    local_unlock: LocalUnlock,
    /// [`DeviceSecrets`] sealed with the account key.
    device_secrets: Vec<u8>,
    is_server_admin: bool,
    /// Local-only mode: no server; `server_url`/`server_id` are empty.
    #[serde(default)]
    local: bool,
}

#[derive(Serialize, Deserialize)]
struct DeviceSecrets {
    device_seed: Vec<u8>,
    refresh_token: String,
}

struct Unlocked {
    account: UnlockedAccount,
    /// `(vault_id, key_version)` → vault key, from verified grants.
    vault_keys: HashMap<(Uuid, u32), SymmetricKey>,
}

/// Public state of the client, for the UI.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Status {
    pub signed_in: bool,
    pub unlocked: bool,
    pub email: Option<String>,
    pub server_url: Option<String>,
    pub user_id: Option<Uuid>,
    pub device_id: Option<Uuid>,
    pub fingerprint: Option<String>,
    pub is_server_admin: bool,
    /// Local-only mode: everything stays on this computer, nothing syncs.
    pub local: bool,
    /// The saved server is reached over plain http on the network (from before https was
    /// required): tokens and metadata travel in the clear.
    pub insecure_server: bool,
}

pub struct Vessel {
    store: Store,
    api: Option<Api>,
    profile: Option<Profile>,
    unlocked: Option<Unlocked>,
    /// (imported, skipped duplicates) by imports since the last migration started.
    import_stats: (usize, usize),
}

fn account_keys(m: &AccountKeyMaterial) -> AccountKeys {
    AccountKeys {
        public_keys: m.public_keys,
        wrapped_account_key_master: Bytes(m.wrapped_account_key_master.clone()),
        wrapped_account_key_recovery: Bytes(m.wrapped_account_key_recovery.clone()),
        encrypted_private_keys: Bytes(m.encrypted_private_keys.clone()),
    }
}

fn material(k: &AccountKeys) -> AccountKeyMaterial {
    AccountKeyMaterial {
        public_keys: k.public_keys,
        wrapped_account_key_master: k.wrapped_account_key_master.0.clone(),
        wrapped_account_key_recovery: k.wrapped_account_key_recovery.0.clone(),
        encrypted_private_keys: k.encrypted_private_keys.0.clone(),
    }
}

/// Fetches `GET /v1/server/info` from a server URL the user typed.
pub async fn server_info(url: &str) -> Result<ServerInfo> {
    api::require_secure_transport(url)?;
    let api = Api::new(url)?;
    let info: ServerInfo = api
        .public::<(), _>(Method::GET, "/v1/server/info", None)
        .await?;
    if info.api_version != API_VERSION {
        return Err(ClientError::invalid(format!(
            "server speaks API v{}, this app speaks v{API_VERSION}",
            info.api_version
        )));
    }
    Ok(info)
}

impl Vessel {
    pub fn open(path: &Path) -> Result<Self> {
        Self::from_store(Store::open(path)?)
    }

    pub fn in_memory() -> Result<Self> {
        Self::from_store(Store::in_memory()?)
    }

    fn from_store(store: Store) -> Result<Self> {
        let profile: Option<Profile> = store.get(PROFILE_KEY)?;
        let api = profile
            .as_ref()
            .filter(|p| !p.local)
            .map(|p| Api::new(&p.server_url))
            .transpose()?;
        Ok(Self {
            store,
            api,
            profile,
            unlocked: None,
            import_stats: (0, 0),
        })
    }

    pub fn status(&self) -> Status {
        let p = self.profile.as_ref();
        Status {
            signed_in: p.is_some(),
            unlocked: self.unlocked.is_some(),
            email: p.map(|p| p.email.clone()),
            server_url: p.map(|p| p.server_url.clone()),
            user_id: p.map(|p| p.user_id),
            device_id: p.map(|p| p.device_id),
            fingerprint: p.map(|p| p.material.public_keys.fingerprint().to_string()),
            is_server_admin: p.is_some_and(|p| p.is_server_admin),
            local: p.is_some_and(|p| p.local),
            insecure_server: p.is_some_and(|p| {
                !p.local && api::require_secure_transport(&p.server_url).is_err()
            }),
        }
    }

    // -- accessors used by the submodules ----------------------------------------------------

    fn api(&self) -> Result<&Api> {
        if self.is_local() {
            return Err(ClientError::LocalMode);
        }
        self.api.as_ref().ok_or(ClientError::NotSignedIn)
    }

    /// Whether this device runs without a sync server.
    pub fn is_local(&self) -> bool {
        self.profile.as_ref().is_some_and(|p| p.local)
    }

    fn profile(&self) -> Result<&Profile> {
        self.profile.as_ref().ok_or(ClientError::NotSignedIn)
    }

    fn unlocked(&self) -> Result<&Unlocked> {
        self.unlocked.as_ref().ok_or(ClientError::Locked)
    }

    fn user_id(&self) -> Result<Uuid> {
        Ok(self.profile()?.user_id)
    }

    fn pins(&self) -> Result<BTreeMap<Uuid, UserPublicKeys>> {
        Ok(self.store.get(PINS_KEY)?.unwrap_or_default())
    }

    /// Trust-on-first-use pinning of other users' public keys.
    fn check_pin(&self, user: &UserPublic) -> Result<()> {
        let mut emails: BTreeMap<String, Uuid> = self.store.get(PIN_EMAILS_KEY)?.unwrap_or_default();
        let email = normalize_email(&user.email);
        match emails.get(&email) {
            Some(id) if *id != user.user_id => {
                return Err(ClientError::KeyChanged {
                    email: user.email.clone(),
                });
            }
            Some(_) => {}
            None => {
                emails.insert(email, user.user_id);
                self.store.set(PIN_EMAILS_KEY, &emails)?;
            }
        }
        let mut pins = self.pins()?;
        match pins.get(&user.user_id) {
            Some(pinned) if *pinned != user.public_keys => Err(ClientError::KeyChanged {
                email: user.email.clone(),
            }),
            Some(_) => Ok(()),
            None => {
                pins.insert(user.user_id, user.public_keys);
                self.store.set(PINS_KEY, &pins)
            }
        }
    }

    /// Persists the refresh token if the API rotated it.
    async fn persist_session(&mut self) -> Result<()> {
        let Some(refresh) = self.api()?.take_changed_refresh().await else {
            return Ok(());
        };
        let unlocked = self.unlocked()?;
        let mut profile = self.profile()?.clone();
        let secrets_raw = open_bytes(
            &unlocked.account.account_key,
            WrapPurpose::DeviceSecrets,
            &profile.device_secrets,
        )?;
        let mut secrets: DeviceSecrets = serde_json::from_slice(&secrets_raw)?;
        secrets.refresh_token = refresh;
        profile.device_secrets = seal_bytes(
            &unlocked.account.account_key,
            WrapPurpose::DeviceSecrets,
            &serde_json::to_vec(&secrets)?,
        )?;
        self.store.set(PROFILE_KEY, &profile)?;
        self.profile = Some(profile);
        Ok(())
    }

    // -- sign-up, login, recovery -------------------------------------------------------------

    /// Creates an account and signs this device in. Returns the recovery key to show the user.
    pub async fn signup(
        &mut self,
        server_url: &str,
        email: &str,
        password: &str,
        device_name: &str,
        invite_token: Option<String>,
    ) -> Result<String> {
        let info = server_info(server_url).await?;
        let email = normalize_email(email);
        let api = Api::new(server_url)?;
        let identity = OpaqueIdentity {
            user: email.as_bytes(),
            server: info.server_id.as_bytes(),
        };

        let (state, request) = client_registration_start(password.as_bytes())?;
        let start: RegisterStartResponse = api
            .public(
                Method::POST,
                "/v1/auth/register/start",
                Some(&RegisterStartRequest {
                    email: email.clone(),
                    request: request.into(),
                    invite_token,
                }),
            )
            .await?;
        let (upload, master_kek) = state.finish(
            password.as_bytes(),
            start.response.as_slice(),
            &identity,
            &KdfParams::default(),
        )?;
        let (material, account, recovery) = create_account(&master_kek)?;
        let (personal_vault, _vk) =
            vaults::new_vault_request(&account, start.user_id, "Pessoal", true)?;

        let device_key = DeviceKey::generate()?;
        let auth: AuthResponse = api
            .public(
                Method::POST,
                "/v1/auth/register/finish",
                Some(&RegisterFinishRequest {
                    registration_id: start.registration_id,
                    upload: upload.into(),
                    account: account_keys(&material),
                    recovery_auth_token: Bytes(recovery.auth_token()?.to_vec()),
                    personal_vault,
                    device: NewDevice {
                        name: device_name.into(),
                        public_key: Bytes(device_key.public_key().to_vec()),
                    },
                }),
            )
            .await?;
        self.finish_auth(
            api,
            info,
            email,
            password,
            auth,
            account,
            device_key,
            Vec::new(),
        )
        .await?;
        Ok(recovery.to_string())
    }

    /// Signs this device in to an existing account.
    pub async fn login(
        &mut self,
        server_url: &str,
        email: &str,
        password: &str,
        device_name: &str,
    ) -> Result<()> {
        self.login_with(server_url, email, password, device_name, Vec::new())
            .await
    }

    /// Login that also carries `carry` (items from local mode) into the account.
    pub(crate) async fn login_with(
        &mut self,
        server_url: &str,
        email: &str,
        password: &str,
        device_name: &str,
        carry: Vec<(Uuid, vessel_model::Entity)>,
    ) -> Result<()> {
        let info = server_info(server_url).await?;
        let email = normalize_email(email);
        let api = Api::new(server_url)?;
        let identity = OpaqueIdentity {
            user: email.as_bytes(),
            server: info.server_id.as_bytes(),
        };

        let (state, request) = client_login_start(password.as_bytes())?;
        let start: LoginStartResponse = api
            .public(
                Method::POST,
                "/v1/auth/login/start",
                Some(&LoginStartRequest {
                    email: email.clone(),
                    request: request.into(),
                }),
            )
            .await?;
        // The server hands back the account's parameters: never weaker than this client's default.
        start.kdf.validate_account()?;
        let out = state
            .finish(
                password.as_bytes(),
                start.response.as_slice(),
                &identity,
                &start.kdf,
            )
            .map_err(|e| match e {
                vessel_crypto::CryptoError::Opaque => ClientError::WrongPassword,
                other => other.into(),
            })?;
        let device_key = DeviceKey::generate()?;
        let auth: AuthResponse = api
            .public(
                Method::POST,
                "/v1/auth/login/finish",
                Some(&LoginFinishRequest {
                    login_id: start.login_id,
                    finalization: out.finalization.into(),
                    device: NewDevice {
                        name: device_name.into(),
                        public_key: Bytes(device_key.public_key().to_vec()),
                    },
                }),
            )
            .await?;
        let account = unlock_with_master(&material(&auth.account), &out.master_kek)?;
        self.finish_auth(api, info, email, password, auth, account, device_key, carry)
            .await
    }

    /// Resets a forgotten master password with the recovery key and signs this device in.
    /// Every other device is signed out.
    pub async fn recover(
        &mut self,
        server_url: &str,
        email: &str,
        recovery_key: &str,
        new_password: &str,
        device_name: &str,
    ) -> Result<String> {
        let recovery: RecoveryKey = recovery_key.parse()?;
        let info = server_info(server_url).await?;
        let email = normalize_email(email);
        let api = Api::new(server_url)?;
        let identity = OpaqueIdentity {
            user: email.as_bytes(),
            server: info.server_id.as_bytes(),
        };

        let (state, request) = client_registration_start(new_password.as_bytes())?;
        let start: RecoveryStartResponse = api
            .public(
                Method::POST,
                "/v1/auth/recovery/start",
                Some(&RecoveryStartRequest {
                    email: email.clone(),
                    recovery_auth_token: Bytes(recovery.auth_token()?.to_vec()),
                    request: request.into(),
                }),
            )
            .await?;
        let account = unlock_with_recovery(&material(&start.account), &recovery)?;
        let (upload, master_kek) = state.finish(
            new_password.as_bytes(),
            start.response.as_slice(),
            &identity,
            &KdfParams::default(),
        )?;
        let device_key = DeviceKey::generate()?;
        let (wrapped_recovery, new_recovery) = vessel_crypto::account::new_recovery_key(&account)?;
        let auth: AuthResponse = api
            .public(
                Method::POST,
                "/v1/auth/recovery/finish",
                Some(&RecoveryFinishRequest {
                    registration_id: start.registration_id,
                    upload: upload.into(),
                    wrapped_account_key_master: Bytes(rewrap_for_master(&account, &master_kek)?),
                    device: NewDevice {
                        name: device_name.into(),
                        public_key: Bytes(device_key.public_key().to_vec()),
                    },
                    wrapped_account_key_recovery: Bytes(wrapped_recovery),
                    recovery_auth_token: Bytes(new_recovery.auth_token()?.to_vec()),
                }),
            )
            .await?;
        self.finish_auth(
            api,
            info,
            email,
            new_password,
            auth,
            account,
            device_key,
            Vec::new(),
        )
        .await?;
        Ok(new_recovery.to_string())
    }

    #[allow(clippy::too_many_arguments)]
    async fn finish_auth(
        &mut self,
        api: Api,
        info: ServerInfo,
        email: String,
        password: &str,
        auth: AuthResponse,
        account: UnlockedAccount,
        device_key: DeviceKey,
        carry: Vec<(Uuid, vessel_model::Entity)>,
    ) -> Result<()> {
        let material = material(&auth.account);
        if account.keys.public() != material.public_keys {
            return Err(ClientError::invalid(
                "server returned keys that do not match this account",
            ));
        }
        // Signing in again to the same account on the same server keeps what was already
        // verified about other people's keys.
        let kept = match &self.profile {
            Some(p) if p.user_id == auth.user_id && p.server_id == info.server_id => Some((
                self.pins()?,
                self.store
                    .get::<BTreeMap<String, Uuid>>(PIN_EMAILS_KEY)?
                    .unwrap_or_default(),
            )),
            _ => None,
        };
        self.store.wipe()?;
        let secrets = DeviceSecrets {
            device_seed: device_key.seed().to_vec(),
            refresh_token: auth.refresh_token.clone(),
        };
        let profile = Profile {
            server_url: api.base_url().to_string(),
            server_id: info.server_id,
            email: email.clone(),
            user_id: auth.user_id,
            device_id: auth.device_id,
            local_unlock: create_local_unlock(&account, password.as_bytes(), KdfParams::default())?,
            device_secrets: seal_bytes(
                &account.account_key,
                WrapPurpose::DeviceSecrets,
                &serde_json::to_vec(&secrets)?,
            )?,
            material,
            is_server_admin: false,
            local: false,
        };
        api.set_session(
            auth.device_id,
            device_key,
            auth.refresh_token,
            Some((auth.access_token, auth.access_expires_at)),
        )
        .await;
        self.store.set(PROFILE_KEY, &profile)?;
        let (mut pins, emails) = kept.unwrap_or_default();
        pins.insert(auth.user_id, account.keys.public());
        self.store.set(PINS_KEY, &pins)?;
        self.store.set(PIN_EMAILS_KEY, &emails)?;
        self.profile = Some(profile);
        self.api = Some(api);
        self.unlocked = Some(Unlocked {
            account,
            vault_keys: HashMap::new(),
        });
        // Items brought from local mode wait in an encrypted queue until the personal vault
        // arrives; a crash before the import finishes resumes on the next sync.
        self.queue_import(carry)?;

        if let Ok(info) = self.api()?.get::<AccountResponse>("/v1/account").await
            && let Some(p) = self.profile.as_mut()
        {
            p.is_server_admin = info.is_server_admin;
            let p = p.clone();
            self.store.set(PROFILE_KEY, &p)?;
        }
        self.sync().await?;
        Ok(())
    }

    // -- lock / unlock / logout ---------------------------------------------------------------

    /// Unlocks with the master password, offline. Wrong passwords fail with [`ClientError::WrongPassword`].
    pub fn unlock(&mut self, password: &str) -> Result<()> {
        let profile = self.profile()?.clone();
        let account = unlock_local(
            &profile.local_unlock,
            password.as_bytes(),
            &profile.material,
        )
        .map_err(|e| match e {
            vessel_crypto::CryptoError::Decrypt => ClientError::WrongPassword,
            other => other.into(),
        })?;
        let secrets_raw = open_bytes(
            &account.account_key,
            WrapPurpose::DeviceSecrets,
            &profile.device_secrets,
        )?;
        let secrets: DeviceSecrets = serde_json::from_slice(&secrets_raw)?;
        let device_key = DeviceKey::from_seed(&secrets.device_seed)?;
        self.install_unlocked(profile.user_id, account)?;
        if profile.local {
            self.resume_import()?;
            return Ok(());
        }
        let api = self.api.as_ref().ok_or(ClientError::NotSignedIn)?;
        let refresh = secrets.refresh_token;
        // Setting the session is async only because of the mutex; nothing is sent here.
        futures_util::FutureExt::now_or_never(api.set_session(
            profile.device_id,
            device_key,
            refresh,
            None,
        ))
        .ok_or_else(|| ClientError::invalid("session busy"))?;
        Ok(())
    }

    /// Opens the cached vault grants with `account` and keeps both in memory.
    fn install_unlocked(&mut self, user_id: Uuid, account: UnlockedAccount) -> Result<()> {
        let mut unlocked = Unlocked {
            account,
            vault_keys: HashMap::new(),
        };
        for summary in self.store.vaults()? {
            if let Some(cached) = self
                .store
                .vault_blob::<GrantsResponse>(&summary.vault_id, store::VaultColumn::Grants)?
            {
                vaults::open_grants(&mut unlocked, user_id, summary.vault_id, &cached);
            }
        }
        self.unlocked = Some(unlocked);
        Ok(())
    }

    /// Forgets every decrypted key. Local data stays encrypted on disk.
    pub fn lock(&mut self) {
        self.unlocked = None;
        if let Some(api) = &self.api {
            let _ = futures_util::FutureExt::now_or_never(api.clear_session());
        }
    }

    /// Signs out on the server (best effort) and deletes all local data.
    pub async fn logout(&mut self) -> Result<()> {
        if self.unlocked.is_some()
            && let Ok(api) = self.api()
        {
            let _ = api.post::<_, ()>("/v1/auth/logout", &()).await;
        }
        self.forget()
    }

    /// Deletes all local data without contacting the server (e.g. after a device revocation).
    pub fn forget(&mut self) -> Result<()> {
        self.store.wipe()?;
        self.profile = None;
        self.api = None;
        self.unlocked = None;
        Ok(())
    }

    /// Changes the master password. Other devices must log in again.
    pub async fn change_password(
        &mut self,
        current_password: &str,
        new_password: &str,
    ) -> Result<()> {
        let profile = self.profile()?.clone();
        unlock_local(
            &profile.local_unlock,
            current_password.as_bytes(),
            &profile.material,
        )
        .map_err(|_| ClientError::WrongPassword)?;
        if profile.local {
            let account = &self.unlocked()?.account;
            let local =
                create_local_unlock(account, new_password.as_bytes(), KdfParams::default())?;
            let mut profile = profile;
            profile.local_unlock = local;
            self.store.set(PROFILE_KEY, &profile)?;
            self.profile = Some(profile);
            return Ok(());
        }
        let identity = OpaqueIdentity {
            user: profile.email.as_bytes(),
            server: profile.server_id.as_bytes(),
        };
        let (state, request) = client_registration_start(new_password.as_bytes())?;
        // The server wants proof of the current password, not just this session.
        let (login, login_request) = client_login_start(current_password.as_bytes())?;
        let start: PasswordChangeStartResponse = self
            .api()?
            .post(
                "/v1/account/password/start",
                &PasswordChangeStartRequest {
                    request: request.into(),
                    login_request: login_request.into(),
                },
            )
            .await?;
        start.login_kdf.validate_account()?;
        let proof = login
            .finish(
                current_password.as_bytes(),
                start.login_response.as_slice(),
                &identity,
                &start.login_kdf,
            )
            .map_err(|_| ClientError::WrongPassword)?;
        let (upload, master_kek) = state.finish(
            new_password.as_bytes(),
            start.response.as_slice(),
            &identity,
            &KdfParams::default(),
        )?;
        let account = &self.unlocked()?.account;
        let wrapped = rewrap_for_master(account, &master_kek)?;
        let local = create_local_unlock(account, new_password.as_bytes(), KdfParams::default())?;
        self.api()?
            .post::<_, ()>(
                "/v1/account/password/finish",
                &PasswordChangeFinishRequest {
                    registration_id: start.registration_id,
                    upload: upload.into(),
                    wrapped_account_key_master: Bytes(wrapped.clone()),
                    login_finalization: proof.finalization.into(),
                },
            )
            .await?;
        let mut profile = profile;
        profile.material.wrapped_account_key_master = wrapped;
        profile.local_unlock = local;
        self.store.set(PROFILE_KEY, &profile)?;
        self.profile = Some(profile);
        self.persist_session().await
    }

    // -- devices ------------------------------------------------------------------------------

    pub async fn devices(&mut self) -> Result<Vec<DeviceInfo>> {
        let devices = self.api()?.get("/v1/devices").await?;
        self.persist_session().await?;
        Ok(devices)
    }

    pub async fn revoke_device(&mut self, device_id: Uuid) -> Result<()> {
        self.api()?
            .delete::<()>(&format!("/v1/devices/{device_id}"))
            .await?;
        self.persist_session().await
    }

    /// Creates a server sign-up invite (server admins only).
    pub async fn create_signup_invite(&mut self, email: Option<String>) -> Result<InviteResponse> {
        let invite = self
            .api()?
            .post(
                "/v1/admin/invites",
                &CreateInviteRequest {
                    role: Role::Viewer,
                    email,
                },
            )
            .await?;
        self.persist_session().await?;
        Ok(invite)
    }

    // -- account deletion -----------------------------------------------------------------

    /// Deletes the account on the server and everything on this device. Shared vaults owned by
    /// someone else are left first (with signed events, so their members' apps see it); vaults
    /// this account owns are deleted with everything in them.
    pub async fn delete_account(&mut self, password: &str) -> Result<()> {
        let profile = self.profile()?.clone();
        if profile.local {
            return Err(ClientError::LocalMode);
        }
        unlock_local(&profile.local_unlock, password.as_bytes(), &profile.material)
            .map_err(|_| ClientError::WrongPassword)?;
        self.sync().await?;
        let foreign: Vec<Uuid> = self
            .vaults()?
            .into_iter()
            .filter(|v| v.role != Role::Owner)
            .map(|v| v.id)
            .collect();
        for vault in foreign {
            self.leave_vault(vault).await?;
        }
        let identity = OpaqueIdentity {
            user: profile.email.as_bytes(),
            server: profile.server_id.as_bytes(),
        };
        let (login, login_request) = client_login_start(password.as_bytes())?;
        let start: AccountDeleteStartResponse = self
            .api()?
            .post(
                "/v1/account/delete/start",
                &AccountDeleteStartRequest {
                    login_request: login_request.into(),
                },
            )
            .await?;
        start.login_kdf.validate_account()?;
        let proof = login
            .finish(
                password.as_bytes(),
                start.login_response.as_slice(),
                &identity,
                &start.login_kdf,
            )
            .map_err(|_| ClientError::WrongPassword)?;
        self.api()?
            .post::<_, ()>(
                "/v1/account/delete/finish",
                &AccountDeleteFinishRequest {
                    deletion_id: start.deletion_id,
                    login_finalization: proof.finalization.into(),
                },
            )
            .await?;
        self.forget()
    }

    // -- server administration (server admins only) ------------------------------------------

    pub async fn admin_users(&mut self) -> Result<Vec<AdminUser>> {
        let users = self.api()?.get("/v1/admin/users").await?;
        self.persist_session().await?;
        Ok(users)
    }

    pub async fn admin_update_user(&mut self, user_id: Uuid, update: &AdminUserUpdate) -> Result<()> {
        self.api()?
            .patch::<_, ()>(&format!("/v1/admin/users/{user_id}"), update)
            .await?;
        self.persist_session().await
    }

    pub async fn admin_delete_user(&mut self, user_id: Uuid) -> Result<()> {
        self.api()?
            .delete::<()>(&format!("/v1/admin/users/{user_id}"))
            .await?;
        self.persist_session().await
    }

    pub async fn admin_stats(&mut self) -> Result<ServerStats> {
        let stats = self.api()?.get("/v1/admin/stats").await?;
        self.persist_session().await?;
        Ok(stats)
    }

    /// Server URL and a fresh access token, for [`events::listen`].
    pub async fn event_credentials(&mut self) -> Result<(String, String)> {
        self.unlocked()?;
        let token = self.api()?.access_token().await?;
        self.persist_session().await?;
        Ok((self.profile()?.server_url.clone(), token))
    }

    /// The user's own fingerprint, to read out when someone verifies you.
    pub fn fingerprint(&self) -> Result<String> {
        Ok(self
            .profile()?
            .material
            .public_keys
            .fingerprint()
            .to_string())
    }
}
