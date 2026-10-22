//! New schema assertions, not Python parity: Python has no Ops bundle contract.

use super::*;
use crate::test_support::Fixture;
use serde_json::{Value, json};
use std::{fs, os::unix::fs::symlink};

use super::super::fixture as elf;
#[path = "distribution_manifest_fixture.rs"]
mod fixture;

fn source() -> Value {
    fixture::manifest(&elf::fixture(true, false, false))
}
fn decode(value: &Value) -> Result<ArtifactManifest, Failure> {
    ArtifactManifest::decode(&serde_json::to_vec(value).unwrap())
}
fn selection() -> Selection {
    Selection::decode(&serde_json::to_vec(&fixture::selection()).unwrap()).unwrap()
}

#[test]
fn selected_source_is_bounded_and_distinguishes_the_new_scope() {
    assert!(Selection::new("owner/repo", &"a".repeat(40), "0.1.0", 1, TARGET, 1, 1).is_ok());
    assert!(
        Selection::new(
            "owner/repo",
            &"a".repeat(40),
            "0.1.0",
            1,
            "linux/amd64",
            1,
            1
        )
        .is_err()
    );
    for key in SOURCE_KEYS {
        let mut selected = fixture::selection();
        selected.as_object_mut().unwrap().remove(key);
        assert!(Selection::decode(&serde_json::to_vec(&selected).unwrap()).is_err());
    }
    let mut selected = fixture::selection();
    selected["schemaVersion"] = json!(1);
    assert!(Selection::decode(&serde_json::to_vec(&selected).unwrap()).is_err());
    selected.as_object_mut().unwrap().remove("schemaVersion");
    selected["protocolVersion"] = json!(2);
    assert!(Selection::decode(&serde_json::to_vec(&selected).unwrap()).is_err());
}

#[test]
fn exact_new_schema_refuses_server_contracts_unknown_fields_and_mixed_numeric_types() {
    for value in [
        json!({}),
        json!([]),
        json!(null),
        json!({"schemaVersion":1,
        "revision":"a".repeat(40),"platform":"linux/amd64","archiveSha256":"b".repeat(64),
        "imageTag":"server","migrations":{},"createdAt":"2026-10-22T00:00:00Z"}),
    ] {
        assert!(decode(&value).is_err());
    }
    for key in KEYS {
        let mut value = source();
        value.as_object_mut().unwrap().remove(key);
        assert!(decode(&value).is_err(), "missing {key}");
    }
    let mut value = source();
    value["unknown"] = json!("PRIVATE-SENTINEL");
    assert_eq!(decode(&value).unwrap_err(), Failure::Fields);
    for key in [
        "schemaVersion",
        "protocolVersion",
        "buildRunId",
        "runAttempt",
    ] {
        for bad in [
            json!(true),
            json!(1.0),
            json!("1"),
            json!(null),
            json!(-1),
            json!(0),
        ] {
            let mut value = source();
            value[key] = bad;
            assert!(decode(&value).is_err(), "{key}");
        }
    }
    for bad in [
        json!(true),
        json!(4096.0),
        json!("4096"),
        json!(0),
        json!(-1),
        json!(super::super::MAX_BYTES + 1),
    ] {
        let mut value = source();
        value["binary"]["bytes"] = bad;
        assert_eq!(decode(&value).unwrap_err(), Failure::Binary);
    }
    for key in ["buildRunId", "runAttempt"] {
        let mut value = source();
        value[key] = json!(i64::MAX);
        assert!(decode(&value).is_ok());
        value[key] = json!(i64::MAX as u64 + 1);
        assert!(decode(&value).is_err());
    }
}

#[test]
fn canonical_source_version_scope_and_names_have_explicit_bounds() {
    for (key, bad) in [
        ("artifactType", "server"),
        ("repository", "owner/repo/other"),
        ("repository", "../repo"),
        ("repository", "owner/private value"),
        ("revision", "Aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        ("packageVersion", "01.0.0"),
        ("packageVersion", "1.2"),
        ("packageVersion", "1.2.3-beta"),
        ("packageVersion", "1.2.3+build"),
        ("packageVersion", "1.2.4294967296"),
        ("packageVersion", "١.2.3"),
        ("target", "linux/amd64"),
        ("target", "aarch64-unknown-linux-musl"),
    ] {
        let mut value = source();
        value[key] = json!(bad);
        assert!(decode(&value).is_err(), "{key}={bad}");
    }
    for (key, bad) in [
        ("name", "simplestChat"),
        ("sha256", "A"),
        ("name", "../simplestchat-ops"),
    ] {
        let mut value = source();
        value["binary"][key] = json!(bad);
        assert!(decode(&value).is_err());
    }
    let mut value = source();
    value["binary"]["extra"] = json!(0);
    assert!(decode(&value).is_err());
    for version in ["0.0.0", "4294967295.4294967295.4294967295"] {
        let mut value = source();
        value["packageVersion"] = json!(version);
        assert!(decode(&value).is_ok());
    }
}

#[test]
fn digest_maps_are_bounded_private_declarations_and_never_filesystem_requests() {
    for key in ["inputDigests", "evidenceDigests"] {
        for name in [
            "",
            "/absolute",
            "../private",
            "foo/../private",
            "foo//bar",
            "foo/./bar",
            "back\\slash",
            "private value",
            "unicode/é",
            &"a".repeat(129),
        ] {
            let mut value = source();
            value[key] = json!({name:"f".repeat(64)});
            assert!(decode(&value).is_err(), "{name}");
        }
        for digest in [
            json!(null),
            json!(true),
            json!("f".repeat(63)),
            json!("A".repeat(64)),
        ] {
            let mut value = source();
            value[key] = json!({"safe":digest});
            assert!(decode(&value).is_err());
        }
        let count = if key == "inputDigests" { 64 } else { 16 };
        let mut value = source();
        value[key] = Value::Object(
            (0..count)
                .map(|n| (format!("x/{n}"), json!("f".repeat(64))))
                .collect(),
        );
        assert!(decode(&value).is_ok());
        value[key]["overflow"] = json!("e".repeat(64));
        assert!(decode(&value).is_err());
    }
    let mut value = source();
    value["inputDigests"] = json!({});
    assert_eq!(decode(&value).unwrap_err(), Failure::Digests);
    let manifest = decode(&source()).unwrap();
    assert_eq!(
        manifest.input_digest("PRIVATE-SENTINEL/source.json"),
        Some("b".repeat(64).as_str())
    );
    assert!(manifest.evidence_digest("unobserved.json").is_none());
    assert!(!format!("{manifest:?}").contains("PRIVATE-SENTINEL"));
}

#[test]
fn original_encodings_duplicates_limits_and_snapshot_file_admission_are_explicit() {
    let value = source();
    let json = serde_json::to_string(&value).unwrap();
    let mut cases = vec![
        json.as_bytes().to_vec(),
        [&b"\xef\xbb\xbf"[..], json.as_bytes()].concat(),
    ];
    cases.push(json.encode_utf16().flat_map(u16::to_le_bytes).collect());
    for original in cases {
        let manifest = ArtifactManifest::decode(&original).unwrap();
        assert_eq!(manifest.as_json(), original);
        assert_eq!(manifest.sha256(), fixture::digest(&original));
    }
    for malformed in [
        "{\"schemaVersion\":1,\"schemaVersion\":1}",
        "{\"schemaVersion\":1,\"schema\\u0056ersion\":1}",
        "{\"x\":NaN}",
        "{\"x\":Infinity}",
        "{\"x\":1e9999}",
    ] {
        assert_eq!(
            ArtifactManifest::decode(malformed.as_bytes()).unwrap_err(),
            Failure::Json
        );
    }
    let nested_duplicate = json.replacen(
        "\"name\":",
        "\"na\\u006de\":\"simplestchat-ops\",\"name\":",
        1,
    );
    assert_eq!(
        ArtifactManifest::decode(nested_duplicate.as_bytes()).unwrap_err(),
        Failure::Json
    );
    let deep = format!("{}0{}", "[".repeat(129), "]".repeat(129));
    assert_eq!(
        ArtifactManifest::decode(deep.as_bytes()).unwrap_err(),
        Failure::Json
    );
    assert_eq!(
        ArtifactManifest::decode(br#"{"x":"\ud800"}"#).unwrap_err(),
        Failure::Json
    );
    let mut original = json.into_bytes();
    original.resize(MAX_MANIFEST_BYTES, b' ');
    assert!(ArtifactManifest::decode(&original).is_ok());
    original.push(b' ');
    assert!(ArtifactManifest::decode(&original).is_err());
    let directory = Fixture::new();
    let path = directory.0.join("ops-release.json");
    fs::write(&path, &original).unwrap();
    assert!(ArtifactManifest::read(&path).is_err());
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let link = directory.0.join("link");
    symlink(&path, &link).unwrap();
    assert!(ArtifactManifest::read(&link).is_err());
    let mut selected = serde_json::to_vec(&fixture::selection()).unwrap();
    selected.resize(MAX_SELECTION_BYTES, b' ');
    assert!(Selection::decode(&selected).is_ok());
    selected.push(b' ');
    assert!(Selection::decode(&selected).is_err());
}

#[test]
fn pair_uses_the_pinned_manifest_once_and_separates_selected_claims_from_linkage_facts() {
    let directory = Fixture::new();
    let binary = directory.0.join("candidate");
    let manifest_path = directory.0.join("ops-release.json");
    let executable = elf::fixture(true, false, false);
    fs::write(&binary, &executable).unwrap();
    let original = serde_json::to_vec_pretty(&source()).unwrap();
    fs::write(&manifest_path, &original).unwrap();
    let manifest = ArtifactManifest::read(&manifest_path).unwrap();
    // An external verifier may have authenticated this exact SHA. A replaced
    // source path must never cause a different document to be reopened here.
    fs::write(&manifest_path, b"PRIVATE-SENTINEL replacement").unwrap();
    let pair = verify_pair(&manifest, &binary, &selection()).unwrap();
    let output = serde_json::to_value(&pair).unwrap();
    assert_eq!(output["manifestSha256"], fixture::digest(&original));
    assert_eq!(output["manifestBytes"], original.len());
    assert_eq!(output["claims"]["target"], TARGET);
    assert_eq!(output["claims"]["evidenceDigestCount"], 0);
    assert_eq!(output["executable"]["sha256"], fixture::digest(&executable));
    assert_eq!(output["executable"]["noNeeded"], true);
    let output = serde_json::to_string(&pair).unwrap();
    for hidden in [
        "PRIVATE-SENTINEL",
        "inputDigests",
        "evidenceDigests",
        "passed",
        "authenticated",
    ] {
        assert!(!output.contains(hidden));
    }
    assert!(!format!("{pair:?}").contains("PRIVATE-SENTINEL"));
    for key in SOURCE_KEYS {
        let mut changed = fixture::selection();
        changed[key] = match key {
            "repository" => json!("other/repo"),
            "revision" => json!("b".repeat(40)),
            "packageVersion" => json!("0.1.1"),
            "buildRunId" => json!(124),
            "runAttempt" => json!(2),
            _ => continue,
        };
        let selected = Selection::decode(&serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(matches!(
            verify_pair(&manifest, &directory.0.join("missing"), &selected),
            Err(Failure::Identity)
        ));
    }
    fs::write(&binary, &executable[..128]).unwrap();
    assert!(matches!(
        verify_pair(&manifest, &binary, &selection()),
        Err(Failure::Executable)
    ));
}

#[test]
fn declared_target_refuses_interpreter_needed_or_loader_hooks_without_claiming_musl() {
    let directory = Fixture::new();
    let path = directory.0.join("candidate");
    for (interpreter, needed, hook) in [
        (true, false, false),
        (false, true, false),
        (false, false, true),
    ] {
        let mut bytes = elf::fixture(true, interpreter, needed);
        if hook {
            elf::dynamic(
                &mut bytes,
                &[
                    (5, elf::BASE + elf::STRINGS as u64),
                    (10, 28),
                    (0x6ffffefc, 1),
                    (0, 0),
                ],
            );
        }
        fs::write(&path, &bytes).unwrap();
        let manifest = decode(&fixture::manifest(&bytes)).unwrap();
        assert!(matches!(
            verify_pair(&manifest, &path, &selection()),
            Err(Failure::Linkage)
        ));
    }
}
