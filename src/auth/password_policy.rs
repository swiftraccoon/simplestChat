//! The password selection policy: the limits, the check beside them and, through
//! `cargo run -q -p xtask -- protocol-ts`, the browser's copy of the limits, so the
//! sign-up form and the server refuse the same passwords. Login never reapplies the
//! policy: existing credentials stay usable.

#![forbid(unsafe_code)]

use super::{common_passwords, password, types::AuthError};

/// The fewest NFC-normalized characters a new password may have.
pub(crate) const MIN_PASSWORD_CHARS: usize = 15;
/// The most NFC-normalized characters, counted as characters so scripts with multi-byte
/// letters are neither admitted short nor refused long.
pub(crate) const MAX_PASSWORD_CHARS: usize = 128;
/// Bounds the hashing input; 128 characters of any script fit within it.
pub(crate) const MAX_PASSWORD_BYTES: usize = 512;

/// Whether a selection sits within the character and byte limits.
fn password_length_ok(password: &str) -> bool {
    let normalized = password::normalize_selection(password);
    let characters = normalized.chars().count();
    (MIN_PASSWORD_CHARS..=MAX_PASSWORD_CHARS).contains(&characters)
        && password.len() <= MAX_PASSWORD_BYTES
}

/// Apply the selection policy at signup, change and recovery.
pub(crate) fn validate_new_password(value: &str) -> Result<(), AuthError> {
    if !password_length_ok(value) || value.chars().any(char::is_control) {
        return Err(AuthError::InvalidInput(
            "Password must be 15–128 characters without control characters",
        ));
    }
    if common_passwords::is_common(&password::normalize_selection(value)) {
        return Err(AuthError::InvalidInput("Choose a less common password"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_password_policy_counts_nfc_characters_and_allows_long_scripts() {
        for password in [
            "a".repeat(14),
            "e\u{301}".repeat(14),
            "x\n".repeat(8),
            "a".repeat(129),
        ] {
            assert!(validate_new_password(&password).is_err());
        }
        for password in ["a".repeat(15), "e\u{301}".repeat(15), "語".repeat(128)] {
            assert!(validate_new_password(&password).is_ok());
        }
    }

    #[test]
    fn the_refusal_names_the_limits_it_applies() {
        let Err(AuthError::InvalidInput(message)) = validate_new_password("short") else {
            panic!("a short password must be refused");
        };
        assert!(message.contains(&format!("{MIN_PASSWORD_CHARS}–{MAX_PASSWORD_CHARS}")));
    }
}
