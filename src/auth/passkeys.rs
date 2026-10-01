//! Authenticated passkey management with fresh, operation-bound proof of ownership.
//!
//! A login session alone cannot add/remove credentials or create a recovery key.
//! Each operation requires a current password or a one-use, account/version-bound
//! passkey assertion. Replacement issues a saved recovery key before provider
//! creation, then swaps credentials atomically. Removal and replacement revoke
//! every session. Recovery secrets are exposed once and are never logged.

#![forbid(unsafe_code)]

use super::{
    account, routes, session,
    types::{AuthError, Claims},
    webauthn::{
        AccountAuthenticationData, AccountChallenge, AccountRegistrationData, ChallengeStore,
        ReplacementRegistration,
    },
};
use crate::signaling::{ClientIp, SignalingServer};
use axum::{Extension, Json, extract::State, http::HeaderMap};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, Postgres, Transaction};
use std::net::IpAddr;
use uuid::Uuid;
use webauthn_rs::prelude::{Passkey, PublicKeyCredential, RegisterPublicKeyCredential};

const MAX_PASSKEYS: i64 = 10;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum PasskeyAction {
    Add {},
    Remove { id: Uuid },
    Replace { id: Uuid },
    RecoveryKey {},
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartRequest {
    pub operation: PasskeyAction,
    pub current_password: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizeRequest {
    pub ceremony_id: String,
    pub credential: PublicKeyCredential,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrollRequest {
    pub ceremony_id: String,
    pub credential: RegisterPublicKeyCredential,
}

#[derive(Serialize, FromRow)]
pub struct PasskeySummary {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize)]
pub struct PasskeySettings {
    pub password_enabled: bool,
    pub recovery_enabled: bool,
    pub passkeys: Vec<PasskeySummary>,
    pub maximum: i64,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActionResponse {
    Authenticate {
        ceremony_id: String,
        options: Value,
    },
    Register {
        ceremony_id: String,
        options: Value,
    },
    ReplaceRegistration {
        ceremony_id: String,
        options: Value,
        recovery_key: String,
    },
    RecoveryKey {
        recovery_key: String,
    },
    Removed,
    Added,
    Replaced,
}

#[derive(FromRow)]
struct AccountState {
    email: String,
    display_name: String,
    password_hash: Option<String>,
    recovery_enabled: bool,
}

fn account_id(claims: &Claims) -> Result<Uuid, AuthError> {
    Uuid::parse_str(&claims.sub).map_err(|_| AuthError::InvalidToken)
}

async fn locked_account(
    transaction: &mut Transaction<'_, Postgres>,
    id: Uuid,
    version: i64,
) -> Result<AccountState, AuthError> {
    sqlx::query_as("SELECT email, display_name, password_hash, recovery_key_hash IS NOT NULL AS recovery_enabled FROM users WHERE id = $1 AND auth_version = $2 FOR NO KEY UPDATE")
        .bind(id).bind(version).fetch_optional(&mut **transaction).await
        .map_err(routes::database_error)?.ok_or(AuthError::InvalidToken)
}

async fn credentials(
    transaction: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<Vec<(Uuid, Value)>, AuthError> {
    let values = sqlx::query_as("SELECT id, credential_json FROM webauthn_credentials WHERE user_id = $1 ORDER BY created_at, id LIMIT $2")
        .bind(id).bind(MAX_PASSKEYS + 1).fetch_all(&mut **transaction).await
        .map_err(routes::database_error)?;
    Ok(values)
}

/// Return record identifiers and dates, never credential IDs or key material.
pub async fn list(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<PasskeySettings>), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let claims = account::authenticated_claims(&server, &headers).await?;
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let id = account_id(&claims)?;
    let mut transaction = pool.begin().await.map_err(routes::database_error)?;
    let owner = locked_account(&mut transaction, id, claims.auth_version).await?;
    let passkeys: Vec<PasskeySummary> = sqlx::query_as("SELECT id, created_at FROM webauthn_credentials WHERE user_id = $1 ORDER BY created_at, id LIMIT $2")
        .bind(id).bind(MAX_PASSKEYS + 1).fetch_all(&mut *transaction).await
        .map_err(routes::database_error)?;
    transaction.commit().await.map_err(routes::database_error)?;
    Ok((
        routes::no_store_headers(),
        Json(PasskeySettings {
            password_enabled: owner.password_hash.is_some(),
            recovery_enabled: owner.recovery_enabled,
            passkeys,
            maximum: MAX_PASSKEYS,
        }),
    ))
}

/// Password proof may perform the requested action immediately. Otherwise issue
/// a modal discovery challenge bound to this account and this exact operation.
pub(crate) async fn start(
    State(server): State<SignalingServer>,
    Extension(ClientIp(source_ip)): Extension<ClientIp>,
    headers: HeaderMap,
    Json(request): Json<StartRequest>,
) -> Result<(HeaderMap, Json<ActionResponse>), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let claims = account::authenticated_claims(&server, &headers).await?;
    let id = account_id(&claims)?;
    if let Some(password) = request.current_password {
        let verified = account::verified_password_hash(&server, &claims, password).await?;
        let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
        let mut transaction = pool.begin().await.map_err(routes::database_error)?;
        let owner = locked_account(&mut transaction, id, claims.auth_version).await?;
        if owner.password_hash.as_deref() != Some(verified.as_str()) {
            return Err(AuthError::InvalidCredentials);
        }
        return apply_action(
            &server,
            transaction,
            owner,
            &claims,
            request.operation,
            source_ip,
        )
        .await;
    }
    if !server.allow_auth_principal(&claims.sub) {
        return Err(AuthError::RateLimited);
    }
    let webauthn = server.webauthn().ok_or(AuthError::NotConfigured)?;
    let store = server.challenge_store().ok_or(AuthError::NotConfigured)?;
    let (challenge, state) = webauthn
        .start_discoverable_authentication()
        .map_err(|error| AuthError::WebAuthnError(error.to_string()))?;
    let ceremony_id = store
        .store_account(
            AccountChallenge::Authentication(AccountAuthenticationData {
                state,
                user_id: id,
                auth_version: claims.auth_version,
                action: request.operation,
            }),
            source_ip,
        )
        .ok_or(AuthError::RateLimited)?;
    Ok((
        routes::no_store_headers(),
        Json(ActionResponse::Authenticate {
            ceremony_id,
            options: routes::modal_login_options(challenge)?,
        }),
    ))
}

/// Complete fresh passkey proof without creating or replacing a login session.
pub(crate) async fn authorize(
    State(server): State<SignalingServer>,
    Extension(ClientIp(source_ip)): Extension<ClientIp>,
    headers: HeaderMap,
    Json(request): Json<AuthorizeRequest>,
) -> Result<(HeaderMap, Json<ActionResponse>), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let claims = account::authenticated_claims(&server, &headers).await?;
    let id = account_id(&claims)?;
    routes::validate_ceremony_id(&request.ceremony_id)?;
    let store = server.challenge_store().ok_or(AuthError::NotConfigured)?;
    let Some(AccountChallenge::Authentication(data)) =
        store.take_account(&request.ceremony_id, id, claims.auth_version)
    else {
        return Err(AuthError::InvalidPasskey);
    };
    let webauthn = server.webauthn().ok_or(AuthError::NotConfigured)?;
    let (presented_owner, presented_id) = webauthn
        .identify_discoverable_authentication(&request.credential)
        .map_err(|_| AuthError::InvalidPasskey)?;
    if presented_owner != id {
        return Err(AuthError::InvalidPasskey);
    }
    let passkey_id = URL_SAFE_NO_PAD.encode(presented_id);
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let mut transaction = pool.begin().await.map_err(routes::database_error)?;
    let owner = locked_account(&mut transaction, id, claims.auth_version).await?;
    routes::verify_discoverable_assertion(
        &mut transaction,
        webauthn,
        &request.credential,
        data.state,
        id,
        &passkey_id,
    )
    .await?;
    apply_action(&server, transaction, owner, &claims, data.action, source_ip).await
}

async fn apply_action(
    server: &SignalingServer,
    mut transaction: Transaction<'_, Postgres>,
    owner: AccountState,
    claims: &Claims,
    action: PasskeyAction,
    source_ip: IpAddr,
) -> Result<(HeaderMap, Json<ActionResponse>), AuthError> {
    let id = account_id(claims)?;
    let response = match action {
        PasskeyAction::Add {} => {
            return prepare_registration(server, transaction, owner, claims, None, source_ip).await;
        }
        PasskeyAction::Replace { id: target } => {
            return prepare_registration(
                server,
                transaction,
                owner,
                claims,
                Some(target),
                source_ip,
            )
            .await;
        }
        PasskeyAction::RecoveryKey {} => {
            let recovery_key = account::generate_recovery_key()?;
            sqlx::query(
                "UPDATE users SET recovery_key_hash = $2, updated_at = now() WHERE id = $1",
            )
            .bind(id)
            .bind(session::hash_token(&recovery_key))
            .execute(&mut *transaction)
            .await
            .map_err(routes::database_error)?;
            ActionResponse::RecoveryKey { recovery_key }
        }
        PasskeyAction::Remove { id: credential } => {
            let version = remove_credential(
                &mut transaction,
                id,
                claims.auth_version,
                credential,
                owner.password_hash.is_some(),
            )
            .await?;
            transaction.commit().await.map_err(routes::database_error)?;
            server.revoke_account_sessions(claims.sub.clone(), version);
            return Ok((
                routes::clear_refresh_cookie_headers(),
                Json(ActionResponse::Removed),
            ));
        }
    };
    transaction.commit().await.map_err(routes::database_error)?;
    Ok((routes::no_store_headers(), Json(response)))
}

async fn prepare_registration(
    server: &SignalingServer,
    mut transaction: Transaction<'_, Postgres>,
    owner: AccountState,
    claims: &Claims,
    target: Option<Uuid>,
    source_ip: IpAddr,
) -> Result<(HeaderMap, Json<ActionResponse>), AuthError> {
    let id = account_id(claims)?;
    let records = credentials(&mut transaction, id).await?;
    if let Some(target) = target {
        if !records.iter().any(|(record, _)| *record == target) {
            return Err(AuthError::InvalidInput("Passkey is no longer available"));
        }
    } else if records.len() >= MAX_PASSKEYS as usize {
        return Err(AuthError::InvalidInput(
            "This account already has the maximum number of passkeys",
        ));
    }
    let keys: Vec<Passkey> = records
        .into_iter()
        .filter(|(record, _)| Some(*record) != target)
        .map(|(_, value)| serde_json::from_value(value))
        .collect::<Result<_, _>>()
        .map_err(|error| AuthError::DatabaseError(error.to_string()))?;
    let exclude = keys.iter().map(|key| key.cred_id().clone()).collect();
    let webauthn = server.webauthn().ok_or(AuthError::NotConfigured)?;
    let (mut challenge, state) = webauthn
        .start_passkey_registration(id, &owner.email, &owner.display_name, Some(exclude))
        .map_err(|error| AuthError::WebAuthnError(error.to_string()))?;
    routes::require_discoverable_registration(&mut challenge)?;
    let options = serde_json::to_value(challenge)
        .map_err(|error| AuthError::WebAuthnError(error.to_string()))?;
    let recovery_key = target
        .map(|_| account::generate_recovery_key())
        .transpose()?;
    let replacement = match (target, recovery_key.as_ref()) {
        (Some(target), Some(key)) => {
            let recovery_hash = session::hash_token(key);
            sqlx::query(
                "UPDATE users SET recovery_key_hash = $2, updated_at = now() WHERE id = $1",
            )
            .bind(id)
            .bind(&recovery_hash)
            .execute(&mut *transaction)
            .await
            .map_err(routes::database_error)?;
            Some(ReplacementRegistration {
                target,
                recovery_hash,
            })
        }
        _ => None,
    };
    let store = server.challenge_store().ok_or(AuthError::NotConfigured)?;
    let ceremony_id = commit_registration(
        transaction,
        store,
        AccountRegistrationData {
            state,
            user_id: id,
            auth_version: claims.auth_version,
            replacement,
        },
        source_ip,
    )
    .await?;
    let response = match recovery_key {
        Some(recovery_key) => ActionResponse::ReplaceRegistration {
            ceremony_id,
            options,
            recovery_key,
        },
        None => ActionResponse::Register {
            ceremony_id,
            options,
        },
    };
    Ok((routes::no_store_headers(), Json(response)))
}

async fn commit_registration(
    transaction: Transaction<'_, Postgres>,
    store: &ChallengeStore,
    data: AccountRegistrationData,
    source_ip: IpAddr,
) -> Result<String, AuthError> {
    let (user, version) = (data.user_id, data.auth_version);
    // Reserve capacity before committing a newly issued recovery key. A full
    // store rolls back both that key and the fresh assertion's counter update.
    let ceremony_id = store
        .store_account(AccountChallenge::Registration(data), source_ip)
        .ok_or(AuthError::RateLimited)?;
    if let Err(error) = transaction.commit().await {
        let _ = store.take_account(&ceremony_id, user, version);
        return Err(routes::database_error(error));
    }
    Ok(ceremony_id)
}

async fn replace_credential(
    transaction: &mut Transaction<'_, Postgres>,
    user: Uuid,
    version: i64,
    replacement: &ReplacementRegistration,
    credential_id: &str,
    credential_json: Value,
) -> Result<i64, AuthError> {
    let _owner = locked_account(transaction, user, version).await?;
    let backup_current: bool = sqlx::query_scalar(
        "SELECT recovery_key_hash IS NOT DISTINCT FROM $2 FROM users WHERE id = $1",
    )
    .bind(user)
    .bind(&replacement.recovery_hash)
    .fetch_one(&mut **transaction)
    .await
    .map_err(routes::database_error)?;
    if !backup_current {
        return Err(AuthError::InvalidInput(
            "The saved recovery key changed; reload and use the latest recovery key",
        ));
    }
    if !credentials(transaction, user)
        .await?
        .iter()
        .any(|(id, _)| *id == replacement.target)
    {
        return Err(AuthError::InvalidInput("Passkey is no longer available"));
    }
    let next = version.checked_add(1).ok_or(AuthError::InvalidToken)?;
    // The unique credential index also rejects reusing the target credential ID.
    // No old credential or session is removed unless insertion succeeds.
    sqlx::query("INSERT INTO webauthn_credentials (user_id, credential_id, credential_json) VALUES ($1, $2, $3)")
        .bind(user).bind(credential_id).bind(credential_json).execute(&mut **transaction).await
        .map_err(routes::credential_insert_error)?;
    let deleted = sqlx::query("DELETE FROM webauthn_credentials WHERE user_id = $1 AND id = $2")
        .bind(user)
        .bind(replacement.target)
        .execute(&mut **transaction)
        .await
        .map_err(routes::database_error)?;
    if deleted.rows_affected() != 1 {
        return Err(AuthError::InvalidPasskey);
    }
    sqlx::query("UPDATE users SET auth_version = $2, updated_at = now() WHERE id = $1")
        .bind(user)
        .bind(next)
        .execute(&mut **transaction)
        .await
        .map_err(routes::database_error)?;
    sqlx::query("DELETE FROM sessions WHERE user_id = $1")
        .bind(user)
        .execute(&mut **transaction)
        .await
        .map_err(routes::database_error)?;
    Ok(next)
}

async fn remove_credential(
    transaction: &mut Transaction<'_, Postgres>,
    user: Uuid,
    version: i64,
    credential: Uuid,
    password_enabled: bool,
) -> Result<i64, AuthError> {
    let records = credentials(transaction, user).await?;
    if !records.iter().any(|(id, _)| *id == credential) {
        return Err(AuthError::InvalidInput("Passkey is no longer available"));
    }
    // A recovery key is a recovery path, not a directly usable sign-in method.
    if records.len() <= 1 && !password_enabled {
        return Err(AuthError::InvalidInput(
            "Add another passkey before removing your last sign-in method",
        ));
    }
    let next = version.checked_add(1).ok_or(AuthError::InvalidToken)?;
    sqlx::query("DELETE FROM webauthn_credentials WHERE user_id = $1 AND id = $2")
        .bind(user)
        .bind(credential)
        .execute(&mut **transaction)
        .await
        .map_err(routes::database_error)?;
    sqlx::query("UPDATE users SET auth_version = $2, updated_at = now() WHERE id = $1")
        .bind(user)
        .bind(next)
        .execute(&mut **transaction)
        .await
        .map_err(routes::database_error)?;
    sqlx::query("DELETE FROM sessions WHERE user_id = $1")
        .bind(user)
        .execute(&mut **transaction)
        .await
        .map_err(routes::database_error)?;
    Ok(next)
}

/// The enrollment challenge represents a recently verified owner, not a bearer
/// enrollment token: current claims and the same authentication version are required.
pub async fn enroll(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Json(request): Json<EnrollRequest>,
) -> Result<(HeaderMap, Json<ActionResponse>), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let claims = account::authenticated_claims(&server, &headers).await?;
    let id = account_id(&claims)?;
    routes::validate_ceremony_id(&request.ceremony_id)?;
    let store = server.challenge_store().ok_or(AuthError::NotConfigured)?;
    let Some(AccountChallenge::Registration(data)) =
        store.take_account(&request.ceremony_id, id, claims.auth_version)
    else {
        return Err(AuthError::InvalidPasskey);
    };
    let webauthn = server.webauthn().ok_or(AuthError::NotConfigured)?;
    let key = webauthn
        .finish_passkey_registration(&request.credential, &data.state)
        .map_err(|_| AuthError::InvalidPasskey)?;
    let value =
        serde_json::to_value(&key).map_err(|error| AuthError::WebAuthnError(error.to_string()))?;
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let mut transaction = pool.begin().await.map_err(routes::database_error)?;
    if let Some(replacement) = data.replacement {
        let next = replace_credential(
            &mut transaction,
            id,
            claims.auth_version,
            &replacement,
            &routes::credential_id(&key),
            value,
        )
        .await?;
        transaction.commit().await.map_err(routes::database_error)?;
        server.revoke_account_sessions(claims.sub, next);
        return Ok((
            routes::clear_refresh_cookie_headers(),
            Json(ActionResponse::Replaced),
        ));
    }
    let _owner = locked_account(&mut transaction, id, claims.auth_version).await?;
    if credentials(&mut transaction, id).await?.len() >= MAX_PASSKEYS as usize {
        return Err(AuthError::InvalidInput(
            "This account already has the maximum number of passkeys",
        ));
    }
    sqlx::query("INSERT INTO webauthn_credentials (user_id, credential_id, credential_json) VALUES ($1, $2, $3)")
        .bind(id).bind(routes::credential_id(&key)).bind(value).execute(&mut *transaction).await
        .map_err(routes::credential_insert_error)?;
    transaction.commit().await.map_err(routes::database_error)?;
    Ok((routes::no_store_headers(), Json(ActionResponse::Added)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn management_requests_accept_only_the_fixed_operation_fields() {
        assert!(
            serde_json::from_value::<StartRequest>(
                serde_json::json!({"operation":{"action":"add"}})
            )
            .is_ok()
        );
        assert!(
            serde_json::from_value::<StartRequest>(serde_json::json!({
                "operation":{"action":"replace","id":Uuid::new_v4()}
            }))
            .is_ok()
        );
        for value in [
            serde_json::json!({"operation":{"action":"add","user_id":Uuid::new_v4()}}),
            serde_json::json!({"operation":{"action":"remove"}}),
            serde_json::json!({"operation":{"action":"remove","id":"credential"}}),
            serde_json::json!({"operation":{"action":"replace"}}),
            serde_json::json!({"operation":{"action":"replace","id":Uuid::new_v4(),"recovery_hash":"untrusted"}}),
            serde_json::json!({"operation":{"action":"recover"}}),
            serde_json::json!({"operation":{"action":"recovery_key"},"authorized":true}),
        ] {
            assert!(serde_json::from_value::<StartRequest>(value).is_err());
        }
    }

    async fn replacement_fixture(
        pool: &sqlx::PgPool,
        count: usize,
    ) -> (Uuid, Vec<Uuid>, ReplacementRegistration) {
        let owner = Uuid::new_v4();
        let recovery_hash = "a".repeat(64);
        sqlx::query("INSERT INTO users (id, email, display_name, recovery_key_hash) VALUES ($1, $2, 'Replacement test', $3)")
            .bind(owner).bind(format!("replace-{owner}@example.test")).bind(&recovery_hash)
            .execute(pool).await.unwrap();
        let mut keys = Vec::new();
        for _ in 0..count {
            let key = Uuid::new_v4();
            sqlx::query("INSERT INTO webauthn_credentials (id, user_id, credential_id, credential_json) VALUES ($1, $2, $3, '{}')")
                .bind(key).bind(owner).bind(key.to_string()).execute(pool).await.unwrap();
            keys.push(key);
        }
        session::create_session(pool, &owner, &session::generate_refresh_token().unwrap())
            .await
            .unwrap();
        let replacement = ReplacementRegistration {
            target: keys[0],
            recovery_hash,
        };
        (owner, keys, replacement)
    }

    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
    async fn replacement_at_limit_preserves_backup_and_atomically_revokes_sessions() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
            .await
            .unwrap();
        let (owner, keys, replacement) = replacement_fixture(&pool, MAX_PASSKEYS as usize).await;
        let new_id = Uuid::new_v4().to_string();
        let mut transaction = pool.begin().await.unwrap();
        assert_eq!(
            replace_credential(
                &mut transaction,
                owner,
                0,
                &replacement,
                &new_id,
                serde_json::json!({})
            )
            .await
            .unwrap(),
            1
        );
        transaction.commit().await.unwrap();
        let remaining: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM webauthn_credentials WHERE user_id = $1")
                .bind(owner)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(remaining.len(), MAX_PASSKEYS as usize);
        assert!(!remaining.contains(&keys[0]));
        assert!(keys[1..].iter().all(|key| remaining.contains(key)));
        let state: (i64, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT auth_version, password_hash, recovery_key_hash FROM users WHERE id = $1",
        )
        .bind(owner)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(state, (1, None, Some(replacement.recovery_hash)));
        let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id = $1")
            .bind(owner)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(sessions, 0);
        let mut transaction = pool.begin().await.unwrap();
        assert!(matches!(
            locked_account(&mut transaction, owner, 0).await,
            Err(AuthError::InvalidToken)
        ));
        transaction.rollback().await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
    async fn replacement_rejects_foreign_stale_and_duplicate_credentials_without_changes() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
            .await
            .unwrap();
        let (owner, keys, original) = replacement_fixture(&pool, 1).await;
        let (_, foreign, _) = replacement_fixture(&pool, 1).await;
        for (target, hash, version, new_id) in [
            (
                Uuid::new_v4(),
                original.recovery_hash.clone(),
                0,
                Uuid::new_v4().to_string(),
            ),
            (
                foreign[0],
                original.recovery_hash.clone(),
                0,
                Uuid::new_v4().to_string(),
            ),
            (keys[0], "b".repeat(64), 0, Uuid::new_v4().to_string()),
            (
                keys[0],
                original.recovery_hash.clone(),
                1,
                Uuid::new_v4().to_string(),
            ),
            (
                keys[0],
                original.recovery_hash.clone(),
                0,
                keys[0].to_string(),
            ),
            (
                keys[0],
                original.recovery_hash.clone(),
                0,
                foreign[0].to_string(),
            ),
        ] {
            let mut transaction = pool.begin().await.unwrap();
            let replacement = ReplacementRegistration {
                target,
                recovery_hash: hash,
            };
            assert!(
                replace_credential(
                    &mut transaction,
                    owner,
                    version,
                    &replacement,
                    &new_id,
                    serde_json::json!({})
                )
                .await
                .is_err()
            );
            transaction.rollback().await.unwrap();
            let remaining: Vec<Uuid> =
                sqlx::query_scalar("SELECT id FROM webauthn_credentials WHERE user_id = $1")
                    .bind(owner)
                    .fetch_all(&pool)
                    .await
                    .unwrap();
            assert_eq!(remaining, keys);
            let state: (i64, Option<String>, Option<String>) = sqlx::query_as(
                "SELECT auth_version, password_hash, recovery_key_hash FROM users WHERE id = $1",
            )
            .bind(owner)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(state, (0, None, Some(original.recovery_hash.clone())));
            let sessions: i64 =
                sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id = $1")
                    .bind(owner)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            assert_eq!(sessions, 1);
        }
    }

    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
    async fn full_challenge_store_rolls_back_replacement_backup_preparation() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
            .await
            .unwrap();
        let (owner, keys, _) = replacement_fixture(&pool, 1).await;
        let webauthn = webauthn_rs::prelude::WebauthnBuilder::new(
            "localhost",
            &url::Url::parse("https://localhost").unwrap(),
        )
        .unwrap()
        .build()
        .unwrap();
        let store = ChallengeStore::new();
        let source = IpAddr::from([192, 0, 2, 1]);
        for _ in 0..3 {
            let (_, state) = webauthn.start_discoverable_authentication().unwrap();
            assert!(
                store
                    .store_account(
                        AccountChallenge::Authentication(AccountAuthenticationData {
                            state,
                            user_id: owner,
                            auth_version: 0,
                            action: PasskeyAction::RecoveryKey {},
                        }),
                        source
                    )
                    .is_some()
            );
        }
        let mut transaction = pool.begin().await.unwrap();
        let _owner = locked_account(&mut transaction, owner, 0).await.unwrap();
        sqlx::query("UPDATE users SET recovery_key_hash = repeat('b', 64) WHERE id = $1")
            .bind(owner)
            .execute(&mut *transaction)
            .await
            .unwrap();
        sqlx::query("UPDATE webauthn_credentials SET credential_json = '{\"fixture_counter\":1}' WHERE id = $1")
            .bind(keys[0]).execute(&mut *transaction).await.unwrap();
        let (_, state) = webauthn
            .start_passkey_registration(owner, "owner@example.test", "Owner", None)
            .unwrap();
        let result = commit_registration(
            transaction,
            &store,
            AccountRegistrationData {
                state,
                user_id: owner,
                auth_version: 0,
                replacement: Some(ReplacementRegistration {
                    target: keys[0],
                    recovery_hash: "b".repeat(64),
                }),
            },
            source,
        )
        .await;
        assert!(matches!(result, Err(AuthError::RateLimited)));
        let backup: Option<String> =
            sqlx::query_scalar("SELECT recovery_key_hash FROM users WHERE id = $1")
                .bind(owner)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(backup, Some("a".repeat(64)));
        let credential: Value =
            sqlx::query_scalar("SELECT credential_json FROM webauthn_credentials WHERE id = $1")
                .bind(keys[0])
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(credential, serde_json::json!({}));
    }

    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
    async fn removal_keeps_a_sign_in_method_and_atomically_revokes_sessions() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(4)
            .connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
            .await
            .unwrap();
        let owner = Uuid::new_v4();
        let email = format!("passkey-management-{owner}@example.test");
        sqlx::query("INSERT INTO users (id, email, display_name, recovery_key_hash) VALUES ($1, $2, 'Management test', repeat('a', 64))")
            .bind(owner).bind(email).execute(&pool).await.unwrap();
        let first = Uuid::new_v4();
        sqlx::query("INSERT INTO webauthn_credentials (id, user_id, credential_id, credential_json) VALUES ($1, $2, $3, '{}')")
            .bind(first).bind(owner).bind(first.to_string()).execute(&pool).await.unwrap();
        let mut transaction = pool.begin().await.unwrap();
        let account = locked_account(&mut transaction, owner, 0).await.unwrap();
        assert!(account.recovery_enabled);
        assert!(matches!(
            remove_credential(&mut transaction, owner, 0, first, false).await,
            Err(AuthError::InvalidInput(_))
        ));
        transaction.rollback().await.unwrap();
        let second = Uuid::new_v4();
        sqlx::query("INSERT INTO webauthn_credentials (id, user_id, credential_id, credential_json) VALUES ($1, $2, $3, '{}')")
            .bind(second).bind(owner).bind(second.to_string()).execute(&pool).await.unwrap();
        session::create_session(&pool, &owner, &session::generate_refresh_token().unwrap())
            .await
            .unwrap();
        let mut transaction = pool.begin().await.unwrap();
        let _owner = locked_account(&mut transaction, owner, 0).await.unwrap();
        assert_eq!(
            remove_credential(&mut transaction, owner, 0, first, false)
                .await
                .unwrap(),
            1
        );
        transaction.commit().await.unwrap();
        let remaining: i64 =
            sqlx::query_scalar("SELECT count(*) FROM webauthn_credentials WHERE user_id = $1")
                .bind(owner)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(remaining, 1);
        let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id = $1")
            .bind(owner)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(sessions, 0);
        let mut transaction = pool.begin().await.unwrap();
        assert!(matches!(
            locked_account(&mut transaction, owner, 0).await,
            Err(AuthError::InvalidToken)
        ));
        transaction.rollback().await.unwrap();
        // A current password independently preserves sign-in after the final key is removed.
        sqlx::query("UPDATE users SET password_hash = 'test-hash' WHERE id = $1")
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        let mut transaction = pool.begin().await.unwrap();
        let account = locked_account(&mut transaction, owner, 1).await.unwrap();
        assert_eq!(
            remove_credential(
                &mut transaction,
                owner,
                1,
                second,
                account.password_hash.is_some()
            )
            .await
            .unwrap(),
            2
        );
        transaction.commit().await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
    async fn concurrent_removals_cannot_remove_both_remaining_sign_in_methods() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(4)
            .connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
            .await
            .unwrap();
        let owner = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO users (id, email, display_name) VALUES ($1, $2, 'Concurrent management')",
        )
        .bind(owner)
        .bind(format!("concurrent-keys-{owner}@example.test"))
        .execute(&pool)
        .await
        .unwrap();
        let keys = [Uuid::new_v4(), Uuid::new_v4()];
        for key in keys {
            sqlx::query("INSERT INTO webauthn_credentials (id, user_id, credential_id, credential_json) VALUES ($1, $2, $3, '{}')")
                .bind(key).bind(owner).bind(key.to_string()).execute(&pool).await.unwrap();
        }
        let refresh = session::generate_refresh_token().unwrap();
        session::create_session(&pool, &owner, &refresh)
            .await
            .unwrap();
        let barrier = tokio::sync::Barrier::new(2);
        let pool_ref = &pool;
        let barrier_ref = &barrier;
        let remove = |key| async move {
            let mut transaction = pool_ref.begin().await.map_err(routes::database_error)?;
            barrier_ref.wait().await;
            let state = locked_account(&mut transaction, owner, 0).await?;
            let version = remove_credential(
                &mut transaction,
                owner,
                0,
                key,
                state.password_hash.is_some(),
            )
            .await?;
            transaction.commit().await.map_err(routes::database_error)?;
            Ok::<_, AuthError>(version)
        };
        let (first, second) = tokio::join!(remove(keys[0]), remove(keys[1]));
        assert!(matches!(
            (&first, &second),
            (Ok(1), Err(AuthError::InvalidToken)) | (Err(AuthError::InvalidToken), Ok(1))
        ));
        let remaining: i64 =
            sqlx::query_scalar("SELECT count(*) FROM webauthn_credentials WHERE user_id = $1")
                .bind(owner)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(remaining, 1);
        let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id = $1")
            .bind(owner)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(sessions, 0);
    }
}
