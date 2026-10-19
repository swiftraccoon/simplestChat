//! Secret-bearing text that never reaches logs or debug output.
//!
//! Passwords, tokens and shared secrets are wrapped where they enter the
//! process (wire fields, environment) and exposed only where they are consumed.
//! `Debug` prints a fixed marker, so a derived `Debug` on a message or a
//! configuration struct cannot leak the value.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A string whose `Debug` output is redacted. Serializes transparently.
#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretString(String);

impl SecretString {
    /// The protected value. Call this only where the value is consumed.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretString([REDACTED])")
    }
}

impl From<String> for SecretString {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for SecretString {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::SecretString;

    #[test]
    fn debug_output_never_contains_the_value() {
        let secret = SecretString::from("hunter2-the-secret");
        let debug = format!("{secret:?}");
        assert!(!debug.contains("hunter2"));
        assert_eq!(debug, "SecretString([REDACTED])");
        assert_eq!(secret.expose_secret(), "hunter2-the-secret");
    }

    #[test]
    fn serialization_is_the_plain_string() {
        let secret: SecretString = serde_json::from_str("\"token-value\"").unwrap();
        assert_eq!(secret.expose_secret(), "token-value");
        assert_eq!(serde_json::to_string(&secret).unwrap(), "\"token-value\"");
        let nested: Option<SecretString> = serde_json::from_str("null").unwrap();
        assert!(nested.is_none());
    }
}
