#![forbid(unsafe_code)]

use crate::auth::types::AuthError;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::{TryRng, rngs::SysRng};
use sha2::{Digest, Sha256};
use sqlx::{Executor, PgPool, Postgres, Transaction};
use uuid::Uuid;

const MAX_ACTIVE_SESSIONS_PER_USER: i64 = 32;
const REFRESH_SECRET_BYTES: usize = 32;
const REFRESH_TOKEN_NEW_FAMILY_PREFIX: &str = "v1n";
const REFRESH_TOKEN_LEGACY_FAMILY_PREFIX: &str = "v1l";
const ENCODED_REFRESH_SECRET_LEN: usize = 43;
const ENCODED_LEGACY_FAMILY_LEN: usize = 48;
// Permit an immediately concurrent request to lose the rotation race without
// treating it as theft. It receives no token and cannot extend this window.
pub(crate) const CONCURRENT_REFRESH_GRACE_SECONDS: i64 = 2;

pub(crate) struct RefreshToken {
    pub(crate) raw: String,
    token_hash: String,
    family_hash: String,
}

pub(crate) enum RefreshRotation {
    Rotated {
        user_id: Uuid,
        refresh_token: RefreshToken,
    },
    /// The exact predecessor was presented within the short concurrency grace
    /// period. It is rejected, but does not revoke the winner's successor.
    ConcurrentRequest,
    /// A predecessor outside the grace period (or an older family token) was
    /// replayed, and the current successor family has been revoked.
    ReuseDetected,
}

pub(crate) fn generate_refresh_token() -> Result<RefreshToken, AuthError> {
    let family_secret = random_secret()?;
    build_refresh_token(&family_secret)
}

pub fn hash_token(raw: &str) -> String {
    hash_bytes(raw.as_bytes())
}

fn hash_bytes(value: &[u8]) -> String {
    hex::encode(Sha256::digest(value))
}

fn random_secret() -> Result<[u8; REFRESH_SECRET_BYTES], AuthError> {
    let mut secret = [0_u8; REFRESH_SECRET_BYTES];
    SysRng.try_fill_bytes(&mut secret).map_err(|error| {
        AuthError::DatabaseError(format!("Secure refresh-token generation failed: {error}"))
    })?;
    Ok(secret)
}

/// The token remains opaque to clients. The generation secret comes first so
/// log truncation is less likely to expose the stable family capability.
fn build_refresh_token(family_secret: &[u8]) -> Result<RefreshToken, AuthError> {
    let generation_secret = random_secret()?;
    let prefix = match family_secret.len() {
        REFRESH_SECRET_BYTES => REFRESH_TOKEN_NEW_FAMILY_PREFIX,
        36 => REFRESH_TOKEN_LEGACY_FAMILY_PREFIX,
        length => {
            return Err(AuthError::DatabaseError(format!(
                "Invalid refresh-token family secret length: {length}"
            )));
        }
    };
    let raw = format!(
        "{prefix}{}{}",
        URL_SAFE_NO_PAD.encode(generation_secret),
        URL_SAFE_NO_PAD.encode(family_secret)
    );
    Ok(RefreshToken {
        token_hash: hash_token(&raw),
        family_hash: hash_bytes(family_secret),
        raw,
    })
}

/// Return the stable secret carried by a versioned token. An existing UUID
/// token is itself the family secret, which makes migration session-preserving.
fn family_secret(raw_token: &str) -> Option<Vec<u8>> {
    if raw_token.is_empty()
        || raw_token.len() > 128
        || !raw_token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return None;
    }

    let (encoded_generation, encoded_family, expected_family_len) =
        if let Some(payload) = raw_token.strip_prefix(REFRESH_TOKEN_NEW_FAMILY_PREFIX) {
            if payload.len() != ENCODED_REFRESH_SECRET_LEN * 2 {
                return None;
            }
            let (generation, family) = payload.split_at(ENCODED_REFRESH_SECRET_LEN);
            (generation, family, REFRESH_SECRET_BYTES)
        } else if let Some(payload) = raw_token.strip_prefix(REFRESH_TOKEN_LEGACY_FAMILY_PREFIX) {
            if payload.len() != ENCODED_REFRESH_SECRET_LEN + ENCODED_LEGACY_FAMILY_LEN {
                return None;
            }
            let (generation, family) = payload.split_at(ENCODED_REFRESH_SECRET_LEN);
            (generation, family, 36)
        } else {
            // The only refresh tokens issued before the versioned format were
            // canonical, lowercase, hyphenated UUID v4 strings.
            let legacy_uuid = Uuid::parse_str(raw_token).ok()?;
            if legacy_uuid.hyphenated().to_string() != raw_token {
                return None;
            }
            return Some(raw_token.as_bytes().to_vec());
        };

    let generation = URL_SAFE_NO_PAD.decode(encoded_generation).ok()?;
    let family = URL_SAFE_NO_PAD.decode(encoded_family).ok()?;
    if generation.len() != REFRESH_SECRET_BYTES || family.len() != expected_family_len {
        return None;
    }
    Some(family)
}

#[cfg(test)]
pub(crate) async fn create_session(
    pool: &PgPool,
    user_id: &Uuid,
    refresh_token: &RefreshToken,
) -> Result<(), AuthError> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|error| AuthError::DatabaseError(error.to_string()))?;
    create_session_with(&mut transaction, user_id, refresh_token).await?;
    transaction
        .commit()
        .await
        .map_err(|error| AuthError::DatabaseError(error.to_string()))
}

pub(crate) async fn create_session_with(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    refresh_token: &RefreshToken,
) -> Result<(), AuthError> {
    // Serialize login/session-cap maintenance per account. Refresh rotation
    // takes the same lock before mutating a session, so every path follows the
    // users->sessions lock order and the cap remains exact under concurrency.
    sqlx::query("SELECT id FROM users WHERE id = $1 FOR NO KEY UPDATE")
        .bind(user_id)
        .execute(&mut **transaction)
        .await
        .map_err(|error| AuthError::DatabaseError(error.to_string()))?;
    let new_session_id = insert_session_with(&mut **transaction, user_id, refresh_token).await?;

    // A user who knows valid credentials must not be able to grow the sessions
    // table without bound by repeatedly logging in. Keep the newest sessions,
    // and remove expired rows for this user in the same transaction.
    sqlx::query(
        "DELETE FROM sessions
         WHERE user_id = $1
           AND id <> $2
           AND (
             expires_at <= now()
             OR id IN (
               SELECT id
               FROM sessions
               WHERE user_id = $1 AND id <> $2 AND expires_at > now()
               ORDER BY created_at DESC, id DESC
               OFFSET $3
             )
           )",
    )
    .bind(user_id)
    .bind(new_session_id)
    .bind(MAX_ACTIVE_SESSIONS_PER_USER - 1)
    .execute(&mut **transaction)
    .await
    .map_err(|error| AuthError::DatabaseError(error.to_string()))?;
    Ok(())
}

async fn insert_session_with<'e, E>(
    executor: E,
    user_id: &Uuid,
    refresh_token: &RefreshToken,
) -> Result<Uuid, AuthError>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_scalar(
        "INSERT INTO sessions (
             user_id, refresh_token_hash, refresh_token_family_hash, expires_at
         ) VALUES ($1, $2, $3, clock_timestamp() + interval '7 days')
         RETURNING id",
    )
    .bind(user_id)
    .bind(&refresh_token.token_hash)
    .bind(&refresh_token.family_hash)
    .fetch_one(executor)
    .await
    .map_err(|e| AuthError::DatabaseError(e.to_string()))
}

pub(crate) async fn rotate_refresh_token_with(
    transaction: &mut Transaction<'_, Postgres>,
    raw_token: &str,
) -> Result<RefreshRotation, AuthError> {
    let token_hash = hash_token(raw_token);
    let family_secret = family_secret(raw_token).ok_or(AuthError::InvalidToken)?;
    let family_hash = hash_bytes(&family_secret);

    // Resolve the account without taking a session-row lock. Both current-token
    // and family lookups use 256-bit hashes and return the same external error,
    // so random input cannot act as a practical account-existence oracle.
    let candidate_user_id: Uuid = sqlx::query_scalar(
        "SELECT user_id
         FROM sessions
         WHERE expires_at > clock_timestamp()
           AND (
             refresh_token_hash = $1
             OR refresh_token_family_hash = $2
           )
         ORDER BY (refresh_token_hash = $1) DESC
         LIMIT 1",
    )
    .bind(&token_hash)
    .bind(&family_hash)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AuthError::DatabaseError(error.to_string()))?
    .ok_or(AuthError::InvalidToken)?;

    let locked_user: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM users WHERE id = $1 FOR NO KEY UPDATE")
            .bind(candidate_user_id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(|error| AuthError::DatabaseError(error.to_string()))?;
    if locked_user.is_none() {
        return Err(AuthError::InvalidToken);
    }

    // Re-check after acquiring the per-user lock. All login and refresh paths
    // use users->sessions ordering, so concurrent rotations serialize without
    // lock inversion and a rotation never changes the session count.
    let current_session = sqlx::query_as::<_, (Uuid, Option<String>)>(
        "SELECT id, refresh_token_family_hash
         FROM sessions
         WHERE refresh_token_hash = $1
           AND user_id = $2
           AND expires_at > clock_timestamp()
         FOR UPDATE",
    )
    .bind(&token_hash)
    .bind(candidate_user_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AuthError::DatabaseError(error.to_string()))?;

    if let Some((session_id, stored_family_hash)) = current_session {
        if stored_family_hash
            .as_deref()
            .is_some_and(|stored_hash| stored_hash != family_hash)
        {
            return Err(AuthError::InvalidToken);
        }

        let successor = build_refresh_token(&family_secret)?;
        sqlx::query(
            "UPDATE sessions
             SET refresh_token_hash = $1,
                 refresh_token_family_hash = $2,
                 previous_refresh_token_hash = $3,
                 refresh_token_rotated_at = clock_timestamp(),
                 expires_at = clock_timestamp() + interval '7 days'
             WHERE id = $4",
        )
        .bind(&successor.token_hash)
        .bind(&successor.family_hash)
        .bind(&token_hash)
        .bind(session_id)
        .execute(&mut **transaction)
        .await
        .map_err(|error| AuthError::DatabaseError(error.to_string()))?;

        return Ok(RefreshRotation::Rotated {
            user_id: candidate_user_id,
            refresh_token: successor,
        });
    }

    let family_session = sqlx::query_as::<_, (Uuid, bool)>(
        "SELECT id,
                COALESCE(
                    previous_refresh_token_hash = $3
                    AND refresh_token_rotated_at
                        > clock_timestamp() - ($4::bigint * interval '1 second'),
                    false
                ) AS exact_predecessor_in_grace
         FROM sessions
         WHERE refresh_token_family_hash = $1
           AND user_id = $2
           AND expires_at > clock_timestamp()
         FOR UPDATE",
    )
    .bind(&family_hash)
    .bind(candidate_user_id)
    .bind(&token_hash)
    .bind(CONCURRENT_REFRESH_GRACE_SECONDS)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|error| AuthError::DatabaseError(error.to_string()))?
    .ok_or(AuthError::InvalidToken)?;

    if family_session.1 {
        return Ok(RefreshRotation::ConcurrentRequest);
    }

    sqlx::query("DELETE FROM sessions WHERE id = $1")
        .bind(family_session.0)
        .execute(&mut **transaction)
        .await
        .map_err(|error| AuthError::DatabaseError(error.to_string()))?;
    Ok(RefreshRotation::ReuseDetected)
}

/// Revoke the family represented by either its current token or any predecessor.
/// Logout remains idempotent, so an unknown or malformed token is not an error.
pub async fn delete_session_by_token(pool: &PgPool, raw_token: &str) -> Result<bool, AuthError> {
    let token_hash = hash_token(raw_token);
    let Some(family_secret) = family_secret(raw_token) else {
        return Ok(false);
    };
    let family_hash = hash_bytes(&family_secret);
    let result = sqlx::query(
        "DELETE FROM sessions
         WHERE refresh_token_hash = $1
            OR refresh_token_family_hash = $2",
    )
    .bind(token_hash)
    .bind(family_hash)
    .execute(pool)
    .await
    .map_err(|e| AuthError::DatabaseError(e.to_string()))?;
    Ok(result.rows_affected() > 0)
}

pub fn spawn_expired_cleanup(pool: &PgPool) {
    let pool_clone = pool.clone();
    tokio::spawn(async move {
        let _ = cleanup_expired(&pool_clone).await;
    });
}

pub async fn delete_user_sessions(pool: &PgPool, user_id: &Uuid) -> Result<(), AuthError> {
    sqlx::query("DELETE FROM sessions WHERE user_id = $1")
        .bind(user_id)
        .execute(pool)
        .await
        .map_err(|e| AuthError::DatabaseError(e.to_string()))?;
    Ok(())
}

pub async fn cleanup_expired(pool: &PgPool) -> Result<u64, AuthError> {
    let result = sqlx::query("DELETE FROM sessions WHERE expires_at < now()")
        .execute(pool)
        .await
        .map_err(|e| AuthError::DatabaseError(e.to_string()))?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_token_hashes_remain_lowercase_sha256_hex() {
        // SHA-256 standard known-answer vectors, independent of token generation.
        assert_eq!(
            hash_token(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hash_token("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn fixed_v1_refresh_fixture_preserves_base64url_and_family_hashes() {
        // Frozen pre-migration v1n format: 32 0xff generation bytes, then
        // 32 0xfb family bytes. Expected hashes were independently checked
        // with Node's crypto implementation, not this crate's encoder/hasher.
        const GENERATION: &str = "__________________________________________8";
        const FAMILY: &str = "-_v7-_v7-_v7-_v7-_v7-_v7-_v7-_v7-_v7-_v7-_s";
        let raw = format!("v1n{GENERATION}{FAMILY}");
        assert_eq!(URL_SAFE_NO_PAD.encode([0xff; 32]), GENERATION);
        assert_eq!(URL_SAFE_NO_PAD.encode([0xfb; 32]), FAMILY);
        assert_eq!(family_secret(&raw).unwrap(), [0xfb; 32]);
        assert_eq!(
            hash_token(&raw),
            "fb8e7de9c3c058fd86581ba30ca82e14e7ce146e8ba82b2441c1e36e63aa4e7c"
        );
        let successor = build_refresh_token(&[0xfb; 32]).unwrap();
        assert!(successor.raw.ends_with(FAMILY));
        assert_eq!(
            successor.family_hash,
            "456a04986c2572de19b058ef2ef20b0077017bcdb15819af052eb9d5d9b8e504"
        );
        assert!(family_secret(&format!("{raw}=")).is_none());
        assert!(family_secret(&raw.replace('_', "/")).is_none());
    }

    async fn rotate_and_commit(
        pool: &PgPool,
        raw_token: &str,
    ) -> Result<RefreshRotation, AuthError> {
        let mut transaction = pool
            .begin()
            .await
            .map_err(|error| AuthError::DatabaseError(error.to_string()))?;
        let outcome = rotate_refresh_token_with(&mut transaction, raw_token).await?;
        transaction
            .commit()
            .await
            .map_err(|error| AuthError::DatabaseError(error.to_string()))?;
        Ok(outcome)
    }

    #[test]
    fn generated_tokens_are_high_entropy_opaque_and_hash_only() {
        let first = generate_refresh_token().unwrap();
        let second = generate_refresh_token().unwrap();

        assert_ne!(first.raw, second.raw);
        assert!(first.raw.starts_with(REFRESH_TOKEN_NEW_FAMILY_PREFIX));
        assert_eq!(
            first.raw.len(),
            REFRESH_TOKEN_NEW_FAMILY_PREFIX.len() + ENCODED_REFRESH_SECRET_LEN * 2
        );
        assert!(
            first
                .raw
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        );
        assert_eq!(first.token_hash.len(), 64);
        assert_eq!(first.family_hash.len(), 64);
        assert!(!first.token_hash.contains(&first.raw));
        assert!(!first.family_hash.contains(&first.raw));
        assert_eq!(
            family_secret(&first.raw).unwrap().len(),
            REFRESH_SECRET_BYTES
        );
    }

    #[test]
    fn successors_keep_only_the_family_capability() {
        let first = generate_refresh_token().unwrap();
        let family = family_secret(&first.raw).unwrap();
        let successor = build_refresh_token(&family).unwrap();

        assert_ne!(first.raw, successor.raw);
        assert_ne!(first.token_hash, successor.token_hash);
        assert_eq!(first.family_hash, successor.family_hash);
    }

    #[test]
    fn legacy_uuid_tokens_can_be_migrated_without_storing_them_raw() {
        let legacy = Uuid::new_v4().to_string();
        let family = family_secret(&legacy).unwrap();
        let successor = build_refresh_token(&family).unwrap();

        assert_eq!(hash_token(&legacy), successor.family_hash);
        assert_eq!(family_secret(&successor.raw).unwrap(), legacy.as_bytes());
        assert!(
            successor
                .raw
                .starts_with(REFRESH_TOKEN_LEGACY_FAMILY_PREFIX)
        );
        assert_eq!(
            successor.raw.len(),
            REFRESH_TOKEN_LEGACY_FAMILY_PREFIX.len()
                + ENCODED_REFRESH_SECRET_LEN
                + ENCODED_LEGACY_FAMILY_LEN
        );
    }

    #[test]
    fn malformed_versioned_tokens_have_no_family_capability() {
        let uppercase_legacy = Uuid::new_v4().to_string().to_ascii_uppercase();
        let malformed = [
            String::new(),
            "not-a-uuid".to_owned(),
            uppercase_legacy,
            "v1.invalid.invalid".to_owned(),
            format!("v1n{}", "A".repeat(ENCODED_REFRESH_SECRET_LEN * 2 - 1)),
            format!("v1n{}", "A".repeat(ENCODED_REFRESH_SECRET_LEN * 2 + 1)),
            format!(
                "v1l{}",
                "A".repeat(ENCODED_REFRESH_SECRET_LEN + ENCODED_LEGACY_FAMILY_LEN - 1)
            ),
            format!("v1x{}", "A".repeat(ENCODED_REFRESH_SECRET_LEN * 2)),
            format!("v2n{}", "A".repeat(ENCODED_REFRESH_SECRET_LEN * 2)),
        ];
        for raw in malformed {
            assert!(family_secret(&raw).is_none(), "{raw}");
        }
    }

    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
    async fn database_rotation_replay_grace_and_session_cap() {
        let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(8)
            .connect(&database_url)
            .await
            .unwrap();

        let cap_user_id: Uuid = sqlx::query_scalar(
            "INSERT INTO users (email, display_name)
             VALUES ($1, 'cap test') RETURNING id",
        )
        .bind(format!("cap-{}@example.test", Uuid::new_v4()))
        .fetch_one(&pool)
        .await
        .unwrap();
        for _ in 0..=MAX_ACTIVE_SESSIONS_PER_USER {
            let token = generate_refresh_token().unwrap();
            create_session(&pool, &cap_user_id, &token).await.unwrap();
        }
        let capped_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id = $1")
                .bind(cap_user_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(capped_count, MAX_ACTIVE_SESSIONS_PER_USER);

        let replay_user_id: Uuid = sqlx::query_scalar(
            "INSERT INTO users (email, display_name)
             VALUES ($1, 'replay test') RETURNING id",
        )
        .bind(format!("replay-{}@example.test", Uuid::new_v4()))
        .fetch_one(&pool)
        .await
        .unwrap();
        let predecessor = generate_refresh_token().unwrap();
        create_session(&pool, &replay_user_id, &predecessor)
            .await
            .unwrap();

        // Two requests racing with the same bearer serialize: one rotates and
        // one is rejected under grace without authenticating or revoking.
        let (first, second) = tokio::join!(
            rotate_and_commit(&pool, &predecessor.raw),
            rotate_and_commit(&pool, &predecessor.raw)
        );
        let mut successor_raw = None;
        let mut concurrent_requests = 0;
        for outcome in [first.unwrap(), second.unwrap()] {
            match outcome {
                RefreshRotation::Rotated { refresh_token, .. } => {
                    successor_raw = Some(refresh_token.raw)
                }
                RefreshRotation::ConcurrentRequest => concurrent_requests += 1,
                RefreshRotation::ReuseDetected => panic!("concurrent request revoked the family"),
            }
        }
        assert_eq!(concurrent_requests, 1);
        let successor_raw = successor_raw.expect("one request rotated");
        let replay_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id = $1")
                .bind(replay_user_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(replay_count, 1);

        // The grace boundary is strict: at two seconds the predecessor is
        // reuse, not a concurrent request, and revokes the live successor.
        sqlx::query(
            "UPDATE sessions
             SET refresh_token_rotated_at =
                 clock_timestamp() - ($2::bigint * interval '1 second')
             WHERE user_id = $1",
        )
        .bind(replay_user_id)
        .bind(CONCURRENT_REFRESH_GRACE_SECONDS)
        .execute(&pool)
        .await
        .unwrap();
        assert!(matches!(
            rotate_and_commit(&pool, &predecessor.raw).await.unwrap(),
            RefreshRotation::ReuseDetected
        ));
        let replay_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id = $1")
                .bind(replay_user_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(replay_count, 0);
        assert!(matches!(
            rotate_and_commit(&pool, &successor_raw).await,
            Err(AuthError::InvalidToken)
        ));

        // Grace applies only to the immediate predecessor. An older consumed
        // token revokes the family even when the latest rotation was recent.
        let older_user_id: Uuid = sqlx::query_scalar(
            "INSERT INTO users (email, display_name)
             VALUES ($1, 'older replay test') RETURNING id",
        )
        .bind(format!("older-{}@example.test", Uuid::new_v4()))
        .fetch_one(&pool)
        .await
        .unwrap();
        let oldest = generate_refresh_token().unwrap();
        create_session(&pool, &older_user_id, &oldest)
            .await
            .unwrap();
        let middle = match rotate_and_commit(&pool, &oldest.raw).await.unwrap() {
            RefreshRotation::Rotated { refresh_token, .. } => refresh_token,
            _ => panic!("oldest token did not rotate"),
        };
        assert!(matches!(
            rotate_and_commit(&pool, &middle.raw).await.unwrap(),
            RefreshRotation::Rotated { .. }
        ));
        assert!(matches!(
            rotate_and_commit(&pool, &oldest.raw).await.unwrap(),
            RefreshRotation::ReuseDetected
        ));

        // A random token cannot identify or revoke an unrelated family.
        let oracle_user_id: Uuid = sqlx::query_scalar(
            "INSERT INTO users (email, display_name)
             VALUES ($1, 'oracle test') RETURNING id",
        )
        .bind(format!("oracle-{}@example.test", Uuid::new_v4()))
        .fetch_one(&pool)
        .await
        .unwrap();
        let current = generate_refresh_token().unwrap();
        create_session(&pool, &oracle_user_id, &current)
            .await
            .unwrap();
        let random = generate_refresh_token().unwrap();
        assert!(matches!(
            rotate_and_commit(&pool, &random.raw).await,
            Err(AuthError::InvalidToken)
        ));
        let oracle_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id = $1")
                .bind(oracle_user_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(oracle_count, 1);

        // Existing UUID sessions survive migration and gain the same replay
        // behavior after their first rotation.
        let legacy_user_id: Uuid = sqlx::query_scalar(
            "INSERT INTO users (email, display_name)
             VALUES ($1, 'legacy test') RETURNING id",
        )
        .bind(format!("legacy-{}@example.test", Uuid::new_v4()))
        .fetch_one(&pool)
        .await
        .unwrap();
        let legacy_raw = Uuid::new_v4().to_string();
        let legacy_hash = hash_token(&legacy_raw);
        sqlx::query(
            "INSERT INTO sessions (
                 user_id, refresh_token_hash, refresh_token_family_hash, expires_at
             ) VALUES ($1, $2, $2, clock_timestamp() + interval '7 days')",
        )
        .bind(legacy_user_id)
        .bind(&legacy_hash)
        .execute(&pool)
        .await
        .unwrap();
        let legacy_successor = match rotate_and_commit(&pool, &legacy_raw).await.unwrap() {
            RefreshRotation::Rotated { refresh_token, .. } => refresh_token,
            _ => panic!("legacy current token did not rotate"),
        };
        assert_eq!(legacy_successor.family_hash, legacy_hash);

        sqlx::query("DELETE FROM users WHERE id = ANY($1)")
            .bind(
                &[
                    cap_user_id,
                    replay_user_id,
                    older_user_id,
                    oracle_user_id,
                    legacy_user_id,
                ][..],
            )
            .execute(&pool)
            .await
            .unwrap();
    }
}
