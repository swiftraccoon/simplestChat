//! The retained Python helper's recorded answers are the reference; the tests use only
//! synthetic identity and owned files. No installed helper or host check runs.

use super::{
    tests::{FixtureHost, write},
    *,
};
use crate::test_support::{Recording, tally};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

/// The retained helper's receipt or failure for every case `cases` builds, each bound to the
/// SHA-256 of the case as this test serializes it (`recorded_preflight.json`).
const RECORDED: &str = include_str!("recorded_preflight.json");

fn cases() -> Vec<Value> {
    let host = FixtureHost::new();
    let base = host.request(&[]);
    let mut result = vec![
        json!({"request":"{}"}),
        json!({"request":base}),
        json!({"request":host.request(&request::FETCH)}),
        json!({"request":host.request(&["force_release.py"])}),
    ];
    for raw in ["[]", "null", "true", "{\"private\":null}"] {
        result.push(json!({"request":raw}));
    }
    for value in [
        Value::Null,
        json!(true),
        json!(4),
        json!("A".repeat(64)),
        json!("a".repeat(63)),
    ] {
        let mut raw: Value = serde_json::from_str(&base).unwrap();
        raw["release-public.py"] = value;
        result.push(json!({"request":raw.to_string()}));
    }
    for (key, values) in [
        ("euid", vec![json!(1)]),
        ("system", vec![json!("Darwin")]),
        ("machine", vec![json!("aarch64")]),
        (
            "release",
            vec![
                json!("ID=ubuntu\nVERSION_ID=13\n"),
                json!("ID=debian\nVERSION_ID=12\n"),
                json!("ID=debian\n"),
                json!("ID=debian\nVERSION_ID=13.\n"),
                json!("ID='debian'\rVERSION_ID=\"13.1\"\r\n"),
                json!("ID=ubuntu\nID=debian\nVERSION_ID=13..suffix\n"),
                json!("ID=debian \nVERSION_ID=13\n"),
                json!("ID='debian\"\nVERSION_ID=13\n"),
            ],
        ),
    ] {
        for value in values {
            let mut case = json!({"request":base});
            case[key] = value;
            result.push(case);
        }
    }
    for (path, mode) in [
        ("etc/simplestchat-public", 0o700),
        ("srv/simplestchat-public", 0o700),
        ("srv/simplestchat-public/releases", 0o700),
        ("usr/local", 0o755),
        ("usr/local/libexec/simplestchat-public", 0o755),
        (
            "usr/local/libexec/simplestchat-public/release-public.py",
            0o644,
        ),
        ("etc/simplestchat-public/images.json", 0o600),
    ] {
        for extra in [0o002, 0o1000, 0o4000] {
            result.push(json!({"request":base,"change":"mode","path":path,"mode":mode|extra}));
        }
        for owner in [[1, 0], [0, 1]] {
            result.push(json!({"request":base,"change":"owner","path":path,"owner":owner}));
        }
    }
    for (path, limit) in [
        ("etc/simplestchat-public/images.json", 65536),
        (
            "usr/local/libexec/simplestchat-public/release-public.py",
            1024 * 1024,
        ),
    ] {
        for size in [0, limit + 1] {
            result.push(json!({"request":base,"change":"size","path":path,"size":size}));
        }
    }
    for change in ["digest", "missing", "link"] {
        result.push(json!({"request":base,"change":change,
            "path":"usr/local/libexec/simplestchat-public/release-public.py"}));
    }
    for change in ["oslink", "fallback"] {
        result.push(json!({"request":base,"change":change}));
    }
    result
}

fn rust(case: &Value) -> Value {
    let mut host = FixtureHost::new();
    if let Some(value) = case.get("euid") {
        host.euid = value.as_u64().unwrap() as u32;
    }
    if let Some(value) = case.get("system") {
        host.system = value.as_str().unwrap().as_bytes().to_vec();
    }
    if let Some(value) = case.get("machine") {
        host.machine = value.as_str().unwrap().as_bytes().to_vec();
    }
    if let Some(value) = case.get("release") {
        write(
            &host.root.join("etc/os-release"),
            value.as_str().unwrap().as_bytes(),
            0o644,
        );
    }
    let path = case
        .get("path")
        .and_then(Value::as_str)
        .map(|name| host.root.join(name));
    match case.get("change").and_then(Value::as_str) {
        Some("mode") => fs::set_permissions(
            path.unwrap(),
            fs::Permissions::from_mode(case["mode"].as_u64().unwrap() as u32),
        )
        .unwrap(),
        Some("owner") => {
            host.access.0.borrow_mut().owners.insert(
                case["path"].as_str().unwrap().into(),
                (
                    case["owner"][0].as_u64().unwrap() as u32,
                    case["owner"][1].as_u64().unwrap() as u32,
                ),
            );
        }
        Some("size") => fs::write(
            path.unwrap(),
            vec![b'x'; case["size"].as_u64().unwrap() as usize],
        )
        .unwrap(),
        Some("missing") => fs::remove_file(path.unwrap()).unwrap(),
        Some("digest") => fs::write(path.unwrap(), b"CHANGED_INSTALLED_HELPER_CONTENT\n").unwrap(),
        Some("link") => {
            let path = path.unwrap();
            let retained = path.with_extension("retained");
            fs::rename(&path, &retained).unwrap();
            symlink(retained, path).unwrap();
        }
        Some("oslink" | "fallback") => {
            fs::rename(
                host.root.join("etc/os-release"),
                host.root.join("usr/lib/os-release"),
            )
            .unwrap();
            if case["change"] == "oslink" {
                symlink("../usr/lib/os-release", host.root.join("etc/os-release")).unwrap();
            }
        }
        _ => {}
    }
    let result = match host.check(case["request"].as_str().unwrap()) {
        Ok(receipt) => json!({"receipt":receipt}),
        Err(error) => json!({"failure":error.code()}),
    };
    host.settled();
    result
}

#[test]
fn recorded_python_reference_matches_complete_host_contract_and_public_failures() {
    const TEST: &str =
        "recorded_python_reference_matches_complete_host_contract_and_public_failures";
    let cases = cases();
    let mut recorded = Recording::new(RECORDED, &[TEST]).cases(TEST);
    let mut counted = std::collections::BTreeMap::new();
    for (index, case) in cases.iter().enumerate() {
        let expected = recorded.answer(&serde_json::to_vec(case).unwrap());
        let actual = rust(case);
        assert_eq!(actual, expected, "case {index}");
        let outcome = actual["failure"].as_str().unwrap_or("receipt").to_string();
        *counted.entry(outcome).or_insert(0) += 1;
    }
    assert_eq!(recorded.finish(), 68);
    // The recorder's tally: receipts, then failures per code.
    let expected = [
        ("helper_digest_mismatch", 1),
        ("invalid_helper_digest", 5),
        ("invalid_helper_set", 4),
        ("invalid_release_file_size", 4),
        ("preflight_unavailable", 1),
        ("receipt", 9),
        ("root_required", 1),
        ("unsafe_release_path", 36),
        ("unsupported_distribution", 5),
        ("unsupported_platform", 2),
    ];
    assert_eq!(counted, tally(&expected));
}
