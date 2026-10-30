use super::*;
use serde_json::{Value, json};

fn revision() -> Revision {
    admit_revision(
        &"a".repeat(40),
        "main",
        "",
        "git@github.com:swiftraccoon/simplestChat.git",
    )
    .unwrap()
}
fn repository() -> Value {
    json!({"id":REPOSITORY_ID,"full_name":REPOSITORY,"fork":false,
        "owner":{"id":OWNER_ID,"login":"swiftraccoon"}})
}
fn record() -> Value {
    json!({"id":123,"run_attempt":2,"head_sha":"a".repeat(40),"head_branch":"main",
        "name":"CI","path":".github/workflows/ci.yml","event":"push",
        "status":"completed","conclusion":"success","repository":repository(),"head_repository":repository()})
}
fn encode(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}
fn listing(value: Value) -> Vec<u8> {
    encode(&json!({"total_count":1,"workflow_runs":[value]}))
}
fn selected() -> Run {
    runs(&listing(record()), &revision()).unwrap()
}
fn artifact_record(kind: ArtifactKind) -> Value {
    json!({"id":456,"name":kind.name(&revision()),"expired":false,
        "url":format!("https://api.github.com/repos/{REPOSITORY}/actions/artifacts/456"),
        "archive_download_url":format!("https://api.github.com/repos/{REPOSITORY}/actions/artifacts/456/zip"),
        "size_in_bytes":12345,"digest":format!("sha256:{}","b".repeat(64)),
        "workflow_run":{"id":123,"head_sha":"a".repeat(40),"repository_id":REPOSITORY_ID,"head_repository_id":REPOSITORY_ID}})
}
fn artifact_listing(value: Value) -> Vec<u8> {
    encode(&json!({"total_count":1,"artifacts":[value]}))
}

#[test]
fn clean_main_and_exact_repository_are_required_before_selection() {
    for remote in [
        "https://github.com/swiftraccoon/simplestChat",
        "https://github.com/swiftraccoon/simplestChat.git",
        "git@github.com:swiftraccoon/simplestChat.git",
        "ssh://git@github.com/swiftraccoon/simplestChat.git",
    ] {
        assert_eq!(
            admit_revision(&format!("{}\n", "a".repeat(40)), "main\n", "", remote)
                .unwrap()
                .as_str(),
            "a".repeat(40)
        );
    }
    for (head, branch, status, remote) in [
        (
            "a".repeat(39),
            "main",
            "",
            "https://github.com/swiftraccoon/simplestChat",
        ),
        (
            "A".repeat(40),
            "main",
            "",
            "https://github.com/swiftraccoon/simplestChat",
        ),
        (
            "a".repeat(40),
            "feature",
            "",
            "https://github.com/swiftraccoon/simplestChat",
        ),
        (
            "a".repeat(40),
            "main",
            " M source.rs",
            "https://github.com/swiftraccoon/simplestChat",
        ),
        (
            "a".repeat(40),
            "main",
            "?? untracked",
            "https://github.com/swiftraccoon/simplestChat",
        ),
        (
            "a".repeat(40),
            "main",
            "",
            "https://credential@github.com/swiftraccoon/simplestChat",
        ),
        (
            "a".repeat(40),
            "main",
            "",
            "https://github.com/other/simplestChat",
        ),
        (
            "a".repeat(40),
            "main",
            "",
            "https://github.com/swiftraccoon/simplestChat?ref=main",
        ),
    ] {
        assert!(admit_revision(&head, branch, status, remote).is_err());
    }
}

#[test]
fn original_run_and_attempt_stay_pinned_through_pending_and_success() {
    let mut pending = record();
    pending["status"] = json!("in_progress");
    pending["conclusion"] = Value::Null;
    let original = runs(&listing(pending.clone()), &revision()).unwrap();
    assert_eq!(original.id.get(), 123);
    assert_eq!(original.attempt.get(), 2);
    assert_eq!(
        run(&encode(&pending), &revision(), &original).unwrap(),
        RunProgress::Pending
    );
    assert_eq!(
        run(&encode(&record()), &revision(), &original).unwrap(),
        RunProgress::Success
    );
    for (key, value) in [("id", json!(124)), ("run_attempt", json!(3))] {
        let mut changed = record();
        changed[key] = value;
        assert!(run(&encode(&changed), &revision(), &original).is_err());
    }
    for conclusion in [
        "failure",
        "cancelled",
        "timed_out",
        "skipped",
        "action_required",
    ] {
        let mut changed = record();
        changed["conclusion"] = json!(conclusion);
        assert!(runs(&listing(changed.clone()), &revision()).is_err());
        assert!(run(&encode(&changed), &revision(), &original).is_err());
    }
    pending["status"] = json!("unknown");
    assert!(run(&encode(&pending), &revision(), &original).is_err());
    pending["status"] = json!("queued");
    pending["conclusion"] = json!("success");
    assert!(run(&encode(&pending), &revision(), &original).is_err());
}

#[test]
fn every_used_run_identity_refuses_drift_types_and_same_name_foreign_repositories() {
    let original = selected();
    for (pointer, value) in [
        ("/id", json!(true)),
        ("/id", json!(0)),
        ("/id", json!(SAFE + 1)),
        ("/id", json!(123.0)),
        ("/run_attempt", json!(false)),
        ("/run_attempt", json!(0)),
        ("/run_attempt", json!(SAFE + 1)),
        ("/head_sha", json!("b".repeat(40))),
        ("/head_branch", json!("feature")),
        ("/name", json!("Other")),
        ("/path", json!(".github/workflows/other.yml")),
        ("/event", json!("pull_request")),
        ("/repository/id", json!(REPOSITORY_ID + 1)),
        ("/repository/fork", json!(true)),
        ("/repository/owner/id", json!(OWNER_ID + 1)),
        ("/repository/owner/login", json!("other")),
        ("/repository/full_name", json!("other/simplestChat")),
        ("/head_repository/id", json!(REPOSITORY_ID + 1)),
        ("/head_repository/fork", json!(true)),
        ("/head_repository/owner/id", json!(OWNER_ID + 1)),
        ("/head_repository/full_name", json!("other/simplestChat")),
    ] {
        let mut changed = record();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(
            runs(&listing(changed.clone()), &revision()).is_err(),
            "{pointer}"
        );
        assert!(
            run(&encode(&changed), &revision(), &original).is_err(),
            "{pointer}"
        );
    }
}

#[test]
fn listing_counts_and_record_bounds_prevent_first_page_or_missing_candidate_selection() {
    for value in [
        json!({"total_count":0,"workflow_runs":[]}),
        json!({"total_count":2,"workflow_runs":[record()]}),
        json!({"total_count":1,"workflow_runs":[record(),record()]}),
        json!({"total_count":1,"workflow_runs":[]}),
        json!({"total_count":true,"workflow_runs":[record()]}),
        json!({"total_count":1.0,"workflow_runs":[record()]}),
    ] {
        assert!(runs(&encode(&value), &revision()).is_err());
    }
    let mut large = record();
    large["unused"] = json!("x".repeat(RECORD));
    assert!(runs(&listing(large), &revision()).is_err());
    let outer = json!({"total_count":1,"workflow_runs":[record()],"unused":"x".repeat(RECORD)});
    assert!(runs(&encode(&outer), &revision()).is_ok());
    assert!(runs(&vec![b' '; RESPONSE + 1], &revision()).is_err());
    let raw = String::from_utf8(encode(&record())).unwrap();
    let duplicate = raw.replacen("\"id\":123", "\"id\":123,\"id\":123", 1);
    assert!(run(duplicate.as_bytes(), &revision(), &selected()).is_err());
}

#[test]
fn both_artifact_kinds_require_exact_api_transport_and_same_selected_run() {
    for kind in [ArtifactKind::Application, ArtifactKind::Ops] {
        let record = artifact_record(kind);
        let admitted = artifact(
            &artifact_listing(record.clone()),
            &revision(),
            &selected(),
            kind,
        )
        .unwrap();
        assert_eq!(admitted.id.get(), 456);
        assert_eq!(admitted.zip_bytes, 12345);
        assert_eq!(admitted.zip_sha256, "b".repeat(64));
        for (pointer, value) in [
            ("/id", json!(true)),
            ("/id", json!(0)),
            ("/id", json!(SAFE + 1)),
            ("/expired", json!(true)),
            ("/expired", json!("false")),
            ("/name", json!("different")),
            ("/url", json!("https://foreign.invalid/456")),
            (
                "/archive_download_url",
                json!("https://foreign.invalid/456/zip"),
            ),
            ("/size_in_bytes", json!(0)),
            ("/size_in_bytes", json!(kind.maximum() + 1)),
            ("/digest", json!(format!("sha256:{}", "B".repeat(64)))),
            ("/digest", json!(format!("sha384:{}", "b".repeat(64)))),
            ("/workflow_run/id", json!(124)),
            ("/workflow_run/head_sha", json!("b".repeat(40))),
            ("/workflow_run/repository_id", json!(REPOSITORY_ID + 1)),
            ("/workflow_run/head_repository_id", json!(REPOSITORY_ID + 1)),
        ] {
            let mut changed = record.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(
                artifact(&artifact_listing(changed), &revision(), &selected(), kind).is_err(),
                "{kind:?} {pointer}"
            );
        }
        for total in [0, 2, 101] {
            assert!(
                artifact(
                    &encode(&json!({"total_count":total,"artifacts":[record.clone()]})),
                    &revision(),
                    &selected(),
                    kind
                )
                .is_err()
            );
        }
        let mut boundary = record.clone();
        boundary["size_in_bytes"] = json!(kind.maximum());
        assert!(artifact(&artifact_listing(boundary), &revision(), &selected(), kind).is_ok());
    }
}

#[test]
fn fresh_main_ref_and_json_type_boundaries_cannot_be_replaced_by_metadata_flags() {
    let original = json!({"ref":"refs/heads/main","object":{"type":"commit","sha":"a".repeat(40)}});
    main(&encode(&original), &revision()).unwrap();
    for (pointer, value) in [
        ("/ref", json!("refs/heads/other")),
        ("/object/type", json!("tag")),
        ("/object/sha", json!("b".repeat(40))),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(main(&encode(&changed), &revision()).is_err());
    }
    assert!(
        main(
            b"{\"ref\":\"refs/heads/main\",\"ref\":\"refs/heads/main\",\"object\":{}}",
            &revision()
        )
        .is_err()
    );
    assert!(runs(b"{\"total_count\":NaN,\"workflow_runs\":[]}", &revision()).is_err());
}
