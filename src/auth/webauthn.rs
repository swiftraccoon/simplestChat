#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};
use url::Url;
use std::sync::Arc;
use webauthn_rs::prelude::*;
use tracing::info;

pub struct RegistrationData {
    pub state: PasskeyRegistration,
    pub user_id: uuid::Uuid,
    pub email: String,
    pub display_name: String,
}

pub struct ChallengeStore {
    registrations: RwLock<HashMap<String, (RegistrationData, Instant)>>,
    authentications: RwLock<HashMap<String, (PasskeyAuthentication, Instant)>>,
}

const CHALLENGE_TTL: Duration = Duration::from_secs(60);
const MAX_CHALLENGES: usize = 10_000;

impl ChallengeStore {
    pub fn new() -> Self {
        Self {
            registrations: RwLock::new(HashMap::new()),
            authentications: RwLock::new(HashMap::new()),
        }
    }

    pub fn store_registration(&self, key: &str, state: PasskeyRegistration, user_id: uuid::Uuid, email: String, display_name: String) -> bool {
        let mut map = self.registrations.write().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        map.retain(|_, (_, created)| now.duration_since(*created) < CHALLENGE_TTL);
        if map.len() >= MAX_CHALLENGES {
            return false;
        }
        let data = RegistrationData { state, user_id, email, display_name };
        map.insert(key.to_string(), (data, now));
        true
    }

    pub fn take_registration(&self, key: &str) -> Option<RegistrationData> {
        let mut map = self.registrations.write().unwrap_or_else(|e| e.into_inner());
        let (data, created) = map.remove(key)?;
        if created.elapsed() > CHALLENGE_TTL {
            return None;
        }
        Some(data)
    }

    pub fn store_authentication(&self, key: &str, state: PasskeyAuthentication) -> bool {
        let mut map = self.authentications.write().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        map.retain(|_, (_, created)| now.duration_since(*created) < CHALLENGE_TTL);
        if map.len() >= MAX_CHALLENGES {
            return false;
        }
        map.insert(key.to_string(), (state, now));
        true
    }

    pub fn take_authentication(&self, key: &str) -> Option<PasskeyAuthentication> {
        let mut map = self.authentications.write().unwrap_or_else(|e| e.into_inner());
        let (state, created) = map.remove(key)?;
        if created.elapsed() > CHALLENGE_TTL {
            return None;
        }
        Some(state)
    }
}

pub fn init_webauthn() -> Option<(Webauthn, Arc<ChallengeStore>)> {
    let rp_id = std::env::var("WEBAUTHN_RP_ID").ok()?;
    let origin_str = std::env::var("WEBAUTHN_ORIGIN").ok()?;
    let origin = Url::parse(&origin_str).ok()?;

    let webauthn = WebauthnBuilder::new(&rp_id, &origin)
        .ok()?
        .rp_name("SimplestChat")
        .build()
        .ok()?;

    info!("WebAuthn/passkey authentication enabled (RP: {})", rp_id);

    Some((webauthn, Arc::new(ChallengeStore::new())))
}
