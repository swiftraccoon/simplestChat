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
        "all admitted owners and helper reservations retire"
    );
    result
}

#[test]
fn synthetic_verifier_fixture_preserves_the_complete_required_output_protocol() {
    use simplestchat_ops::distribution::manifest::{Selection, verification::RetainedMetadata};
    for count in [0, 16] {
        let fixture = Fixture::new(count);
        let selection = Selection::decode(&bytes(&fixture.wire["selection"])).unwrap();
        let metadata = RetainedMetadata::decode(
            &selection,
            &fs::read(fixture.input.join("ops-release.json")).unwrap(),
            &fs::read(fixture.input.join("ops-evidence.json")).unwrap(),
            &fs::read(fixture.input.join("ops-predicate.json")).unwrap(),
            &fs::read(fixture.input.join("ops-preparation.json")).unwrap(),
        )
        .unwrap();
        let interpret = |output: &[u8]| {
            metadata.interpret_successful_verifier_output(
                &fixture.outputs[6],
                &fixture.outputs[7],
                output,
            )
        };
        interpret(&fixture.outputs[8]).unwrap();
        let mut missing: Value = serde_json::from_slice(&fixture.outputs[8]).unwrap();
        let _ = missing[0].as_object_mut().unwrap().remove("attestation");
        assert_eq!(
            interpret(&bytes(&missing)).unwrap_err(),
            simplestchat_ops::distribution::manifest::verification::Failure::Verifier
        );
    }
}

#[tokio::test]
async fn complete_acquisition_and_embedded_verifier_retains_exact_zero_and_maximum_graphs() {
    for count in [0, 16] {
        let fixture = Fixture::new(count);
        let mut fake = Fake::new(&fixture);
        run(&fixture, &mut fake).await.unwrap();
        assert_eq!(fake.calls, 9);
        assert_eq!(
            fake.replays.len(),
            1,
            "embedded verifier cannot publish an early result"
        );
        let observation: Value = serde_json::from_slice(&fake.replays[0]).unwrap();
        assert_eq!(observation["retainedFiles"], count + 6);
        assert_eq!(observation["artifact"]["artifactId"], 456);
        assert_eq!(observation["kind"], "ops-artifact-acquisition-observation");
        for entry in fs::read_dir(&fixture.input).unwrap() {
            let entry = entry.unwrap();
            let expected = fs::read(entry.path()).unwrap();
            assert_eq!(
                fs::read(fixture.output.join("input").join(entry.file_name())).unwrap(),
                expected
            );
            assert_eq!(
                fs::read(fixture.output.join("verified").join(entry.file_name())).unwrap(),
                expected
            );
        }
        assert_eq!(
            fs::read(fixture.output.join("artifact.zip")).unwrap(),
            fixture.outputs[4]
        );
        let public = String::from_utf8(fake.replays[0].clone()).unwrap();
        for private in ["PRIVATE", "TOKEN", fixture.root.to_str().unwrap()] {
            assert!(!public.contains(private));
        }
    }
}

#[test]
fn original_request_rejects_ambiguous_fields_tokens_urls_numbers_and_control_paths() {
    let fixture = Fixture::new(0);
    let original = bytes(&fixture.wire);
    assert!(request::Request::decode(&original, &super::super::sha(&original)).is_ok());
    assert!(request::Request::decode(&original, &"0".repeat(64)).is_err());
    for (key, value) in [
        ("artifactId", json!(0)),
        ("artifactId", json!(9_007_199_254_740_992u64)),
        ("artifactId", json!(true)),
        ("artifactId", json!(1.0)),
        ("url", json!("https://private.invalid")),
        ("token", json!("PRIVATE-TOKEN")),
        ("authentication", json!("anonymous")),
        ("output", json!("/tmp/unsafe\u{0085}path")),
        ("output", json!("relative")),
        ("output", json!("/")),
    ] {
        let mut wire = fixture.wire.clone();
        wire[key] = value;
        let raw = bytes(&wire);
        assert!(
            request::Request::decode(&raw, &super::super::sha(&raw)).is_err(),
            "{key}"
        );
    }
    for raw in [
        b"{\"artifactId\":1,\"artifactId\":1}".as_slice(),
        &[b'x'; 16 * 1024 + 1],
    ] {
        assert!(request::Request::decode(raw, &super::super::sha(raw)).is_err());
    }
}

#[test]
fn output_reserves_one_component_for_the_unchanged_retained_verifier_paths() {
    let fixture = Fixture::new(0);
    for count in [15, 16] {
        let output = std::path::PathBuf::from(format!("/{}", vec!["a"; count].join("/")));
        let mut wire = fixture.wire.clone();
        wire["output"] = json!(output);
        let raw = bytes(&wire);
        let decoded = request::Request::decode(&raw, &super::super::sha(&raw));
        assert_eq!(decoded.is_ok(), count == 15);
        assert!(request::path(&output));
        for name in ["input", "verified", "verify-request.json"] {
            assert_eq!(
                super::super::verify::request_path(&output.join(name)),
                count == 15
            );
        }
    }
}

#[tokio::test]
async fn missing_explicit_token_refuses_before_output_or_tool_dispatch() {
    let mut fixture = Fixture::new(0);
    fixture.environment.remove(std::ffi::OsStr::new("GH_TOKEN"));
    let mut fake = Fake::new(&fixture);
    assert_eq!(run(&fixture, &mut fake).await.unwrap_err().code, 1);
    assert_eq!(fake.calls, 0);
    assert!(!fixture.output.exists());
}

#[tokio::test]
async fn fresh_api_mismatch_refuses_zip_dispatch_even_with_zero_cli_status() {
    for (index, pointer, value) in [
        (1, "/owner/id", json!(1)),
        (1, "/fork", json!(true)),
        (2, "/run_attempt", json!(1)),
        (2, "/head_sha", json!("b".repeat(40))),
        (2, "/event", json!("workflow_dispatch")),
        (2, "/status", json!("in_progress")),
        (2, "/conclusion", json!("failure")),
        (2, "/head_repository/id", json!(1)),
        (3, "/id", json!(457)),
        (3, "/expired", json!(true)),
        (3, "/name", json!("simplestchat-ops-wrong")),
        (3, "/url", json!("https://foreign.invalid/artifact")),
        (
            3,
            "/archive_download_url",
            json!("https://foreign.invalid/private"),
        ),
        (3, "/workflow_run/id", json!(124)),
        (3, "/workflow_run/head_repository_id", json!(1)),
        (3, "/size_in_bytes", json!(super::ZIP + 1)),
        (3, "/size_in_bytes", json!(0)),
        (3, "/digest", json!("")),
    ] {
        let fixture = Fixture::new(0);
        let mut fake = Fake::new(&fixture);
        let mut observed: Value = serde_json::from_slice(&fake.outputs[index]).unwrap();
        *observed.pointer_mut(pointer).unwrap() = value;
        fake.outputs[index] = bytes(&observed);
        assert_eq!(
            run(&fixture, &mut fake).await.unwrap_err().code,
            1,
            "{pointer}"
        );
        assert_eq!(fake.calls, if index < 3 { 3 } else { 4 }, "{pointer}");
        assert!(!fixture.output.join("artifact.zip").exists());
        assert!(fake.replays.is_empty());
    }
}

#[tokio::test]
async fn api_digest_size_and_complete_zip_structure_are_independent_admission_gates() {
    for change in 0..4 {
        let fixture = Fixture::new(0);
        let mut fake = Fake::new(&fixture);
        let mut artifact: Value = serde_json::from_slice(&fake.outputs[3]).unwrap();
        match change {
            0 => artifact["digest"] = json!(format!("sha256:{}", "0".repeat(64))),
            1 => artifact["size_in_bytes"] = json!(fake.outputs[4].len() + 1),
            2 => {
                fake.outputs[4][0] ^= 1;
                artifact["digest"] =
                    json!(format!("sha256:{}", super::super::sha(&fake.outputs[4])));
            }
            _ => {
                fake.outputs[4].extend(b"HIDDEN-TRAILER");
                artifact["digest"] =
                    json!(format!("sha256:{}", super::super::sha(&fake.outputs[4])));
                artifact["size_in_bytes"] = json!(fake.outputs[4].len());
            }
        }
        fake.outputs[3] = bytes(&artifact);
        assert_eq!(run(&fixture, &mut fake).await.unwrap_err().code, 1);
        assert_eq!(fake.calls, 5);
        assert!(!fixture.output.join("verified").exists());
        assert!(fake.replays.is_empty());
    }
}

#[tokio::test]
async fn retained_verifier_still_owns_certificate_subject_predicate_and_pair_policy() {
    for (pointer, value) in [
        (
            "/0/verificationResult/signature/certificate/sourceRepositoryIdentifier",
            json!(1),
        ),
        (
            "/0/verificationResult/signature/certificate/runnerEnvironment",
            json!("self-hosted"),
        ),
        (
            "/0/verificationResult/statement/subject/2/digest/sha256",
            json!("0".repeat(64)),
        ),
        (
            "/0/verificationResult/statement/predicate/schemaVersion",
            json!(2),
        ),
    ] {
        let fixture = Fixture::new(0);
        let mut fake = Fake::new(&fixture);
        let mut verification: Value = serde_json::from_slice(&fake.outputs[8]).unwrap();
        *verification.pointer_mut(pointer).unwrap() = value;
        fake.outputs[8] = bytes(&verification);
        assert_eq!(run(&fixture, &mut fake).await.unwrap_err().code, 1);
        assert_eq!(fake.calls, 9);
        assert!(!fixture.output.join("acquisition-observation.json").exists());
        assert!(fake.replays.is_empty());
    }
}

#[tokio::test]
async fn same_length_postfinish_zip_edits_and_original_request_replacement_refuse() {
    for request in [false, true] {
        let fixture = Fixture::new(0);
        let mut fake = Fake::new(&fixture);
        let path = if request {
            fixture.request.clone()
        } else {
            fixture.output.join("artifact.zip")
        };
        fake.mutate = Some(Box::new(move |index| {
            if index == 4 {
                let mut data = fs::read(&path).unwrap();
                if request {
                    fs::rename(&path, path.with_extension("original")).unwrap();
                } else {
                    data[0] ^= 1;
                }
                fs::write(&path, data).unwrap();
            }
        }));
        assert_eq!(run(&fixture, &mut fake).await.unwrap_err().code, 1);
        assert_eq!(fake.calls, 5);
        assert!(fake.replays.is_empty());
    }
}

#[tokio::test]
async fn every_known_cli_failure_wins_over_later_signal_and_no_future_dispatch_occurs() {
    for index in 0..9 {
        for code in [42, 124, 130, 143] {
            let fixture = Fixture::new(0);
            let mut fake = Fake::new(&fixture);
            fake.fail = Some((index, code));
            let cancelled = fake.cancelled.clone();
            fake.mutate = Some(Box::new(move |at| {
                if at == index {
                    cancelled.store(143, std::sync::atomic::Ordering::SeqCst);
                }
            }));
            assert_eq!(
                run(&fixture, &mut fake).await.unwrap_err().code,
                code,
                "{index}"
            );
            assert_eq!(fake.calls, index + 1);
            assert!(fake.replays.is_empty());
        }
    }
}

#[test]
fn schedule_reserves_complete_verifier_once_and_refuses_beyond_exact_remaining_tail() {
    let clock = Arc::new(Clock::new());
    let context = plan::Context::new(clock.clone());
    assert_eq!(context.tail().unwrap(), Duration::from_secs(1325));
    clock.advance(Duration::from_secs(115));
    context.command(0).unwrap();
    clock.advance(Duration::from_nanos(1));
    assert_eq!(context.command(0).unwrap_err().code, 124);
}

#[tokio::test]
async fn late_setup_and_command_completion_cannot_dispatch_the_next_operation() {
    for setup in [false, true] {
        let fixture = Fixture::new(0);
        let mut fake = Fake::new(&fixture);
        if setup {
            fake.setup_late = Some(fixture.output.join("gh-0.stdout"));
        } else {
            fake.advance = Some((0, Duration::from_secs(1441)));
        }
        assert_eq!(run(&fixture, &mut fake).await.unwrap_err().code, 124);
        assert_eq!(fake.calls, usize::from(!setup));
        assert!(fake.replays.is_empty());
    }
}

#[tokio::test]
async fn saved_observation_does_not_hide_later_signal_or_whole_deadline() {
    for (signal, seconds, code) in [(130, 0, 130), (143, 0, 143), (0, 1441, 124)] {
        let fixture = Fixture::new(0);
        let mut fake = Fake::new(&fixture);
        fake.replay_cancel = signal;
        fake.replay_advance = Duration::from_secs(seconds);
        assert_eq!(run(&fixture, &mut fake).await.unwrap_err().code, code);
        assert_eq!(fake.replays.len(), 1);
        assert_eq!(
            fs::read(fixture.output.join("acquisition-observation.json")).unwrap(),
            fake.replays[0]
        );
    }
}

#[tokio::test]
async fn final_replay_rechecks_original_verified_file_and_outer_namespace_witnesses() {
    use std::os::unix::fs::PermissionsExt;
    for root in [false, true] {
        for earlier in [None, Some(42)] {
            let fixture = Fixture::new(0);
            let mut fake = Fake::new(&fixture);
            let output = fixture.output.clone();
            let displaced = fixture.root.join("original-after-replay");
            fake.replay_mutate = Some(Box::new(move || {
                if root {
                    fs::rename(&output, &displaced).unwrap();
                    fs::create_dir(&output).unwrap();
                    fs::set_permissions(&output, fs::Permissions::from_mode(0o700)).unwrap();
                } else {
                    let path = output.join("verified/simplestchat-ops");
                    let original = fs::read(&path).unwrap();
                    fs::rename(&path, &displaced).unwrap();
                    fs::write(&path, original).unwrap();
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
                }
            }));
            fake.replay_fail = earlier;
            assert_eq!(
                run(&fixture, &mut fake).await.unwrap_err().code,
                earlier.unwrap_or(1),
                "root={root}"
            );
            assert_eq!(fake.calls, 9);
            assert_eq!(fake.replays.len(), 1);
            let observation = if root {
                fixture
                    .root
                    .join("original-after-replay/acquisition-observation.json")
            } else {
                fixture.output.join("acquisition-observation.json")
            };
            assert_eq!(fs::read(observation).unwrap(), fake.replays[0]);
        }
    }
}
