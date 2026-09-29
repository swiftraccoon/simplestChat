#![forbid(unsafe_code)]
//! Invitation codes: 20 symbols from a 32-symbol alphabet (100 bits), no
//! letters that read like digits, lowercase so a code survives being read aloud.

use rand::{TryRng, rngs::SysRng};

pub const CODE_LEN: usize = 20;
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
    fn codes_are_twenty_alphabet_symbols_and_normalize_from_typed_text() {
        let first = generate().unwrap();
        let second = generate().unwrap();
        assert_ne!(first, second);
        assert!(is_valid(&first));
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
}
