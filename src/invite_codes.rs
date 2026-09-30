#![forbid(unsafe_code)]
//! Invitation codes: 32 symbols from a 32-symbol alphabet (160 bits), no
//! letters that read like digits, lowercase so a code survives being read aloud.

use rand::{TryRng, rngs::SysRng};
use sha2::{Digest, Sha256};

pub const CODE_LEN: usize = 32;
const ALPHABET: &[u8; 32] = b"abcdefghjkmnpqrstuvwxyz023456789";

/// A fresh code from the system's randomness.
pub fn generate() -> anyhow::Result<String> {
    let mut bytes = [0u8; CODE_LEN];
    SysRng
        .try_fill_bytes(&mut bytes)
        .map_err(|error| anyhow::anyhow!("invite code generation failed: {error}"))?;
    Ok(bytes
        .iter()
        .map(|byte| char::from(ALPHABET[usize::from(byte & 31)]))
        .collect())
}

/// Whether text has the shape of a code this server issues.
pub fn is_valid(code: &str) -> bool {
    code.len() == CODE_LEN && code.bytes().all(|byte| ALPHABET.contains(&byte))
}

/// Database lookup key for a normalized high-entropy capability. It is never
/// returned by the API. Unlike a password, this random token has no guessable
/// human vocabulary; its digest needs no slow KDF or rotating server key.
pub fn digest(code: &str) -> String {
    hex::encode(Sha256::digest(code.as_bytes()))
}

/// A code as typed or pasted: trimmed and lowercased, or `None` when it could
/// never match.
pub fn normalize(code: &str) -> Option<String> {
    let code = code.trim().to_ascii_lowercase();
    is_valid(&code).then_some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_random_160_bit_secrets_and_normalize_typed_input() {
        let first = generate().unwrap();
        let second = generate().unwrap();
        assert_ne!(first, second);
        assert!(is_valid(&first));
        assert_eq!(first.len(), 32);
        assert!(!is_valid(&"a".repeat(20)));
        assert_eq!(
            digest("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            normalize(&format!("  {}  ", first.to_ascii_uppercase())).as_deref(),
            Some(first.as_str())
        );
        assert!(!is_valid("short"));
        assert!(!is_valid(&"a".repeat(21)));
        assert!(
            !is_valid(&"1".repeat(20)),
            "1 reads like l and is not issued"
        );
        assert!(normalize("abcdefghjkmnpqrstuvw!").is_none());
    }

    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
    async fn database_secret_migration_invalidates_only_invitations_and_rebuilds_receipt_key() {
        let pool =
            sqlx::PgPool::connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
                .await
                .unwrap();
        let mut transaction = pool.begin().await.unwrap();
        // Transaction-local temporary tables shadow no persistent data; rollback
        // drops all fixtures, including the migration's DDL.
        sqlx::query("SET LOCAL search_path TO pg_temp, pg_catalog")
            .execute(&mut *transaction)
            .await
            .unwrap();
        sqlx::raw_sql("CREATE TEMP TABLE invites (code VARCHAR(32) PRIMARY KEY);
            CREATE TEMP TABLE invite_redemptions (code VARCHAR(32) REFERENCES invites(code) ON DELETE CASCADE, user_id UUID NOT NULL, PRIMARY KEY(code, user_id));
            CREATE TEMP TABLE room_roles (role SMALLINT);
            INSERT INTO room_roles VALUES(2);
            INSERT INTO invites VALUES('abcdefghjkmnpqrstuvwx');
            INSERT INTO invite_redemptions VALUES('abcdefghjkmnpqrstuvwx', gen_random_uuid());")
            .execute(&mut *transaction).await.unwrap();
        sqlx::raw_sql(include_str!("../migrations/021_invitation_secrets.sql"))
            .execute(&mut *transaction)
            .await
            .unwrap();
        for query in [
            "SELECT COUNT(*) FROM invites",
            "SELECT COUNT(*) FROM invite_redemptions",
        ] {
            let count: i64 = sqlx::query_scalar(query)
                .fetch_one(&mut *transaction)
                .await
                .unwrap();
            assert_eq!(
                count, 0,
                "outstanding capabilities are intentionally invalidated"
            );
        }
        let role: i16 = sqlx::query_scalar("SELECT role FROM room_roles")
            .fetch_one(&mut *transaction)
            .await
            .unwrap();
        assert_eq!(role, 2, "existing membership is untouched");
        let hash = digest(&generate().unwrap());
        let id: uuid::Uuid =
            sqlx::query_scalar("INSERT INTO invites(code_hash) VALUES($1) RETURNING id")
                .bind(&hash)
                .fetch_one(&mut *transaction)
                .await
                .unwrap();
        sqlx::query("INSERT INTO invite_redemptions(invite_hash,user_id) VALUES($1,$2)")
            .bind(&hash)
            .bind(uuid::Uuid::new_v4())
            .execute(&mut *transaction)
            .await
            .unwrap();
        sqlx::query("DELETE FROM invites WHERE id=$1")
            .bind(id)
            .execute(&mut *transaction)
            .await
            .unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM invite_redemptions")
            .fetch_one(&mut *transaction)
            .await
            .unwrap();
        assert_eq!(count, 0, "revocation cascades by digest after migration");
        transaction.rollback().await.unwrap();
    }
}
