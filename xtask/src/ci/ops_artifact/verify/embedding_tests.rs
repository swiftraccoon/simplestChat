use super::plan::Clock as _;
use super::{fixture::*, *};
use serde_json::Value;
use std::{fs, os::unix::fs::PermissionsExt, sync::Arc, time::Duration};

#[tokio::test]
async fn embedded_verification_returns_observation_without_publishing_to_stdout() {
    let fixture = Fixture::new(16);
    let clock = Arc::new(fixture::Clock::new());
    let mut fake = Fake::new(&fixture, clock.clone());
    let budget = super::super::namespace::Budget::default();
    let end = clock.now() + Duration::from_secs(1440);
    let retained = observe_retained(
        fixture.request.clone(),
        fixture.pin(),
        fixture.root.clone(),
        fixture.environment.clone(),
        budget.clone(),
        Window { clock, end },
        &mut fake,
    )
    .await
    .unwrap();
    let observation: Value = serde_json::from_slice(&retained.bytes).unwrap();
    assert_eq!(observation["kind"], "ops-retained-verification-observation");
    assert_eq!(observation["files"], 22);
    assert_eq!(fake.calls, 4);
    assert!(fake.replay.is_empty());
    assert!(
        fixture
            .output
            .join("verification-observation.json")
            .is_file()
    );
    assert_eq!(budget.count(), 0);
}

#[tokio::test]
async fn embedding_reserves_the_complete_verifier_and_outer_tail_before_file_access() {
    for seconds in [644, 645] {
        let fixture = Fixture::new(0);
        let clock = Arc::new(fixture::Clock::new());
        let mut fake = Fake::new(&fixture, clock.clone());
        let budget = super::super::namespace::Budget::default();
        let end = clock.now() + Duration::from_secs(seconds);
        let result = observe_retained(
            fixture.request.clone(),
            fixture.pin(),
            fixture.root.clone(),
            fixture.environment.clone(),
            budget.clone(),
            Window { clock, end },
            &mut fake,
        )
        .await;
        if seconds == 644 {
            assert_eq!(result.unwrap_err().code, 124);
            assert_eq!(fake.calls, 0);
            assert!(!fixture.output.exists());
        } else {
            result.unwrap();
            assert_eq!(fake.calls, 4);
        }
        assert!(fake.replay.is_empty());
        assert_eq!(budget.count(), 0);
    }
}

#[tokio::test]
async fn embedding_preserves_verification_deadline_and_existing_cancellation_owner() {
    let fixture = Fixture::new(0);
    let clock = Arc::new(fixture::Clock::new());
    let mut fake = Fake::new(&fixture, clock.clone());
    fake.advance_on_call = Some((0, Duration::from_secs(600)));
    let budget = super::super::namespace::Budget::default();
    let end = clock.now() + Duration::from_secs(1440);
    let result = observe_retained(
        fixture.request.clone(),
        fixture.pin(),
        fixture.root.clone(),
        fixture.environment.clone(),
        budget.clone(),
        Window { clock, end },
        &mut fake,
    )
    .await;
    assert_eq!(result.unwrap_err().code, 124);
    assert_eq!(fake.calls, 1);
    assert!(fake.replay.is_empty());
    assert!(!fixture.output.join("simplestchat-ops").exists());
    assert_eq!(budget.count(), 0);
}

#[tokio::test]
async fn retained_output_witness_refuses_identical_replacement_files_and_directories() {
    for replace_directory in [false, true] {
        let fixture = Fixture::new(0);
        let clock = Arc::new(fixture::Clock::new());
        let mut fake = Fake::new(&fixture, clock.clone());
        let budget = super::super::namespace::Budget::default();
        let end = clock.now() + Duration::from_secs(1440);
        let retained = observe_retained(
            fixture.request.clone(),
            fixture.pin(),
            fixture.root.clone(),
            fixture.environment.clone(),
            budget.clone(),
            Window { clock, end },
            &mut fake,
        )
        .await
        .unwrap();
        retained
            .witness
            .check(&budget, &RetainedCheckpoint::fixture())
            .unwrap();
        if replace_directory {
            let previous = fixture.root.join("previous-output");
            fs::rename(&fixture.output, &previous).unwrap();
            fs::create_dir(&fixture.output).unwrap();
            fs::set_permissions(&fixture.output, fs::Permissions::from_mode(0o700)).unwrap();
            for entry in fs::read_dir(previous).unwrap() {
                let entry = entry.unwrap();
                fs::rename(entry.path(), fixture.output.join(entry.file_name())).unwrap();
            }
        } else {
            let path = fixture.output.join("simplestchat-ops");
            let original = fs::read(&path).unwrap();
            fs::rename(&path, fixture.root.join("previous-binary")).unwrap();
            fs::write(&path, original).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert!(
            retained
                .witness
                .check(&budget, &RetainedCheckpoint::fixture())
                .is_err()
        );
        assert_eq!(budget.count(), 0);
        assert!(fake.replay.is_empty());
    }
}
