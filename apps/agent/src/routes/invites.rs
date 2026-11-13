//! Vault invites (join a vault) and sign-up invites (join the server).

use axum::Json;
use axum::extract::{Path, State};
use axum::response::Html;
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;
use vessel_protocol::*;

use super::vaults::vault_for_member;
use crate::auth::{Auth, hash_token, new_token, require_server_admin};
use crate::db::{self, now, role_code, role_from, user_by_id, uuid_from, vault_members};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

const INVITE_TTL_SECS: i64 = 7 * 24 * 3600;

fn invite_url(state: &AppState, token: &str) -> String {
    format!("{}/invite/{token}", state.config.public_url)
}

pub async fn create_vault_invite(
    State(state): State<AppState>,
    auth: Auth,
    Path(vault): Path<Uuid>,
    Json(req): Json<CreateInviteRequest>,
) -> AppResult<Json<InviteResponse>> {
    if req.role == Role::Owner {
        return Err(AppError::bad_request("cannot invite an owner"));
    }
    let token = new_token()?;
    let hash = hash_token(&token);
    let expires_at = now() + INVITE_TTL_SECS;
    state
        .db(move |conn| {
            let (row, role) = vault_for_member(conn, &vault, &auth.user_id)?;
            if row.personal {
                return Err(AppError::forbidden("the personal vault cannot be shared"));
            }
            if !role.can_manage() {
                return Err(AppError::forbidden("admins only"));
            }
            if role != Role::Owner && req.role >= Role::Admin {
                return Err(AppError::forbidden("only the owner can invite admins"));
            }
            conn.execute(
                "INSERT INTO invites (token_hash, vault_id, role, email, created_by, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    hash,
                    vault.as_bytes().as_slice(),
                    role_code(req.role),
                    req.email.as_deref().map(normalize_email),
                    auth.user_id.as_bytes().as_slice(),
                    expires_at
                ],
            )?;
            Ok(())
        })
        .await?;
    Ok(Json(InviteResponse {
        url: invite_url(&state, &token),
        token,
        expires_at,
    }))
}

pub async fn create_signup_invite(
    State(state): State<AppState>,
    auth: Auth,
    Json(req): Json<CreateInviteRequest>,
) -> AppResult<Json<InviteResponse>> {
    let token = new_token()?;
    let hash = hash_token(&token);
    let expires_at = now() + INVITE_TTL_SECS;
    state
        .db(move |conn| {
            require_server_admin(conn, &auth.user_id)?;
            conn.execute(
                "INSERT INTO invites (token_hash, vault_id, role, email, created_by, expires_at) VALUES (?1, NULL, ?2, ?3, ?4, ?5)",
                params![hash, role_code(Role::Viewer), req.email.as_deref().map(normalize_email), auth.user_id.as_bytes().as_slice(), expires_at],
            )?;
            Ok(())
        })
        .await?;
    Ok(Json(InviteResponse {
        url: invite_url(&state, &token),
        token,
        expires_at,
    }))
}

/// Accepting a vault invite puts the caller in the vault's pending list. A manager's app then
/// verifies their fingerprint, signs an `Add` event and issues the grant.
pub async fn accept(
    State(state): State<AppState>,
    auth: Auth,
    Path(token): Path<String>,
) -> AppResult<Json<AcceptInviteResponse>> {
    let hash = hash_token(&token);
    let (vault, managers) = state
        .db(move |conn| {
            let tx = conn.transaction()?;
            let row: Option<(Option<Vec<u8>>, i64, Option<String>, Vec<u8>, i64, Option<Vec<u8>>)> = tx
                .query_row(
                    "SELECT vault_id, role, email, created_by, expires_at, accepted_by FROM invites WHERE token_hash = ?1",
                    [&hash],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
                )
                .optional()?;
            let invalid = || AppError::new(axum::http::StatusCode::FORBIDDEN, ErrorCode::InvalidInvite, "invite is invalid or expired");
            let (vault, role, email, created_by, expires_at, accepted_by) = row.ok_or_else(invalid)?;
            let user = user_by_id(&tx, &auth.user_id)?.ok_or_else(AppError::unauthorized)?;
            if expires_at < now() || email.is_some_and(|e| e != user.email) {
                return Err(invalid());
            }
            if let Some(accepted) = accepted_by
                && accepted != auth.user_id.as_bytes()
            {
                return Err(invalid());
            }
            let Some(vault) = vault else {
                return Ok((None, Vec::new()));
            };
            let vault = uuid_from(vault)?;
            let vault_exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM vaults WHERE id = ?1)",
                [vault.as_bytes().as_slice()],
                |r| r.get(0),
            )?;
            if !vault_exists {
                return Err(invalid());
            }
            if db::member_role(&tx, &vault, &auth.user_id)?.is_some() {
                return Err(AppError::conflict("you are already a member of this vault"));
            }
            tx.execute(
                "INSERT OR REPLACE INTO pending_members (vault_id, user_id, role, invited_by, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![vault.as_bytes().as_slice(), auth.user_id.as_bytes().as_slice(), role_code(role_from(role)?), created_by, now()],
            )?;
            tx.execute(
                "UPDATE invites SET accepted_by = ?1 WHERE token_hash = ?2",
                params![auth.user_id.as_bytes().as_slice(), hash],
            )?;
            let managers: Vec<Uuid> =
                vault_members(&tx, &vault)?.into_iter().filter(|(_, r)| r.can_manage()).map(|(u, _)| u).collect();
            tx.commit()?;
            Ok((Some(vault), managers))
        })
        .await?;
    if let Some(vault_id) = vault {
        state.notify(managers, ServerEvent::VaultUpdated { vault_id });
    }
    Ok(Json(AcceptInviteResponse { vault_id: vault }))
}

/// Plain page for invite links opened in a browser.
pub async fn landing_page(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Html<String> {
    let token: String = token
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let server = &state.config.public_url;
    Html(format!(
        r#"<!doctype html><html lang="pt-BR"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Convite Vessel</title><style>body{{font:16px system-ui;background:#0f1117;color:#e6e8ee;display:grid;place-items:center;min-height:100vh;margin:0}}
main{{max-width:520px;padding:32px;background:#171a23;border-radius:14px;border:1px solid #262b38}}code{{display:block;word-break:break-all;background:#0f1117;padding:12px;border-radius:8px;margin:8px 0}}</style></head>
<body><main><h1>Você foi convidado para o Vessel</h1><p>Abra o app Vessel, entre (ou crie uma conta) no servidor abaixo e use <b>Aceitar convite</b> com este código:</p>
<p>Servidor</p><code>{server}</code><p>Código do convite</p><code>{token}</code></main></body></html>"#
    ))
}
