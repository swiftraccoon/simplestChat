#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tracing::info;
use url::Url;
use uuid::Uuid;
use webauthn_rs::prelude::*;

pub struct RegistrationData {
    pub state: PasskeyRegistration,
    pub user_id: Uuid,
    pub email: String,
    pub display_name: String,
}

pub struct AuthenticationData {
    pub state: PasskeyAuthentication,
    pub user_id: Uuid,
}

struct TimedChallenge<T> {
    data: T,
    created_at: Instant,
    source_ip: IpAddr,
}

pub struct ChallengeStore {
    challenges: RwLock<ChallengeMaps>,
}

#[derive(Default)]
struct ChallengeMaps {
    registrations: HashMap<String, TimedChallenge<RegistrationData>>,
    authentications: HashMap<String, TimedChallenge<AuthenticationData>>,
}

impl ChallengeMaps {
    fn retain_live(&mut self) {
        self.registrations
            .retain(|_, challenge| challenge.created_at.elapsed() < CHALLENGE_TTL);
        self.authentications
            .retain(|_, challenge| challenge.created_at.elapsed() < CHALLENGE_TTL);
    }

    fn len(&self) -> usize {
        self.registrations.len() + self.authentications.len()
    }
}

const CHALLENGE_TTL: Duration = Duration::from_secs(60);
// Email and display-name lengths are validated before anything reaches this
// store. These limits are a final, process-wide memory bound; request-level
// rate limiting should still be applied by the HTTP router.
const MAX_CHALLENGES: usize = 1_024;
const MAX_CHALLENGES_PER_PRINCIPAL: usize = 3;
const MAX_REGISTRATION_CHALLENGES_PER_IP: usize = 8;
const MAX_AUTHENTICATION_CHALLENGES_PER_IP: usize = 16;

impl ChallengeStore {
    pub fn new() -> Self {
        Self {
            challenges: RwLock::new(ChallengeMaps::default()),
        }
    }

    pub fn store_registration(
        &self,
        state: PasskeyRegistration,
        user_id: Uuid,
        email: String,
        display_name: String,
        source_ip: IpAddr,
    ) -> Option<String> {
        let source_ip = rate_limit_ip(source_ip);
        let mut challenges = self.challenges.write().unwrap_or_else(|e| e.into_inner());
        challenges.retain_live();

        let principal_at_limit = challenges
            .registrations
            .values()
            .filter(|challenge| challenge.data.email == email)
            .count()
            >= MAX_CHALLENGES_PER_PRINCIPAL;
        let source_at_limit = challenges
            .registrations
            .values()
            .filter(|challenge| challenge.source_ip == source_ip)
            .count()
            >= MAX_REGISTRATION_CHALLENGES_PER_IP;

        if principal_at_limit || source_at_limit || challenges.len() >= MAX_CHALLENGES {
            // Retrying the same ceremony owner may replace only that owner's
            // oldest challenge. Requiring both principal and source prevents a
            // caller who merely knows another account/email from evicting it.
            let replacement = challenges
                .registrations
                .iter()
                .filter(|(_, challenge)| {
                    challenge.data.email == email && challenge.source_ip == source_ip
                })
                .min_by_key(|(_, challenge)| challenge.created_at)
                .map(|(ceremony_id, _)| ceremony_id.clone());
            challenges.registrations.remove(&replacement?);
        }

        let ceremony_id = fresh_ceremony_id(&challenges.registrations);
        challenges.registrations.insert(
            ceremony_id.clone(),
            TimedChallenge {
                data: RegistrationData {
                    state,
                    user_id,
                    email,
                    display_name,
                },
                created_at: Instant::now(),
                source_ip,
            },
        );
        Some(ceremony_id)
    }

    pub fn take_registration(&self, ceremony_id: &str) -> Option<RegistrationData> {
        let mut challenges = self.challenges.write().unwrap_or_else(|e| e.into_inner());
        let challenge = challenges.registrations.remove(ceremony_id)?;
        (challenge.created_at.elapsed() < CHALLENGE_TTL).then_some(challenge.data)
    }

    pub fn store_authentication(
        &self,
        state: PasskeyAuthentication,
        user_id: Uuid,
        source_ip: IpAddr,
    ) -> Option<String> {
        let source_ip = rate_limit_ip(source_ip);
        let mut challenges = self.challenges.write().unwrap_or_else(|e| e.into_inner());
        challenges.retain_live();

        let principal_at_limit = challenges
            .authentications
            .values()
            .filter(|challenge| challenge.data.user_id == user_id)
            .count()
            >= MAX_CHALLENGES_PER_PRINCIPAL;
        let source_at_limit = challenges
            .authentications
            .values()
            .filter(|challenge| challenge.source_ip == source_ip)
            .count()
            >= MAX_AUTHENTICATION_CHALLENGES_PER_IP;

        if principal_at_limit || source_at_limit || challenges.len() >= MAX_CHALLENGES {
            let replacement = challenges
                .authentications
                .iter()
                .filter(|(_, challenge)| {
                    challenge.data.user_id == user_id && challenge.source_ip == source_ip
                })
                .min_by_key(|(_, challenge)| challenge.created_at)
                .map(|(ceremony_id, _)| ceremony_id.clone());
            challenges.authentications.remove(&replacement?);
        }

        let ceremony_id = fresh_ceremony_id(&challenges.authentications);
        challenges.authentications.insert(
            ceremony_id.clone(),
            TimedChallenge {
                data: AuthenticationData { state, user_id },
                created_at: Instant::now(),
                source_ip,
            },
        );
        Some(ceremony_id)
    }

    pub fn take_authentication(&self, ceremony_id: &str) -> Option<AuthenticationData> {
        let mut challenges = self.challenges.write().unwrap_or_else(|e| e.into_inner());
        let challenge = challenges.authentications.remove(ceremony_id)?;
        (challenge.created_at.elapsed() < CHALLENGE_TTL).then_some(challenge.data)
    }
}

fn rate_limit_ip(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V4(_) => address,
        IpAddr::V6(address) => {
            if let Some(address) = address.to_ipv4_mapped() {
                return IpAddr::V4(address);
            }
            let segments = address.segments();
            IpAddr::V6(std::net::Ipv6Addr::new(
                segments[0],
                segments[1],
                segments[2],
                segments[3],
                0,
                0,
                0,
                0,
            ))
        }
    }
}

impl Default for ChallengeStore {
    fn default() -> Self {
        Self::new()
    }
}

fn fresh_ceremony_id<T>(map: &HashMap<String, T>) -> String {
    loop {
        let candidate = Uuid::new_v4().to_string();
        if !map.contains_key(&candidate) {
            return candidate;
        }
    }
}

pub fn init_webauthn() -> anyhow::Result<Option<(Webauthn, Arc<ChallengeStore>)>> {
    let rp_id = std::env::var("WEBAUTHN_RP_ID").ok();
    let origin_str = std::env::var("WEBAUTHN_ORIGIN").ok();
    let (rp_id, origin_str) = match (rp_id, origin_str) {
        (None, None) => return Ok(None),
        (Some(rp_id), Some(origin)) => (rp_id, origin),
        _ => anyhow::bail!("WEBAUTHN_RP_ID and WEBAUTHN_ORIGIN must be configured together"),
    };
    let origin = Url::parse(&origin_str)
        .map_err(|_| anyhow::anyhow!("WEBAUTHN_ORIGIN must be a valid URL"))?;
    let secure_origin = origin.scheme() == "https"
        || (origin.scheme() == "http"
            && origin.host_str().is_some_and(|host| {
                host.eq_ignore_ascii_case("localhost")
                    || host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|address| address.is_loopback())
            }));
    if !secure_origin
        || !origin.username().is_empty()
        || origin.password().is_some()
        || origin.path() != "/"
        || origin.query().is_some()
        || origin.fragment().is_some()
    {
        anyhow::bail!(
            "WEBAUTHN_ORIGIN must be an HTTPS origin without a path (HTTP localhost is allowed for development)"
        );
    }

    let webauthn = WebauthnBuilder::new(&rp_id, &origin)
        .map_err(|error| anyhow::anyhow!("Invalid WebAuthn relying party: {error}"))?
        .rp_name("SimplestChat")
        .build()
        .map_err(|error| anyhow::anyhow!("Invalid WebAuthn configuration: {error}"))?;

    info!(rp_id, "WebAuthn/passkey authentication enabled");

    Ok(Some((webauthn, Arc::new(ChallengeStore::new()))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_webauthn() -> Webauthn {
        WebauthnBuilder::new(
            "localhost",
            &Url::parse("https://localhost").expect("valid test origin"),
        )
        .expect("valid test RP")
        .rp_name("test")
        .build()
        .expect("valid WebAuthn configuration")
    }

    #[test]
    fn registration_ceremonies_do_not_overwrite_each_other() {
        let webauthn = test_webauthn();
        let store = ChallengeStore::new();
        let email = "alice@example.com";
        let source_ip = IpAddr::from([192, 0, 2, 1]);
        let mut ids = Vec::new();

        for _ in 0..MAX_CHALLENGES_PER_PRINCIPAL {
            let user_id = Uuid::new_v4();
            let (_, state) = webauthn
                .start_passkey_registration(user_id, email, "Alice", None)
                .expect("registration challenge");
            ids.push(
                store
                    .store_registration(
                        state,
                        user_id,
                        email.to_string(),
                        "Alice".to_string(),
                        source_ip,
                    )
                    .expect("challenge should fit"),
            );
        }

        assert_eq!(
            ids.iter().collect::<std::collections::HashSet<_>>().len(),
            MAX_CHALLENGES_PER_PRINCIPAL
        );
        for id in ids {
            assert!(store.take_registration(&id).is_some());
        }
    }

    #[test]
    fn same_principal_and_source_retry_replaces_only_its_own_challenge() {
        let webauthn = test_webauthn();
        let store = ChallengeStore::new();
        let email = "alice@example.com";
        let source_ip = IpAddr::from([192, 0, 2, 1]);
        let mut original_ids = Vec::new();

        for _ in 0..MAX_CHALLENGES_PER_PRINCIPAL {
            let user_id = Uuid::new_v4();
            let (_, state) = webauthn
                .start_passkey_registration(user_id, email, "Alice", None)
                .expect("registration challenge");
            original_ids.push(
                store
                    .store_registration(
                        state,
                        user_id,
                        email.to_string(),
                        "Alice".to_string(),
                        source_ip,
                    )
                    .expect("challenge should fit"),
            );
        }

        let user_id = Uuid::new_v4();
        let (_, state) = webauthn
            .start_passkey_registration(user_id, email, "Alice", None)
            .expect("registration challenge");
        let replacement_id = store
            .store_registration(
                state,
                user_id,
                email.to_string(),
                "Alice".to_string(),
                source_ip,
            )
            .expect("same owner retry should replace one old challenge");

        let challenges = store
            .challenges
            .read()
            .unwrap_or_else(|error| error.into_inner());
        assert_eq!(challenges.registrations.len(), MAX_CHALLENGES_PER_PRINCIPAL);
        assert!(challenges.registrations.contains_key(&replacement_id));
        assert_eq!(
            original_ids
                .iter()
                .filter(|id| challenges.registrations.contains_key(*id))
                .count(),
            MAX_CHALLENGES_PER_PRINCIPAL - 1
        );
    }

    #[test]
    fn another_source_cannot_replace_a_principals_challenges() {
        let webauthn = test_webauthn();
        let store = ChallengeStore::new();
        let email = "victim@example.com";
        let victim_ip = IpAddr::from([192, 0, 2, 1]);
        let attacker_ip = IpAddr::from([198, 51, 100, 1]);
        let mut victim_ids = Vec::new();

        for _ in 0..MAX_CHALLENGES_PER_PRINCIPAL {
            let user_id = Uuid::new_v4();
            let (_, state) = webauthn
                .start_passkey_registration(user_id, email, "Victim", None)
                .expect("registration challenge");
            victim_ids.push(
                store
                    .store_registration(
                        state,
                        user_id,
                        email.to_string(),
                        "Victim".to_string(),
                        victim_ip,
                    )
                    .expect("victim challenge should fit"),
            );
        }

        let attacker_id = Uuid::new_v4();
        let (_, attacker_state) = webauthn
            .start_passkey_registration(attacker_id, email, "Victim", None)
            .expect("registration challenge");
        assert!(
            store
                .store_registration(
                    attacker_state,
                    attacker_id,
                    email.to_string(),
                    "Victim".to_string(),
                    attacker_ip,
                )
                .is_none()
        );
        for ceremony_id in victim_ids {
            assert!(store.take_registration(&ceremony_id).is_some());
        }
    }

    #[test]
    fn registration_ceremonies_are_limited_per_ipv6_prefix() {
        let webauthn = test_webauthn();
        let store = ChallengeStore::new();
        for index in 0..=MAX_REGISTRATION_CHALLENGES_PER_IP {
            let user_id = Uuid::new_v4();
            let email = format!("user-{index}@example.test");
            let (_, state) = webauthn
                .start_passkey_registration(user_id, &email, "User", None)
                .expect("registration challenge");
            let source_ip = format!("2001:db8:1:2::{:x}", index + 1).parse().unwrap();
            let stored =
                store.store_registration(state, user_id, email, "User".to_string(), source_ip);
            assert_eq!(stored.is_some(), index < MAX_REGISTRATION_CHALLENGES_PER_IP);
        }
    }

    #[test]
    fn attacker_saturation_cannot_evict_an_existing_ceremony() {
        let webauthn = test_webauthn();
        let store = ChallengeStore::new();
        let (_, state) = webauthn
            .start_passkey_registration(Uuid::new_v4(), "template@example.test", "Template", None)
            .expect("registration challenge");

        let victim_id = store
            .store_registration(
                state.clone(),
                Uuid::new_v4(),
                "victim@example.test".to_string(),
                "Victim".to_string(),
                IpAddr::from([192, 0, 2, 1]),
            )
            .expect("victim challenge should fit");

        let mut first_attacker = None;
        for index in 0..(MAX_CHALLENGES - 1) {
            let source_ip = IpAddr::from([
                198,
                18,
                u8::try_from(index / 256).unwrap(),
                u8::try_from(index % 256).unwrap(),
            ]);
            let email = format!("attacker-{index}@example.test");
            let ceremony_id = store
                .store_registration(
                    state.clone(),
                    Uuid::new_v4(),
                    email.clone(),
                    "Attacker".to_string(),
                    source_ip,
                )
                .expect("unique attacker entry should fit before capacity");
            if index == 0 {
                first_attacker = Some((ceremony_id, email, source_ip));
            }
        }

        assert_eq!(
            store
                .challenges
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .len(),
            MAX_CHALLENGES
        );
        assert!(
            store
                .store_registration(
                    state.clone(),
                    Uuid::new_v4(),
                    "overflow@example.test".to_string(),
                    "Overflow".to_string(),
                    IpAddr::from([203, 0, 113, 250]),
                )
                .is_none(),
            "a new source must receive backpressure at global capacity"
        );

        let (old_attacker_id, attacker_email, attacker_ip) = first_attacker.unwrap();
        let replacement_id = store
            .store_registration(
                state,
                Uuid::new_v4(),
                attacker_email,
                "Attacker".to_string(),
                attacker_ip,
            )
            .expect("an existing owner may replace its own entry at capacity");
        let challenges = store
            .challenges
            .read()
            .unwrap_or_else(|error| error.into_inner());
        assert_eq!(challenges.len(), MAX_CHALLENGES);
        assert!(challenges.registrations.contains_key(&victim_id));
        assert!(!challenges.registrations.contains_key(&old_attacker_id));
        assert!(challenges.registrations.contains_key(&replacement_id));
        drop(challenges);

        assert!(store.take_registration(&victim_id).is_some());
    }
}
