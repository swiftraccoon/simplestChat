//! Source-bound inert verifier fixtures and actual private export files. These
//! tests prove no cryptography, network, SSH transfer or candidate execution.

use super::super::{
    acquire::fixture::{Fake, Fixture, bytes},
    namespace::Budget,
};
use super::{execute, files, plan, request, worker};
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

fn wire(fixture: &Fixture) -> Value {
    let mut wire = fixture.wire.clone();
    wire["output"] = json!(fixture.root.join("exported"));
    wire["remoteInput"] = json!(format!(
        "/usr/local/libexec/simplestchat-ops-bootstrap/{}/input",
        "a".repeat(32)
    ));
    wire
}
fn save(fixture: &Fixture, wire: &Value) -> (PathBuf, String) {
    let path = fixture.root.join("export-request.json");
    let raw = bytes(wire);
    fs::write(&path, &raw).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    (path, super::super::sha(&raw))
}
async fn run(fixture: &Fixture, wire: &Value, fake: &mut Fake) -> crate::tooling::Result<()> {
    let (path, selected) = save(fixture, wire);
    let budget = Budget::default();
    let plan = plan::Plan::new(fake.clock.clone());
    let result = execute::transaction(
        execute::Inputs {
            path,
            selected,
            caller: fixture.root.clone(),
            environment: fixture.environment.clone(),
        },
        budget.clone(),
        &plan,
        fake,
    )
    .await;
    assert_eq!(budget.count(), 0);
    assert_eq!(budget.close_counts().1, 0);
    result
}
#[tokio::test]
async fn complete_authenticated_export_preserves_original_three_members_and_closed_installer_pins()
{
    for role in ["first", "upgrade", "repeat"] {
        let fixture = Fixture::new(1);
        let mut wire = wire(&fixture);
        let mut fake = Fake::new(&fixture);
        let binary = fs::read(fixture.root.join("binary")).unwrap();
        let binary_sha = super::super::sha(&binary);
        if role != "first" {
            wire["installedBinary"] = json!({"sha256":if role=="repeat"{
            binary_sha.clone()}else{"e".repeat(64)},"bytes":binary.len()});
        }
        run(&fixture, &wire, &mut fake).await.unwrap();
        assert_eq!(fake.calls, 9);
        assert_eq!(fake.replays.len(), 1);
        let row: Value = serde_json::from_slice(&fake.replays[0]).unwrap();
        assert_eq!(
            row["operation"],
            if role == "upgrade" {
                "upgrade"
            } else {
                "install"
            }
        );
        assert_eq!(row["artifactId"], 456);
        assert_eq!(row["binary"]["sha256"], binary_sha);
        assert!(row.get("authenticated").is_none());
        let tar = fs::read(row["transfer"]["path"].as_str().unwrap()).unwrap();
        assert_eq!(super::super::sha(&tar), row["transfer"]["sha256"]);
        assert_eq!(tar.len() as u64, row["transfer"]["bytes"].as_u64().unwrap());
        let mut names = Vec::new();
        let mut member_pins = std::collections::BTreeMap::new();
        for entry in tar::Archive::new(tar.as_slice()).entries().unwrap() {
            let mut entry = entry.unwrap();
            let name = entry.path().unwrap().to_str().unwrap().to_owned();
            assert_eq!(entry.header().mode().unwrap(), 0o600);
            assert_eq!(entry.header().uid().unwrap(), 0);
            let mut data = Vec::new();
            entry.read_to_end(&mut data).unwrap();
            if name == "ops-selection.json" {
                let selected: Value = serde_json::from_slice(&data).unwrap();
                assert_eq!(selected, wire["selection"]);
            } else {
                assert_eq!(data, fs::read(fixture.input.join(&name)).unwrap());
            }
            member_pins.insert(name.clone(), (super::super::sha(&data), data.len() as u64));
            names.push(name);
        }
        assert_eq!(
            names,
            ["ops-release.json", "ops-selection.json", "simplestchat-ops"]
        );
        let raw = fs::read(row["installerRequest"]["path"].as_str().unwrap()).unwrap();
        assert_eq!(super::super::sha(&raw), row["installerRequest"]["sha256"]);
        let install: Value = serde_json::from_slice(&raw).unwrap();
        for (role, name) in [
            ("manifest", "ops-release.json"),
            ("selection", "ops-selection.json"),
            ("binary", "simplestchat-ops"),
        ] {
            assert_eq!(install[role]["sha256"], member_pins[name].0);
            assert_eq!(install[role]["bytes"], member_pins[name].1);
        }
        assert_eq!(
            install["schemaVersion"],
            if role == "upgrade" { 2 } else { 1 }
        );
        assert_eq!(install["binary"]["sha256"], binary_sha);
        assert_eq!(
            install["binary"]["path"],
            format!("{}/simplestchat-ops", wire["remoteInput"].as_str().unwrap())
        );
        assert_eq!(install.get("installedBinary").is_some(), role == "upgrade");
        for name in [
            "ops-pair.tar",
            "install-request.json",
            "export.json",
            "acquire-request.json",
        ] {
            assert_eq!(
                fs::metadata(fixture.root.join("exported").join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o7777,
                0o600
            );
        }
        assert_eq!(
            fs::read(fixture.root.join("exported/export.json")).unwrap(),
            fake.replays[0]
        );
        let public = String::from_utf8(fake.replays[0].clone()).unwrap();
        for secret in ["PRIVATE-TOKEN", "PRIVATE-DEBUG", "PRIVATE-PROXY"] {
            assert!(!public.contains(secret));
        }
    }
}
#[test]
fn request_closes_optional_custody_remote_namespace_and_all_numeric_and_path_neighbors() {
    let fixture = Fixture::new(0);
    let original = wire(&fixture);
    let raw = bytes(&original);
    assert!(request::Request::decode(&raw, &super::super::sha(&raw)).is_ok());
    for (key, value) in [
        ("schemaVersion", json!(1.0)),
        ("artifactId", json!(true)),
        ("artifactId", json!(0)),
        ("artifactId", json!(9_007_199_254_740_992u64)),
        ("authentication", json!("anonymous")),
        ("authenticated", json!(true)),
        ("url", json!("https://private.invalid")),
        ("token", json!("PRIVATE-TOKEN")),
        ("output", json!("relative")),
        ("output", json!("/private/unsafe\u{0085}path")),
        ("installedBinary", Value::Null),
        (
            "installedBinary",
            json!({"sha256":"a".repeat(64),"bytes":0}),
        ),
        (
            "installedBinary",
            json!({"sha256":"a".repeat(64),"bytes":true}),
        ),
        (
            "installedBinary",
            json!({"sha256":"A".repeat(64),"bytes":4096}),
        ),
        (
            "installedBinary",
            json!({"sha256":"a".repeat(64),"bytes":4096,"path":"/foreign"}),
        ),
    ] {
        let mut changed = original.clone();
        changed[key] = value;
        let raw = bytes(&changed);
        assert!(
            request::Request::decode(&raw, &super::super::sha(&raw)).is_err(),
            "{key}"
        );
    }
    for remote in [
        "/run/other/input".to_owned(),
        format!("/run/simplestchat-ops-bootstrap/{}/input", "a".repeat(32)),
        format!(
            "/usr/local/libexec/simplestchat-public/bootstrap/{}/input",
            "a".repeat(32)
        ),
        format!(
            "/usr/local/libexec/simplestchat-ops-bootstrap-other/{}/input",
            "a".repeat(32)
        ),
        format!(
            "/usr/local/libexec/simplestchat-ops-bootstrap/{}/input",
            "A".repeat(32)
        ),
        format!(
            "/usr/local/libexec/simplestchat-ops-bootstrap/{}/input",
            "a".repeat(31)
        ),
        format!(
            "/usr/local/libexec/simplestchat-ops-bootstrap/{}/input/",
            "a".repeat(32)
        ),
        format!(
            "/usr/local/libexec/simplestchat-ops-bootstrap/{}/../input",
            "a".repeat(32)
        ),
    ] {
        let mut changed = original.clone();
        changed["remoteInput"] = json!(remote);
        let raw = bytes(&changed);
        assert!(request::Request::decode(&raw, &super::super::sha(&raw)).is_err());
    }
    let duplicate = String::from_utf8(raw.clone()).unwrap().replacen(
        "\"artifactId\":456",
        "\"artifactId\":456,\"artifactId\":456",
        1,
    );
    assert!(
        request::Request::decode(
            duplicate.as_bytes(),
            &super::super::sha(duplicate.as_bytes())
        )
        .is_err()
    );
    assert!(request::Request::decode(&raw, &"0".repeat(64)).is_err());
}

#[test]
fn retained_zip_is_omitted_by_default_and_only_the_explicit_original_path_propagates() {
    let fixture = Fixture::new(0);
    let mut original = wire(&fixture);
    let raw = bytes(&original);
    let decoded = request::Request::decode(&raw, &super::super::sha(&raw)).unwrap();
    let acquired: Value = serde_json::from_slice(&decoded.acquire().unwrap()).unwrap();
    assert!(acquired.get("artifactZip").is_none());

    let source = fixture.root.join("PRIVATE-retained-ops.zip");
    original["artifactZip"] = json!(source);
    let raw = bytes(&original);
    let decoded = request::Request::decode(&raw, &super::super::sha(&raw)).unwrap();
    let acquired: Value = serde_json::from_slice(&decoded.acquire().unwrap()).unwrap();
    assert_eq!(acquired["artifactZip"], original["artifactZip"]);
    assert_eq!(acquired["selection"], original["selection"]);
    assert_eq!(acquired["artifactId"], original["artifactId"]);
    assert_eq!(acquired["authentication"], "gh-token");
    assert_eq!(format!("{decoded:?}"), "PinnedOpsDistributionExportRequest");

    for value in [
        Value::Null,
        json!(true),
        json!(123),
        json!({"path":source,"authenticated":true}),
        json!("relative.zip"),
        json!("/"),
        json!("/private/../ops.zip"),
        json!("/private//ops.zip"),
        json!("/private/unsafe\u{0085}ops.zip"),
        original["output"].clone(),
        json!(fixture.root.join("exported/inside.zip")),
        json!(fixture.root),
    ] {
        let mut changed = original.clone();
        changed["artifactZip"] = value;
        let raw = bytes(&changed);
        assert!(request::Request::decode(&raw, &super::super::sha(&raw)).is_err());
    }
    let raw = String::from_utf8(bytes(&original)).unwrap();
    let duplicate = raw.replacen(
        "\"artifactZip\":",
        "\"artifactZip\":null,\"artifactZip\":",
        1,
    );
    assert!(
        request::Request::decode(
            duplicate.as_bytes(),
            &super::super::sha(duplicate.as_bytes())
        )
        .is_err()
    );
}

fn retain_zip(fixture: &Fixture, wire: &mut Value, fake: &mut Fake) -> PathBuf {
    let source = fixture.root.join("PRIVATE-retained-ops.zip");
    fs::write(&source, &fixture.outputs[4]).unwrap();
    fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();
    wire["artifactZip"] = json!(source);
    // Only the download body is replaced by actual local data. Every fresh
    // metadata and opaque-verifier invocation still owns its real capture.
    fake.outputs.remove(4);
    source
}

#[tokio::test]
async fn retained_zip_still_requires_every_fresh_metadata_and_opaque_verifier_result() {
    for failed in std::iter::once(None).chain((0..8).map(Some)) {
        let fixture = Fixture::new(1);
        let mut wire = wire(&fixture);
        let mut fake = Fake::new(&fixture);
        let source = retain_zip(&fixture, &mut wire, &mut fake);
        fake.fail = failed.map(|index| (index, 23));
        let result = run(&fixture, &wire, &mut fake).await;
        if let Some(index) = failed {
            assert!(result.is_err(), "fresh result {index}");
            assert_eq!(fake.calls, index + 1);
            assert!(fake.replays.is_empty());
            assert!(!fixture.root.join("exported/install-request.json").exists());
        } else {
            result.unwrap();
            assert_eq!(fake.calls, 8);
            assert_eq!(fake.replays.len(), 1);
            let report: Value = serde_json::from_slice(&fake.replays[0]).unwrap();
            assert_eq!(report["selection"], wire["selection"]);
            assert_eq!(report["artifactId"], wire["artifactId"]);
            assert!(report.get("artifactZip").is_none());
            let text = String::from_utf8(fake.replays[0].clone()).unwrap();
            assert!(!text.contains(source.to_str().unwrap()));
            assert!(!text.contains("PRIVATE-retained-ops"));
            let acquisition: Value = serde_json::from_slice(
                &fs::read(fixture.root.join("exported/acquire-request.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(acquisition["artifactZip"], wire["artifactZip"]);
            assert_eq!(acquisition["authentication"], "gh-token");
            assert_eq!(fs::read(&source).unwrap(), fixture.outputs[4]);
        }
    }
}

#[tokio::test]
async fn changed_or_replaced_retained_zip_during_fresh_metadata_cannot_publish_delivery() {
    for replace in [false, true] {
        let fixture = Fixture::new(0);
        let mut wire = wire(&fixture);
        let mut fake = Fake::new(&fixture);
        let source = retain_zip(&fixture, &mut wire, &mut fake);
        fake.mutate = Some(Box::new(move |index| {
            if index == 3 {
                let mut raw = fs::read(&source).unwrap();
                if replace {
                    fs::rename(&source, source.with_extension("original")).unwrap();
                } else {
                    raw[0] ^= 1;
                }
                fs::write(&source, raw).unwrap();
                fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();
            }
        }));
        assert!(run(&fixture, &wire, &mut fake).await.is_err());
        assert_eq!(fake.calls, 4);
        assert!(fake.replays.is_empty());
        assert!(!fixture.root.join("exported/install-request.json").exists());
    }
}

#[tokio::test]
async fn every_acquisition_child_refusal_withholds_export_and_attempts_all_owner_closes() {
    for index in 0..9 {
        let fixture = Fixture::new(0);
        let wire = wire(&fixture);
        let mut fake = Fake::new(&fixture);
        fake.fail = Some((index, 23));
        assert!(run(&fixture, &wire, &mut fake).await.is_err());
        assert_eq!(fake.calls, index + 1);
        assert!(fake.replays.is_empty());
        assert!(!fixture.root.join("exported/install-request.json").exists());
    }
}
#[tokio::test]
async fn changed_outer_original_during_authentication_cannot_become_delivery_authority() {
    for replace in [false, true] {
        let fixture = Fixture::new(0);
        let wire = wire(&fixture);
        let mut fake = Fake::new(&fixture);
        let path = fixture.root.join("export-request.json");
        fake.mutate = Some(Box::new(move |index| {
            if index == 8 {
                let raw = fs::read(&path).unwrap();
                if replace {
                    fs::remove_file(&path).unwrap();
                }
                fs::write(&path, if replace { raw } else { b"changed".to_vec() }).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            }
        }));
        assert!(run(&fixture, &wire, &mut fake).await.is_err());
        assert!(fake.replays.is_empty());
        assert!(!fixture.root.join("exported/install-request.json").exists());
    }
}
#[tokio::test]
async fn final_replay_and_original_changes_remain_negative_after_receipt_publication() {
    for target in [
        "request",
        "transfer",
        "installer",
        "parent",
        "cancel",
        "replay",
        "deadline",
    ] {
        let fixture = Fixture::new(0);
        let wire = wire(&fixture);
        let mut fake = Fake::new(&fixture);
        let path = fixture.root.join(match target {
            "request" => "export-request.json",
            "transfer" => "exported/ops-pair.tar",
            "installer" => "exported/install-request.json",
            _ => "exported",
        });
        if target == "cancel" {
            fake.replay_cancel = 143;
        } else if target == "replay" {
            fake.replay_fail = Some(1);
        } else if target == "deadline" {
            fake.replay_advance = plan::ENTRY;
        } else {
            fake.replay_mutate = Some(Box::new(move || {
                if target == "parent" {
                    fs::rename(&path, path.with_extension("saved")).unwrap();
                    fs::create_dir(&path).unwrap();
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
                } else {
                    let bytes = fs::read(&path).unwrap();
                    fs::rename(&path, path.with_extension("saved")).unwrap();
                    fs::write(&path, bytes).unwrap();
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
                }
            }));
        }
        let result = run(&fixture, &wire, &mut fake).await;
        assert!(result.is_err(), "{target}");
        if target == "deadline" {
            assert_eq!(result.unwrap_err().code, 124);
        }
        assert_eq!(fake.replays.len(), 1, "{target}");
    }
}
#[tokio::test]
async fn all_budgeted_file_owners_close_before_the_single_stdout_replay() {
    let fixture = Fixture::new(0);
    let wire = wire(&fixture);
    let (path, selected) = save(&fixture, &wire);
    let mut fake = Fake::new(&fixture);
    let budget = Budget::default();
    let at_replay = budget.clone();
    fake.replay_mutate = Some(Box::new(move || {
        assert_eq!(
            at_replay.count(),
            0,
            "stdout cannot precede retained file closes"
        );
        assert_eq!(at_replay.close_counts().1, 0);
    }));
    let plan = plan::Plan::new(fake.clock.clone());
    let result = execute::transaction(
        execute::Inputs {
            path,
            selected,
            caller: fixture.root.clone(),
            environment: fixture.environment.clone(),
        },
        budget.clone(),
        &plan,
        &mut fake,
    )
    .await;
    assert!(result.is_ok());
    assert_eq!(fake.replays.len(), 1);
    assert_eq!(budget.count(), 0);
    assert_eq!(budget.close_counts().1, 0);
}
#[tokio::test]
async fn custody_pin_with_candidate_digest_and_foreign_length_never_publishes_installer_request() {
    let fixture = Fixture::new(0);
    let mut wire = wire(&fixture);
    let binary = fs::read(fixture.root.join("binary")).unwrap();
    wire["installedBinary"] = json!({"sha256":super::super::sha(&binary),"bytes":binary.len()+1});
    let mut fake = Fake::new(&fixture);
    assert!(run(&fixture, &wire, &mut fake).await.is_err());
    assert!(fake.replays.is_empty());
    assert!(!fixture.root.join("exported/install-request.json").exists());
}
#[test]
fn whole_export_tail_keeps_full_acquisition_and_cannot_restart_the_original_clock() {
    let clock = Arc::new(super::super::acquire::fixture::Clock::new());
    let plan = plan::Plan::new(clock.clone());
    let tail: Duration = plan::PHASES.iter().copied().sum();
    assert_eq!(tail, Duration::from_secs(1815));
    assert_eq!(plan::PHASES[1], Duration::from_secs(1440));
    let end = plan.begin(0).unwrap();
    clock.advance(Duration::from_secs(30));
    assert!(plan.complete(0, end).is_err());
    assert!(plan.begin(1).is_err());
    let clock = Arc::new(super::super::acquire::fixture::Clock::new());
    let plan = plan::Plan::new(clock.clone());
    let end = plan.begin(0).unwrap();
    plan.complete(0, end).unwrap();
    clock.advance(Duration::from_secs(76));
    assert!(plan.begin(1).is_err());
    let clock = Arc::new(super::super::acquire::fixture::Clock::new());
    let original = super::super::acquire::AcquisitionClock::now(&*clock);
    clock.advance(Duration::from_secs(46));
    let plan = plan::Plan::from_start(clock, original);
    assert_eq!(plan.origin(), original);
    assert!(
        plan.begin(0).is_err(),
        "runtime setup cannot restart the entry clock"
    );
}
#[test]
fn final_output_close_failure_is_negative_after_real_directory_owners_have_closed() {
    let fixture = Fixture::new(0);
    let budget = Budget::default();
    let clock = Arc::new(super::super::acquire::fixture::Clock::new());
    let start = super::super::acquire::AcquisitionClock::now(&*clock);
    let checkpoint = worker::Checkpoint {
        clock,
        start,
        end: start + Duration::from_secs(30),
        abort: Arc::new(AtomicBool::new(false)),
    };
    let result =
        files::close_failure_probe(&fixture.root.join("close-probe"), &budget, &checkpoint);
    assert!(result.is_err());
    assert_eq!(budget.count(), 0);
    assert_eq!(budget.close_counts().1, 0);
}
