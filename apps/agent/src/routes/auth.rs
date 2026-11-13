//! OPAQUE sign-up, login, password change and recovery; session refresh and logout.

use std::net::SocketAddr;

use axum::Json;
use axum::extract::{ConnectInfo, State};
use axum::http::HeaderMap;
use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;
use vessel_crypto::account::recovery_auth_hash;
use vessel_crypto::identity::verify_ed25519;
use vessel_crypto::opaque::{
    OpaqueIdentity, server_login_start, server_registration_finish, server_registration_start,
};
use vessel_protocol::*;

use super::client_ip;
use super::vaults::insert_new_vault;
use crate::auth::{self, Auth, create_device, create_session, hash_token, rotate_session};
use crate::db::{self, now, user_by_email, user_by_id, uuid_from};
use crate::error::{AppError, AppResult};
use crate::state::{AppState, Registration};

type Peer = ConnectInfo<SocketAddr>;

fn auth_response(
    conn: &rusqlite::Connection,
    user_id: Uuid,
    device: &NewDevice,
) -> AppResult<AuthResponse> {
    let user = user_by_id(conn, &user_id)?.ok_or_else(AppError::unauthorized)?;
    let device_id = create_device(conn, &user_id, device)?;
    let tokens = create_session(conn, &user_id, &device_id)?;
    Ok(AuthResponse {
        user_id,
        device_id,
        access_token: tokens.access,
        access_expires_at: tokens.access_expires,
        refresh_token: tokens.refresh,
        account: user.account,
        kdf: user.kdf,
    })
}

fn validate_email(email: &str) -> AppResult<String> {
    let email = normalize_email(email);
    let valid = email.len() <= 254
        && email
            .split_once('@')
            .is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.'));
    if !valid {
        return Err(AppError::bad_request("invalid e-mail address"));
    }
    Ok(email)
}

/// Checks whether a sign-up is allowed. Returns the invite token hash to consume, if any.
fn signup_allowed(
    conn: &rusqlite::Connection,
    mode: SignupMode,
    email: &str,
    invite: Option<&str>,
) -> AppResult<Option<Vec<u8>>> {
    if db::user_count(conn)? == 0 {
        return Ok(None);
    }
    if let Some(token) = invite {
        let hash = hash_token(token);
        let row: Option<(Option<Vec<u8>>, Option<String>, i64, Option<Vec<u8>>)> = conn
            .query_row(
                "SELECT vault_id, email, expires_at, accepted_by FROM invites WHERE token_hash = ?1",
                [&hash],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        if let Some((vault, restricted, expires, accepted)) = row {
            let email_ok = restricted.is_none_or(|e| e == email);
            if vault.is_none()
                && accepted.is_none()
                && expires > now()
                && email_ok
                && mode != SignupMode::Disabled
            {
                return Ok(Some(hash));
            }
            // A vault invite also admits its invitee as a new user, once: the token is consumed
            // by the sign-up (accepted_by = the new user, who can then accept it into the vault)
            // and only while its vault still exists.
            if let Some(vault_id) = vault
                && accepted.is_none()
                && expires > now()
                && email_ok
                && mode != SignupMode::Disabled
            {
                let vault_exists: bool = conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM vaults WHERE id = ?1)",
                    [&vault_id],
                    |r| r.get(0),
                )?;
                if vault_exists {
                    return Ok(Some(hash));
                }
            }
        }
        return Err(AppError::new(
            axum::http::StatusCode::FORBIDDEN,
            ErrorCode::InvalidInvite,
            "invite is invalid or expired",
        ));
    }
    match mode {
        SignupMode::Open => Ok(None),
        _ => Err(AppError::new(
            axum::http::StatusCode::FORBIDDEN,
            ErrorCode::SignupsClosed,
            "sign-ups require an invite",
        )),
    }
}

pub async fn register_start(
    State(state): State<AppState>,
    peer: Peer,
    headers: HeaderMap,
    Json(req): Json<RegisterStartRequest>,
) -> AppResult<Json<RegisterStartResponse>> {
    state.check_rate(client_ip(&state, &headers, &peer))?;
    let email = validate_email(&req.email)?;
    let mode = state.config.signups;
    let invite = req.invite_token.clone();
    let email2 = email.clone();
    state
        .db(move |conn| {
            // Admission first, so a closed server does not reveal which e-mails have accounts.
            signup_allowed(conn, mode, &email2, invite.as_deref())?;
            if user_by_email(conn, &email2)?.is_some() {
                return Err(AppError::conflict(
                    "an account with this e-mail already exists",
                ));
            }
            Ok(())
        })
        .await?;

    let user_id = Uuid::now_v7();
    let response =
        server_registration_start(&state.setup, req.request.as_slice(), user_id.as_bytes())?;
    let registration_id = state.put_registration(Registration::NewAccount {
        email,
        user_id,
        invite_token: req.invite_token,
    })?;
    Ok(Json(RegisterStartResponse {
        registration_id,
        user_id,
        response: response.into(),
        kdf: KdfParams::default(),
    }))
}

pub async fn register_finish(
    State(state): State<AppState>,
    Json(req): Json<RegisterFinishRequest>,
) -> AppResult<Json<AuthResponse>> {
    let Registration::NewAccount {
        email,
        user_id,
        invite_token,
    } = state.take_registration(&req.registration_id)?
    else {
        return Err(AppError::bad_request("not a sign-up"));
    };
    let password_file = server_registration_finish(req.upload.as_slice())?;
    let mode = state.config.signups;

    let response = state
        .db(move |conn| {
            let tx = conn.transaction()?;
            if user_by_email(&tx, &email)?.is_some() {
                return Err(AppError::conflict("an account with this e-mail already exists"));
            }
            let first = db::user_count(&tx)? == 0;
            let invite_hash = signup_allowed(&tx, mode, &email, invite_token.as_deref())?;
            let a = &req.account;
            tx.execute(
                "INSERT INTO users (id, email, password_file, kdf, public_kem, public_sign, wrapped_ak_master,
                    wrapped_ak_recovery, encrypted_private_keys, recovery_auth_hash, is_admin, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    user_id.as_bytes().as_slice(),
                    email,
                    password_file,
                    serde_json::to_string(&KdfParams::default())?,
                    a.public_keys.kem.as_slice(),
                    a.public_keys.sign.as_slice(),
                    a.wrapped_account_key_master.as_slice(),
                    a.wrapped_account_key_recovery.as_slice(),
                    a.encrypted_private_keys.as_slice(),
                    recovery_auth_hash(req.recovery_auth_token.as_slice()).as_slice(),
                    first,
                    now(),
                ],
            )?;
            if !req.personal_vault.personal {
                return Err(AppError::bad_request("the first vault must be the personal vault"));
            }
            insert_new_vault(&tx, &user_id, &req.personal_vault)?;
            if let Some(hash) = invite_hash {
                tx.execute(
                    "UPDATE invites SET accepted_by = ?1 WHERE token_hash = ?2",
                    params![user_id.as_bytes().as_slice(), hash],
                )?;
            }
            let response = auth_response(&tx, user_id, &req.device)?;
            tx.commit()?;
            Ok(response)
        })
        .await?;
    tracing::info!(user = %response.user_id, "account created");
    Ok(Json(response))
}

pub async fn login_start(
    State(state): State<AppState>,
    peer: Peer,
    headers: HeaderMap,
    Json(req): Json<LoginStartRequest>,
) -> AppResult<Json<LoginStartResponse>> {
    state.check_rate(client_ip(&state, &headers, &peer))?;
    let email = normalize_email(&req.email);
    let email2 = email.clone();
    let user = state.db(move |conn| user_by_email(conn, &email2)).await?;

    // Unknown users get a deterministic fake credential id and the default KDF, so the response
    // looks the same as for a real account.
    let (user_id, password_file, kdf, credential_id) = match user {
        Some(u) => (
            Some(u.id),
            Some(u.password_file),
            u.kdf,
            u.id.as_bytes().to_vec(),
        ),
        None => (
            None,
            None,
            KdfParams::default(),
            Sha256::digest(email.as_bytes())[..16].to_vec(),
        ),
    };
    let server_id = state.config.server_id();
    let identity = OpaqueIdentity {
        user: email.as_bytes(),
        server: server_id.as_bytes(),
    };
    let (login_state, response) = server_login_start(
        &state.setup,
        password_file.as_deref(),
        req.request.as_slice(),
        &credential_id,
        &identity,
    )?;
    let login_id = state.put_login(login_state, user_id)?;
    Ok(Json(LoginStartResponse {
        login_id,
        response: response.into(),
        kdf,
    }))
}

pub async fn login_finish(
    State(state): State<AppState>,
    Json(req): Json<LoginFinishRequest>,
) -> AppResult<Json<AuthResponse>> {
    let pending = state.take_login(&req.login_id)?;
    let user_id = pending.user_id.ok_or_else(AppError::unauthorized)?;
    let email = state
        .db(move |conn| Ok(user_by_id(conn, &user_id)?.map(|u| u.email)))
        .await?
        .ok_or_else(AppError::unauthorized)?;
    let server_id = state.config.server_id();
    let identity = OpaqueIdentity {
        user: email.as_bytes(),
        server: server_id.as_bytes(),
    };
    pending
        .state
        .finish(req.finalization.as_slice(), &identity)
        .map_err(|_| AppError::unauthorized())?;

    let response = state
        .db(move |conn| auth_response(conn, user_id, &req.device))
        .await?;
    tracing::info!(user = %user_id, device = %response.device_id, "login");
    Ok(Json(response))
}

pub async fn refresh(
    State(state): State<AppState>,
    Json(req): Json<RefreshRequest>,
) -> AppResult<Json<RefreshResponse>> {
    if (now() - req.timestamp).abs() > REFRESH_SKEW_SECS {
        return Err(AppError::bad_request(
            "clock skew too large; check the system time",
        ));
    }
    let tokens = state
        .db(move |conn| {
            let row: Option<(Vec<u8>, Vec<u8>, i64, Vec<u8>, Option<i64>)> = conn
                .query_row(
                    "SELECT s.id, s.device_id, s.refresh_expires, d.public_key, d.revoked_at
                     FROM sessions s JOIN devices d ON d.id = s.device_id WHERE s.refresh_hash = ?1",
                    [hash_token(&req.refresh_token)],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )
                .optional()?;
            let Some((session_id, device_id, expires, public_key, revoked)) = row else {
                return Err(AppError::unauthorized());
            };
            if uuid_from(device_id)? != req.device_id || expires < now() || revoked.is_some() {
                return Err(AppError::unauthorized());
            }
            let message = refresh_signing_bytes(&req.device_id, &req.refresh_token, req.timestamp);
            verify_ed25519(&public_key, &message, req.signature.as_slice()).map_err(|_| AppError::unauthorized())?;
            rotate_session(conn, &session_id)
        })
        .await?;
    Ok(Json(RefreshResponse {
        access_token: tokens.access,
        access_expires_at: tokens.access_expires,
        refresh_token: tokens.refresh,
    }))
}

pub async fn logout(
    State(state): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
) -> AppResult<Json<()>> {
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string)
        .ok_or_else(AppError::unauthorized)?;
    state
        .db(move |conn| {
            conn.execute(
                "DELETE FROM sessions WHERE access_hash = ?1",
                [hash_token(&token)],
            )?;
            conn.execute(
                "UPDATE devices SET revoked_at = ?1 WHERE id = ?2",
                params![now(), auth.device_id.as_bytes().as_slice()],
            )?;
            Ok(())
        })
        .await?;
    Ok(Json(()))
}

// ---------------------------------------------------------------------------------------------
// Password change (authenticated) and recovery (with the recovery key)
// ---------------------------------------------------------------------------------------------

pub async fn password_start(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<PasswordChangeStartRequest>,
) -> AppResult<Json<PasswordChangeStartResponse>> {
    let response = server_registration_start(
        &state.setup,
        req.request.as_slice(),
        auth.user_id.as_bytes(),
    )?;
    let registration_id = state.put_registration(Registration::PasswordChange {
        user_id: auth.user_id,
    })?;
    Ok(Json(PasswordChangeStartResponse {
        registration_id,
        response: response.into(),
        kdf: KdfParams::default(),
    }))
}

pub async fn password_finish(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<PasswordChangeFinishRequest>,
) -> AppResult<Json<()>> {
    let Registration::PasswordChange { user_id } = state.take_registration(&req.registration_id)?
    else {
        return Err(AppError::bad_request("not a password change"));
    };
    if user_id != auth.user_id {
        return Err(AppError::forbidden("registration belongs to another user"));
    }
    let password_file = server_registration_finish(req.upload.as_slice())?;
    state
        .db(move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "UPDATE users SET password_file = ?1, wrapped_ak_master = ?2, kdf = ?3 WHERE id = ?4",
                params![
                    password_file,
                    req.wrapped_account_key_master.as_slice(),
                    serde_json::to_string(&KdfParams::default())?,
                    user_id.as_bytes().as_slice()
                ],
            )?;
            // Other devices must log in again with the new password.
            tx.execute(
                "DELETE FROM sessions WHERE user_id = ?1 AND device_id != ?2",
                params![user_id.as_bytes().as_slice(), auth.device_id.as_bytes().as_slice()],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await?;
    Ok(Json(()))
}

pub async fn recovery_start(
    State(state): State<AppState>,
    peer: Peer,
    headers: HeaderMap,
    Json(req): Json<RecoveryStartRequest>,
) -> AppResult<Json<RecoveryStartResponse>> {
    state.check_rate(client_ip(&state, &headers, &peer))?;
    let email = normalize_email(&req.email);
    let user = state
        .db(move |conn| user_by_email(conn, &email))
        .await?
        .ok_or_else(AppError::unauthorized)?;
    let presented = recovery_auth_hash(req.recovery_auth_token.as_slice());
    if !bool::from(presented.as_slice().ct_eq(&user.recovery_auth_hash)) {
        return Err(AppError::unauthorized());
    }
    let response =
        server_registration_start(&state.setup, req.request.as_slice(), user.id.as_bytes())?;
    let registration_id = state.put_registration(Registration::Recovery { user_id: user.id })?;
    Ok(Json(RecoveryStartResponse {
        registration_id,
        response: response.into(),
        kdf: KdfParams::default(),
        account: user.account,
    }))
}

pub async fn recovery_finish(
    State(state): State<AppState>,
    Json(req): Json<RecoveryFinishRequest>,
) -> AppResult<Json<AuthResponse>> {
    let Registration::Recovery { user_id } = state.take_registration(&req.registration_id)? else {
        return Err(AppError::bad_request("not a recovery"));
    };
    let password_file = server_registration_finish(req.upload.as_slice())?;
    let response = state
        .db(move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "UPDATE users SET password_file = ?1, wrapped_ak_master = ?2, kdf = ?3 WHERE id = ?4",
                params![
                    password_file,
                    req.wrapped_account_key_master.as_slice(),
                    serde_json::to_string(&KdfParams::default())?,
                    user_id.as_bytes().as_slice()
                ],
            )?;
            // Recovery means the password may be compromised: sign out every device.
            tx.execute("DELETE FROM sessions WHERE user_id = ?1", [user_id.as_bytes().as_slice()])?;
            let response = auth_response(&tx, user_id, &req.device)?;
            tx.commit()?;
            Ok(response)
        })
        .await?;
    tracing::info!(user = %user_id, "account recovered");
    Ok(Json(response))
}

#[allow(dead_code)]
fn _assert_auth_send(a: auth::Auth) -> impl Send {
    a
}
