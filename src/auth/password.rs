#![forbid(unsafe_code)]

use argon2::{
    Argon2,
    password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash},
};
use icu_normalizer::ComposingNormalizer;

/// Account passwords use NFC for both selection and verification. Stored values
/// are ordinary Argon2 PHC strings; there is no alternate normalization policy.
pub fn normalize_selection(password: &str) -> String {
    ComposingNormalizer::new_nfc()
        .normalize(password)
        .into_owned()
}

pub fn hash_account_password(password: &str) -> Result<String, argon2::password_hash::Error> {
    hash_password(&normalize_selection(password))
}

pub fn verify_account_password(
    password: &str,
    hash: &str,
) -> Result<bool, argon2::password_hash::Error> {
    verify_password(&normalize_selection(password), hash)
}

pub fn hash_password(password: &str) -> Result<String, argon2::password_hash::Error> {
    let argon2 = Argon2::default();
    let hash = argon2.hash_password(password.as_bytes())?;
    Ok(hash.to_string())
}

pub fn verify_password(password: &str, hash: &str) -> Result<bool, argon2::password_hash::Error> {
    let parsed = PasswordHash::new(hash)?;
    match Argon2::default().verify_password(password.as_bytes(), &parsed) {
        Ok(()) => Ok(true),
        Err(argon2::password_hash::Error::PasswordInvalid) => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};

    #[test]
    fn account_passwords_use_one_nfc_policy_and_standard_phc_format() {
        let decomposed = "Cafe\u{301}-rides-bikes";
        let composed = "Café-rides-bikes";
        let current = hash_account_password(decomposed).unwrap();
        assert!(current.starts_with("$argon2id$"));
        assert!(verify_account_password(decomposed, &current).unwrap());
        assert!(verify_account_password(composed, &current).unwrap());
        assert!(!verify_account_password("different password", &current).unwrap());
        let ascii = hash_password("existing-ascii-password").unwrap();
        assert!(verify_account_password("existing-ascii-password", &ascii).unwrap());
    }

    #[test]
    fn standard_argon2id_phc_hash_verifies() {
        // RustCrypto argon2 0.5.3 tests/kat.rs:
        // reference_argon2id_v0x13_2_8_1 (password="password", salt="somesalt").
        // Its deliberately small KAT work factor is not our issuance policy.
        const REFERENCE_HASH: &str =
            "$argon2id$v=19$m=256,t=2,p=1$c29tZXNhbHQ$nf65EOgLrQMR/uIPnA4rEsF5h7TKyQwu9U1bMCHGi/4";
        assert!(verify_password("password", REFERENCE_HASH).unwrap());
        assert!(!verify_password("wrong-password", REFERENCE_HASH).unwrap());
    }

    #[test]
    fn test_hash_and_verify() {
        let password = "correct-horse-battery-staple";
        let hash = hash_password(password).unwrap();
        assert!(verify_password(password, &hash).unwrap());
        assert!(!verify_password("wrong-password", &hash).unwrap());
    }

    #[test]
    fn new_hashes_preserve_work_factors_and_use_fresh_random_salts() {
        let hash1 = hash_password("password1").unwrap();
        let hash2 = hash_password("password1").unwrap();
        assert_ne!(hash1, hash2);
        let first: Vec<_> = hash1.split('$').collect();
        let second: Vec<_> = hash2.split('$').collect();
        for fields in [&first, &second] {
            // Argon2 0.5.3 Params::DEFAULT and SaltString::generate policy.
            assert_eq!(fields.len(), 6);
            assert_eq!(&fields[..4], &["", "argon2id", "v=19", "m=19456,t=2,p=1"]);
            assert_eq!(STANDARD_NO_PAD.decode(fields[4]).unwrap().len(), 16);
            assert_eq!(STANDARD_NO_PAD.decode(fields[5]).unwrap().len(), 32);
        }
        assert_ne!(
            first[4], second[4],
            "equal passwords must receive new salts"
        );
        assert!(verify_password("password1", &hash1).unwrap());
        assert!(verify_password("password1", &hash2).unwrap());
    }

    #[test]
    fn test_empty_password() {
        let hash = hash_password("").unwrap();
        assert!(verify_password("", &hash).unwrap());
        assert!(!verify_password("not-empty", &hash).unwrap());
    }
}
