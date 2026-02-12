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
#[derive(Debug, Clone)]
pub struct TurnConfig {
    /// TURN server URLs (e.g. ["turn:example.com:3478", "turns:example.com:5349"])
    pub urls: Vec<String>,
    /// Shared secret for generating time-limited credentials
    pub secret: String,
    /// Credential TTL in seconds (default: 24h)
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
    /// Load from environment variables. Returns None if TURN_URL is not set.
    pub fn from_env() -> Option<Self> {
        let urls_str = std::env::var("TURN_URLS").ok()?;
        let secret = std::env::var("TURN_SECRET").ok()?;
        let ttl_secs = std::env::var("TURN_TTL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(86400); // 24 hours default

        let urls: Vec<String> = urls_str.split(',').map(|s| s.trim().to_string()).collect();

        Some(Self { urls, secret, ttl_secs })
    }

    /// Generate time-limited credentials for a participant.
    ///
    /// coturn format: username = "expiry_timestamp:arbitrary_id"
    ///               credential = base64(HMAC-SHA1(secret, username))
    pub fn generate_credentials(&self, participant_id: &str) -> IceServer {
        let expiry = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            + self.ttl_secs;

        let username = format!("{expiry}:{participant_id}");

        let mut mac =
            HmacSha1::new_from_slice(self.secret.as_bytes()).expect("HMAC accepts any key size");
        mac.update(username.as_bytes());
        let credential = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());

        IceServer {
            urls: self.urls.clone(),
            username: Some(username),
            credential: Some(credential),
        }
    }
}
