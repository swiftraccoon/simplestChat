#![forbid(unsafe_code)]

// TURN credential generation for coturn time-limited credentials.
// Uses HMAC-SHA1 per the TURN REST API spec (coturn --use-auth-secret).

use base64::Engine;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha1 = Hmac<Sha1>;

/// TURN server configuration, loaded from environment variables.
#[derive(Clone)]
pub struct TurnConfig {
    /// TURN server URLs (e.g. ["turn:example.com:3478", "turns:example.com:5349"])
    pub urls: Vec<String>,
    /// Shared secret for generating time-limited credentials
    pub secret: String,
    /// Credential TTL in seconds (default: 10 minutes)
    pub ttl_secs: u64,
}

/// ICE server entry sent to clients
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IceServer {
    pub urls: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}

impl TurnConfig {
    /// Load from environment variables. Returns None if TURN_URLS is not set.
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let Some(urls_str) = std::env::var("TURN_URLS").ok() else {
            return Ok(None);
        };
        let secret = std::env::var("TURN_SECRET")
            .map_err(|_| anyhow::anyhow!("TURN_SECRET is required when TURN_URLS is set"))?;
        if secret.as_bytes().len() < 32 {
            anyhow::bail!("TURN_SECRET must contain at least 32 bytes");
        }
        let ttl_secs = std::env::var("TURN_TTL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(600);
        if !(60..=3600).contains(&ttl_secs) {
            anyhow::bail!("TURN_TTL must be between 60 and 3600 seconds");
        }

        let urls: Vec<String> = urls_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if urls.is_empty() {
            anyhow::bail!("TURN_URLS must contain at least one URL");
        }
        if urls
            .iter()
            .any(|url| !url.starts_with("turn:") && !url.starts_with("turns:"))
        {
            anyhow::bail!("TURN_URLS entries must use turn: or turns:");
        }

        Ok(Some(Self {
            urls,
            secret,
            ttl_secs,
        }))
    }

    /// Generate time-limited credentials with a fresh opaque identifier.
    ///
    /// coturn format: username = "expiry_timestamp:arbitrary_id"
    ///               credential = base64(HMAC-SHA1(secret, username))
    pub fn generate_credentials(&self) -> IceServer {
        let expiry = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            + self.ttl_secs;

        // The suffix is intentionally unrelated to the application account or
        // room. TURN logs should not become a cross-room identity trail.
        let username = format!("{expiry}:{}", uuid::Uuid::new_v4());

        let mut mac =
            HmacSha1::new_from_slice(self.secret.as_bytes()).expect("HMAC accepts any key size");
        mac.update(username.as_bytes());
        let credential =
            base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());

        IceServer {
            urls: self.urls.clone(),
            username: Some(username),
            credential: Some(credential),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_turn_credential_does_not_expose_secret() {
        let config = TurnConfig {
            urls: vec!["turn:turn.example:3478".to_string()],
            secret: "a-secure-random-secret-with-32-bytes".to_string(),
            ttl_secs: 600,
        };
        let credentials = config.generate_credentials();
        assert!(credentials.username.is_some());
        assert!(credentials.credential.is_some());
        assert_ne!(
            credentials.credential.as_deref(),
            Some(config.secret.as_str())
        );
        let username = credentials.username.as_deref().unwrap();
        let opaque_id = username.split_once(':').unwrap().1;
        assert!(uuid::Uuid::parse_str(opaque_id).is_ok());
        assert_ne!(
            config.generate_credentials().username,
            credentials.username,
            "each credential issuance must use a fresh unlinkable identifier"
        );
    }
}
