//! Pure producer-policy regressions.

use super::{fixture::*, *};

fn wire() -> serde_json::Value {
    serde_json::json!({"schemaVersion":1,"selection":{"repository":"owner/repo",
        "revision":"a".repeat(40),"packageVersion":"0.1.0","protocolVersion":1,
        "target":TARGET,"buildRunId":1,"runAttempt":1},"source":{"path":"/materials/source",
        "sha256":"b".repeat(64)},"builderKit":{"path":"/materials/kit","sha256":"c".repeat(64)},
        "output":"/controlled/fresh-run"})
}
fn decode(value: &serde_json::Value) -> Result<request::Request> {
    let bytes = serde_json::to_vec(value).unwrap();
    request::Request::decode(&bytes, &sha(&bytes))
}
#[test]
fn request_original_bytes_and_strict_relationships_are_admitted() {
    let value = wire();
    let request = decode(&value).unwrap();
    assert_eq!(
        request.selection.identity(),
        simplestchat_ops::distribution::manifest::Selection::decode(
            &serde_json::to_vec(&value["selection"]).unwrap()
        )
        .unwrap()
        .identity()
    );
    let bytes = serde_json::to_vec_pretty(&value).unwrap();
    assert!(request::Request::decode(&bytes, &request.sha256).is_err());
    assert!(!format!("{request:?}").contains("materials"));
}
#[test]
fn request_refuses_scope_types_numbers_unknowns_and_aliases() {
    for (field, value) in [
        ("schemaVersion", serde_json::json!(1.0)),
        ("selection", serde_json::json!({})),
        ("PRIVATE-KEY", serde_json::json!("PRIVATE-VALUE")),
        ("output", serde_json::json!("/materials/source/run")),
        ("output", serde_json::json!("/materials/../escape")),
        ("output", serde_json::json!("/controlled:injected/run")),
    ] {
        let mut request = wire();
        request[field] = value;
        assert!(decode(&request).is_err(), "{field}");
    }
    for field in ["buildRunId", "runAttempt", "protocolVersion"] {
        for invalid in [
            serde_json::json!(true),
            serde_json::json!(1.0),
            serde_json::json!(null),
        ] {
            let mut request = wire();
            request["selection"][field] = invalid;
            assert!(decode(&request).is_err());
        }
    }
    let mut request = wire();
    request["builderKit"]["path"] = "/materials/source/kit".into();
    assert!(decode(&request).is_err());
}
#[test]
fn duplicate_decoded_keys_and_oversized_requests_are_refused() {
    let original = serde_json::to_string(&wire()).unwrap();
    let duplicate = original.replacen("{", "{\"schemaVersion\":1,", 1);
    assert!(request::Request::decode(duplicate.as_bytes(), &sha(duplicate.as_bytes())).is_err());
    let mut oversized = original.into_bytes();
    oversized.resize(64 * 1024 + 1, b' ');
    assert!(request::Request::decode(&oversized, &sha(&oversized)).is_err());
}

#[test]
fn only_output_paths_need_the_path_list_colon_refusal() {
    let mut value = wire();
    value["source"]["path"] = "/materials/source:opaque".into();
    assert!(decode(&value).is_ok());
    value["output"] = "/controlled/fresh:run".into();
    assert!(decode(&value).is_err());
}
#[test]
fn watchdog_reserves_cleanup_and_never_admits_zero_body() {
    let clock = FakeClock::new();
    let deadline = Deadline::new(&clock);
    assert_eq!(
        deadline.body(Duration::from_secs(2000)).unwrap(),
        Duration::from_secs(1055)
    );
    clock.0.set(clock.now() + Duration::from_secs(1055));
    assert_eq!(deadline.body(Duration::from_secs(1)).unwrap_err().code, 124);
    clock.0.set(clock.now() + Duration::from_secs(25));
    assert!(deadline.check(false).is_err());
    assert!(deadline.check(true).is_ok());
    clock.0.set(clock.now() + RESERVE);
    assert!(deadline.check(true).is_err());
}
#[test]
fn descriptor_cap_is_checked_and_partial_reservations_unwind() {
    let budget = namespace::Budget::default();
    let held = budget.reserve(128).unwrap();
    assert_eq!(budget.count(), 128);
    assert!(budget.permit().is_err());
    drop(held);
    assert_eq!(budget.count(), 0);
    assert!(budget.reserve(129).is_err());
    assert_eq!(budget.count(), 0);
}
#[test]
fn tool_versions_are_bounded_observations_not_private_output() {
    let bytes = concat!(
        "rustc 1.99.0 (fixture 2026-10-22)\nrelease: 1.99.0\n",
        "host: x86_64-unknown-linux-gnu\nPRIVATE: hidden\n"
    )
    .as_bytes();
    let result = resolution::version(bytes, false).unwrap().to_string();
    assert!(!result.contains("hidden"));
    assert_eq!(resolution::version(bytes, true).unwrap_err().code, 1);
    assert!(resolution::version(&bytes.repeat(200), false).is_err());
    assert!(resolution::version(&[bytes, b"release: 1.99.0\n"].concat(), false).is_err());
}
