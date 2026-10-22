use super::{fixture::*, *};
use serde_json::{Value, json};
use std::{fs, sync::Arc, time::Duration};

async fn run(fixture: &Fixture, fake: &mut Fake) -> Result<()> {
    let budget = super::super::namespace::Budget::default();
    let context = plan::Context::new(fake.clock.clone());
    let result = execute::transaction(
        fixture.request.clone(),
        fixture.pin(),
        fixture.root.clone(),
        fixture.environment.clone(),
        budget.clone(),
        &context,
        fake,
    )
    .await;
    assert_eq!(
        budget.count(),
        0,
        "every admitted owner/reservation unwinds"
    );
    result
}

#[tokio::test]
async fn complete_flat_transaction_uses_original_bytes_all_payloads_and_fixed_four_calls() {
    for count in [0, 16] {
        let fixture = Fixture::new(count);
        let mut fake = Fake::new(&fixture, Arc::new(Clock::new()));
        run(&fixture, &mut fake).await.unwrap();
        assert_eq!(fake.calls, 4);
        let observation: Value = serde_json::from_slice(&fake.replay).unwrap();
        assert_eq!(observation["files"], count + 6);
        assert_eq!(observation["kind"], "ops-retained-verification-observation");
        for entry in fs::read_dir(&fixture.input).unwrap() {
            let entry = entry.unwrap();
            assert_eq!(
                fs::read(entry.path()).unwrap(),
                fs::read(fixture.output.join(entry.file_name())).unwrap()
            );
        }
        let public = String::from_utf8(fake.replay.clone()).unwrap();
        for secret in ["PRIVATE", "TOKEN", fixture.root.to_str().unwrap()] {
            assert!(!public.contains(secret));
        }
        assert!(
            fixture
                .output
                .join("verification-observation.json")
                .is_file()
        );
    }
}

#[tokio::test]
async fn every_selected_claim_mismatch_refuses_before_binary_or_output_admission() {
    let replacements = [
        ("repository", json!("another/project")),
        ("revision", json!("c".repeat(40))),
        ("packageVersion", json!("0.1.1")),
        ("protocolVersion", json!(2)),
        ("target", json!("x86_64-unknown-linux-gnu")),
        ("buildRunId", json!(124)),
        ("runAttempt", json!(3)),
    ];
    for (key, value) in replacements {
        let mut fixture = Fixture::new(0);
        fixture.wire["selection"][key] = value;
        fixture.save_request();
        fs::remove_file(fixture.input.join("simplestchat-ops")).unwrap();
        let mut fake = Fake::new(&fixture, Arc::new(Clock::new()));
        assert_eq!(run(&fixture, &mut fake).await.unwrap_err().code, 1, "{key}");
        assert_eq!(fake.calls, 0);
        assert!(!fixture.output.exists());
    }
}

#[tokio::test]
async fn token_only_admission_excludes_ambient_credentials_and_anonymous_keyring_fallback() {
    for mode in ["anonymous", "gh-token"] {
        let mut fixture = Fixture::new(0);
        fixture.wire["authentication"] = json!(mode);
        fixture.save_request();
        fixture.environment.remove(std::ffi::OsStr::new("GH_TOKEN"));
        fixture
            .environment
            .insert("GITHUB_TOKEN".into(), "PRIVATE-AMBIENT".into());
        let mut fake = Fake::new(&fixture, Arc::new(Clock::new()));
        assert_eq!(run(&fixture, &mut fake).await.unwrap_err().code, 1);
        assert_eq!(fake.calls, 0);
        assert!(!fixture.output.exists());
    }
}

#[test]
fn exact_request_rejects_duplicates_unknowns_unsafe_numbers_and_unicode_control_paths() {
    let fixture = Fixture::new(0);
    let original = bytes(&fixture.wire);
    assert!(request::Request::decode(&original, &super::super::sha(&original)).is_ok());
    for path in [
        "/tmp/unsafe\u{0085}path",
        "/tmp/unsafe\u{009f}path",
        "relative",
    ] {
        let mut wire = fixture.wire.clone();
        wire["output"] = json!(path);
        let raw = bytes(&wire);
        assert!(request::Request::decode(&raw, &super::super::sha(&raw)).is_err());
    }
    for raw in [
        b"{}".as_slice(),
        b"{\"schemaVersion\":1,\"schemaVersion\":1}",
        &[b'x'; 16 * 1024 + 1],
    ] {
        assert!(request::Request::decode(raw, &super::super::sha(raw)).is_err());
    }
    for count in [0u64, 9_007_199_254_740_992] {
        let mut wire = fixture.wire.clone();
        wire["selection"]["buildRunId"] = json!(count);
        let raw = bytes(&wire);
        assert!(request::Request::decode(&raw, &super::super::sha(&raw)).is_err());
    }
}

#[tokio::test]
async fn fake_successful_exit_does_not_replace_certificate_subject_or_api_policy() {
    for (index, pointer, replacement) in [
        (1, "/owner/id", json!(1)),
        (2, "/head_repository/id", json!(1)),
        (
            3,
            "/0/verificationResult/signature/certificate/sourceRepositoryIdentifier",
            json!(1),
        ),
        (
            3,
            "/0/verificationResult/statement/subject/2/digest/sha256",
            json!("0".repeat(64)),
        ),
        (
            3,
            "/0/verificationResult/signature/certificate/runnerEnvironment",
            json!("self-hosted"),
        ),
    ] {
        let fixture = Fixture::new(0);
        let mut fake = Fake::new(&fixture, Arc::new(Clock::new()));
        let mut output: Value = serde_json::from_slice(&fake.outputs[index]).unwrap();
        *output.pointer_mut(pointer).unwrap() = replacement;
        fake.outputs[index] = bytes(&output);
        assert_eq!(run(&fixture, &mut fake).await.unwrap_err().code, 1);
        assert_eq!(fake.calls, 4);
        assert!(!fixture.output.join("simplestchat-ops").exists());
        assert!(fake.replay.is_empty());
    }
}

#[tokio::test]
async fn nonzero_cli_remains_primary_over_valid_json_late_cancel_and_final_close_refusal() {
    let fixture = Fixture::new(0);
    let mut fake = Fake::new(&fixture, Arc::new(Clock::new()));
    fake.fail = Some((1, 42));
    let cancelled = fake.cancelled.clone();
    fake.mutate = Some(Box::new(move |index| {
        if index == 1 {
            cancelled.store(143, std::sync::atomic::Ordering::SeqCst);
        }
    }));
    assert_eq!(run(&fixture, &mut fake).await.unwrap_err().code, 42);
    assert_eq!(fake.calls, 2);
    assert!(fake.replay.is_empty());
}

#[tokio::test]
async fn observed_overall_deadline_prevents_next_dispatch_and_replay() {
    let fixture = Fixture::new(0);
    let mut fake = Fake::new(&fixture, Arc::new(Clock::new()));
    fake.advance_on_call = Some((0, Duration::from_secs(601)));
    assert_eq!(run(&fixture, &mut fake).await.unwrap_err().code, 124);
    assert_eq!(fake.calls, 1);
    assert!(fake.replay.is_empty());
}

#[test]
fn complete_schedule_accounts_unused_aggregate_hash_once_and_admits_exact_tail() {
    let clock = Arc::new(Clock::new());
    let context = plan::Context::new(clock.clone());
    assert_eq!(context.tail().unwrap(), Duration::from_secs(530));
    context.hash_used.set(Duration::from_secs(20));
    assert_eq!(context.tail().unwrap(), Duration::from_secs(510));
    clock.advance(Duration::from_secs(90));
    context.admit().unwrap();
    context.command(0).unwrap();
    clock.advance(Duration::from_nanos(1));
    assert_eq!(context.command(0).unwrap_err().code, 124);
}

#[tokio::test]
async fn setup_deadline_refusal_settles_both_logs_without_dispatch() {
    let fixture = Fixture::new(0);
    let mut fake = Fake::new(&fixture, Arc::new(Clock::new()));
    fake.late_setup = Some(fixture.output.join("gh-0.stdout"));
    assert_eq!(run(&fixture, &mut fake).await.unwrap_err().code, 124);
    assert_eq!(fake.calls, 0);
    assert!(fake.replay.is_empty());
    assert_eq!(
        fs::metadata(fixture.output.join("gh-0.stdout"))
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        fs::metadata(fixture.output.join("gh-0.stderr"))
            .unwrap()
            .len(),
        0
    );
}

#[tokio::test]
async fn saved_observation_precedes_final_replay_signal_deadline_and_cleanup_fences() {
    for (cancel, advance, unproven, expected) in [
        (143, 0, false, 143),
        (130, 0, false, 130),
        (0, 601, false, 124),
        (0, 0, true, 1),
    ] {
        let fixture = Fixture::new(0);
        let mut fake = Fake::new(&fixture, Arc::new(Clock::new()));
        fake.replay_cancel = cancel;
        fake.replay_advance = Duration::from_secs(advance);
        fake.replay_unproven = unproven;
        assert_eq!(run(&fixture, &mut fake).await.unwrap_err().code, expected);
        assert_eq!(fake.calls, 4);
        assert_eq!(
            fs::read(fixture.output.join("verification-observation.json")).unwrap(),
            fake.replay
        );
    }
}
