//! Fixed driver contract neighbors; producer Native tests execute the same
//! compiled bytes through the real retained-tool supervisor.

use super::{Fixture, driver};
use std::{ffi::OsStr, fs, os::unix::fs::PermissionsExt};

#[test]
fn malformed_invocations_emit_no_data_and_create_no_candidate() {
    let fixture = Fixture::new();
    for (role, args, status) in [
        ("rustc", vec!["--print".into()], 96),
        ("cargo", vec!["metadata".into()], 97),
        ("cargo", vec!["build".into()], 97),
        ("cc", vec![], 97),
    ] {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        assert_eq!(
            driver::execute(
                &fixture.root,
                OsStr::new(role),
                &args,
                &mut stdout,
                &mut stderr
            )
            .unwrap(),
            status
        );
        assert!(stdout.is_empty() && stderr.is_empty());
        assert!(!fixture.root.join("target").exists());
    }
}

#[test]
fn metadata_and_failure_inputs_remain_data_with_exact_feature_edges() {
    let fixture = Fixture::new();
    let metadata = fixture.metadata();
    assert_eq!(
        metadata["resolve"]["nodes"][0]["features"],
        serde_json::json!(["maintain-guest"])
    );
    assert_eq!(
        metadata["resolve"]["nodes"][0]["deps"],
        serde_json::json!([
            {"name":"openssl","pkg":"fixture-openssl"},
            {"name":"x509_parser","pkg":"fixture-x509"},
            {"name":"asn1_rs","pkg":"fixture-asn1"}
        ])
    );
    assert_eq!(metadata["packages"][1]["version"], "0.10.81");
    assert_eq!(metadata["packages"][2]["version"], "0.18.1");
    assert_eq!(metadata["packages"][3]["name"], "asn1-rs");
    assert_eq!(metadata["packages"][3]["version"], "0.7.2");
    assert_eq!(
        metadata["resolve"]["nodes"][3]["features"],
        serde_json::json!(["bigint"])
    );
    let args = driver::arguments(&fixture.root, true);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    assert_eq!(
        driver::execute(
            &fixture.root,
            OsStr::new("cargo"),
            &args,
            &mut stdout,
            &mut stderr
        )
        .unwrap(),
        0
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&stdout).unwrap(),
        metadata
    );
    assert!(stderr.is_empty());
    assert!(!fixture.root.join("target").exists());
}

#[test]
fn nonzero_build_preserves_distinct_streams_and_nonexecutable_candidate() {
    let fixture = Fixture::new();
    fixture.fail_build();
    let payload = b"inert candidate\0bytes";
    fs::write(fixture.kit.join("toolchain/fixture/candidate.elf"), payload).unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let args = driver::arguments(&fixture.root, false);
    assert_eq!(
        driver::execute(
            &fixture.root,
            OsStr::new("cargo"),
            &args,
            &mut stdout,
            &mut stderr
        )
        .unwrap(),
        100
    );
    assert_eq!(stdout, b"PRIVATE-STDOUT\n");
    assert_eq!(stderr, b"PRIVATE-STDERR\n");
    let candidate = fixture
        .root
        .join("target/x86_64-unknown-linux-musl/release/simplestchat-ops");
    assert_eq!(fs::read(&candidate).unwrap(), payload);
    assert_eq!(
        fs::metadata(candidate).unwrap().permissions().mode() & 0o7777,
        0o600
    );
}

#[test]
fn invalid_or_oversized_status_refuses_before_any_candidate_effect() {
    let fixture = Fixture::new();
    for invalid in [b"1\n".as_slice(), b"100\nextra", b"00\n", b"0"] {
        fs::write(fixture.kit.join("toolchain/fixture/build-status"), invalid).unwrap();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let args = driver::arguments(&fixture.root, false);
        assert!(
            driver::execute(
                &fixture.root,
                OsStr::new("cargo"),
                &args,
                &mut stdout,
                &mut stderr
            )
            .is_err()
        );
        assert!(stdout.is_empty() && stderr.is_empty());
        assert!(!fixture.root.join("target").exists());
    }
}
