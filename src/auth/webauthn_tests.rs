//! Tests of `src/auth/webauthn.rs`.

use super::*;

fn test_webauthn() -> Webauthn {
    WebauthnBuilder::new(
        "localhost",
        &url::Url::parse("https://localhost").expect("valid test origin"),
    )
    .expect("valid test RP")
    .rp_name("test")
    .build()
    .expect("valid WebAuthn configuration")
}

#[test]
fn account_challenges_are_owner_version_bound_one_use_and_expiring() {
    let webauthn = test_webauthn();
    let store = ChallengeStore::new();
    let owner = Uuid::new_v4();
    let make = || {
        let (_, state) = webauthn.start_discoverable_authentication().unwrap();
        AccountChallenge::Authentication(AccountAuthenticationData {
            state,
            user_id: owner,
            auth_version: 7,
            action: super::super::passkeys::PasskeyAction::RecoveryKey {},
        })
    };
    let source = IpAddr::from([192, 0, 2, 1]);
    let id = store.store_account(make(), source).unwrap();
    assert!(store.take_account(&id, Uuid::new_v4(), 7).is_none());
    assert!(store.take_account(&id, owner, 6).is_none());
    assert!(matches!(
        store.take_account(&id, owner, 7),
        Some(AccountChallenge::Authentication(_))
    ));
    assert!(store.take_account(&id, owner, 7).is_none());
    let expired = store.store_account(make(), source).unwrap();
    store
        .challenges
        .write()
        .accounts
        .get_mut(&expired)
        .unwrap()
        .created_at = Instant::now() - CHALLENGE_TTL;
    assert!(store.take_account(&expired, owner, 7).is_none());
}

#[test]
fn replacement_registration_has_five_minutes_without_extending_other_challenges() {
    let webauthn = test_webauthn();
    let store = ChallengeStore::new();
    let owner = Uuid::new_v4();
    let source = IpAddr::from([192, 0, 2, 1]);
    let make = |replacement| {
        let (_, state) = webauthn
            .start_passkey_registration(owner, "owner@example.test", "Owner", None)
            .unwrap();
        AccountChallenge::Registration(AccountRegistrationData {
            state,
            user_id: owner,
            auth_version: 7,
            replacement,
        })
    };
    let ordinary = store.store_account(make(None), source).unwrap();
    let replacement = store
        .store_account(
            make(Some(ReplacementRegistration {
                target: Uuid::new_v4(),
                recovery_hash: "a".repeat(64),
            })),
            source,
        )
        .unwrap();
    for id in [&ordinary, &replacement] {
        store
            .challenges
            .write()
            .accounts
            .get_mut(id)
            .unwrap()
            .created_at = Instant::now() - Duration::from_secs(90);
    }
    // Admission pruning uses the same per-operation deadline as consumption.
    let latest = store.store_account(make(None), source).unwrap();
    assert!(store.take_account(&ordinary, owner, 7).is_none());
    assert!(
        store
            .take_account(&replacement, Uuid::new_v4(), 7)
            .is_none()
    );
    assert!(store.take_account(&replacement, owner, 6).is_none());
    assert!(store.take_account(&replacement, owner, 7).is_some());
    assert!(store.take_account(&replacement, owner, 7).is_none());
    assert!(store.take_account(&latest, owner, 7).is_some());
    let expired = store
        .store_account(
            make(Some(ReplacementRegistration {
                target: Uuid::new_v4(),
                recovery_hash: "a".repeat(64),
            })),
            source,
        )
        .unwrap();
    store
        .challenges
        .write()
        .accounts
        .get_mut(&expired)
        .unwrap()
        .created_at = Instant::now() - REPLACEMENT_TTL;
    assert!(store.take_account(&expired, owner, 7).is_none());
}

#[test]
fn account_challenges_share_capacity_across_operations_without_eviction() {
    let webauthn = test_webauthn();
    let store = ChallengeStore::new();
    let owner = Uuid::new_v4();
    let source = IpAddr::from([192, 0, 2, 1]);
    let mut ids = Vec::new();
    for _ in 0..MAX_CHALLENGES_PER_PRINCIPAL {
        let (_, state) = webauthn
            .start_passkey_registration(owner, "owner@example.test", "Owner", None)
            .unwrap();
        ids.push(
            store
                .store_account(
                    AccountChallenge::Registration(AccountRegistrationData {
                        state,
                        user_id: owner,
                        auth_version: 0,
                        replacement: None,
                    }),
                    source,
                )
                .unwrap(),
        );
    }
    let (_, state) = webauthn.start_discoverable_authentication().unwrap();
    assert!(
        store
            .store_account(
                AccountChallenge::Authentication(AccountAuthenticationData {
                    state,
                    user_id: owner,
                    auth_version: 0,
                    action: super::super::passkeys::PasskeyAction::Add {},
                }),
                source
            )
            .is_none()
    );
    for id in ids {
        assert!(store.take_account(&id, owner, 0).is_some());
    }
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

    let challenges = store.challenges.read();
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
        let stored = store.store_registration(state, user_id, email, "User".to_string(), source_ip);
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

    assert_eq!(store.challenges.read().len(), MAX_CHALLENGES);
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
    let challenges = store.challenges.read();
    assert_eq!(challenges.len(), MAX_CHALLENGES);
    assert!(challenges.registrations.contains_key(&victim_id));
    assert!(!challenges.registrations.contains_key(&old_attacker_id));
    assert!(challenges.registrations.contains_key(&replacement_id));
    drop(challenges);

    assert!(store.take_registration(&victim_id).is_some());
}

#[test]
fn anonymous_authentication_is_single_use_and_expires() {
    let webauthn = test_webauthn();
    let store = ChallengeStore::new();
    let (_, state) = webauthn.start_discoverable_authentication().unwrap();
    let address = IpAddr::from([192, 0, 2, 10]);
    let first = store.store_authentication(state.clone(), address).unwrap();
    let second = store.store_authentication(state, address).unwrap();
    assert_ne!(first, second);
    assert!(store.take_authentication(&first).is_some());
    assert!(store.take_authentication(&first).is_none());

    store
        .challenges
        .write()
        .authentications
        .get_mut(&second)
        .unwrap()
        .created_at = Instant::now() - CHALLENGE_TTL;
    assert!(store.take_authentication(&second).is_none());
    assert!(store.challenges.read().authentications.is_empty());
}

#[test]
fn anonymous_authentication_bounds_a_shared_source_without_eviction() {
    let webauthn = test_webauthn();
    let store = ChallengeStore::new();
    let (_, state) = webauthn.start_discoverable_authentication().unwrap();
    let mut ids = Vec::new();
    for index in 0..MAX_AUTHENTICATION_CHALLENGES_PER_IP {
        // IPv6 addresses within a /64 share the same admission budget.
        let address = format!("2001:db8:1:2::{:x}", index + 1).parse().unwrap();
        ids.push(store.store_authentication(state.clone(), address).unwrap());
    }
    assert!(
        store
            .store_authentication(state.clone(), "2001:db8:1:2::ffff".parse().unwrap())
            .is_none()
    );
    assert!(
        store
            .store_authentication(state, "2001:db8:1:3::1".parse().unwrap())
            .is_some()
    );
    for id in ids {
        assert!(store.take_authentication(&id).is_some());
    }
}

#[test]
fn anonymous_authentication_respects_shared_global_capacity() {
    let webauthn = test_webauthn();
    let store = ChallengeStore::new();
    let (_, registration) = webauthn
        .start_passkey_registration(Uuid::new_v4(), "owner@example.test", "Owner", None)
        .unwrap();
    let original = store
        .store_registration(
            registration,
            Uuid::new_v4(),
            "owner@example.test".into(),
            "Owner".into(),
            IpAddr::from([192, 0, 2, 1]),
        )
        .unwrap();
    let (_, authentication) = webauthn.start_discoverable_authentication().unwrap();
    for index in 0..(MAX_CHALLENGES - 1) {
        let address = IpAddr::from([
            198,
            18,
            u8::try_from(index / 256).unwrap(),
            u8::try_from(index % 256).unwrap(),
        ]);
        assert!(
            store
                .store_authentication(authentication.clone(), address)
                .is_some()
        );
    }
    assert!(
        store
            .store_authentication(authentication, IpAddr::from([203, 0, 113, 1]))
            .is_none()
    );
    assert_eq!(store.challenges.read().len(), MAX_CHALLENGES);
    assert!(store.take_registration(&original).is_some());
}
