//! The release builder's explicit UTF-8 metadata profile and actual guard policy.
//!
//! Ignored fields still pass the shared strict decoder. These are accepted-domain
//! fixtures, not a claim to accept every metadata object the Python reference can.

use crate::json::Json;
use crate::release::build::{
    control::{Failure, Owner},
    decode_metadata, files,
    guard::Guards,
    json,
};
use crate::test_support::Fixture;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
    time::Instant,
};

const INSPECTION: usize = 2 * 1024 * 1024;
const CURRENT: usize = 64 * 1024;
const PERSISTENT: usize = 16 * 1024;
const IMAGE_PREFIX: &str = concat!(
    r#"{"id":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","#,
    r#""os":"linux","architecture":"amd64","user":"10001:10001","#,
    r#""labels":{"org.opencontainers.image.revision":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"#,
    r#""rootfs":{"Type":"layers","Layers":["#,
    r#""sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"]},"#,
    r#""cmd":["/app/simplestChat"],"entrypoint":null,"ignored":"#,
);

fn image(ignored: &str) -> Vec<u8> {
    let mut bytes = IMAGE_PREFIX.as_bytes().to_vec();
    bytes.extend_from_slice(ignored.as_bytes());
    bytes.push(b'}');
    bytes
}

fn state(ignored: &str) -> Vec<u8> {
    format!(r#"{{"schemaVersion":1,"finalized":true,"ignored":{ignored}}}"#).into_bytes()
}

fn ignored(value: &Json) -> &Json {
    value.get("ignored").unwrap()
}

fn image_shape(value: &Json) {
    assert_eq!(value.get("os").and_then(Json::string), Some("linux"));
    assert_eq!(
        value.get("architecture").and_then(Json::string),
        Some("amd64")
    );
    assert_eq!(
        value.get("user").and_then(Json::string),
        Some("10001:10001")
    );
    assert_eq!(
        value.get("cmd").and_then(Json::array).unwrap()[0].string(),
        Some("/app/simplestChat")
    );
    assert!(matches!(value.get("entrypoint"), Some(Json::Null)));
    assert_eq!(
        value
            .get("rootfs")
            .unwrap()
            .get("Type")
            .and_then(Json::string),
        Some("layers")
    );
}

fn filled(prefix: &[u8], suffix: &[u8], total: usize) -> Vec<u8> {
    let mut bytes = prefix.to_vec();
    bytes.resize(total - suffix.len(), b'x');
    bytes.extend_from_slice(suffix);
    assert_eq!(bytes.len(), total);
    bytes
}

fn image_size(total: usize) -> Vec<u8> {
    let mut prefix = IMAGE_PREFIX.as_bytes().to_vec();
    prefix.push(b'"');
    filled(&prefix, b"\"}", total)
}

fn state_size(total: usize) -> Vec<u8> {
    filled(
        br#"{"schemaVersion":1,"finalized":true,"ignored":""#,
        b"\"}",
        total,
    )
}

fn private(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
    files::close(file).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn guard(record: &[u8], persistent: bool) -> std::result::Result<(), Failure> {
    let fixture = Fixture::new();
    let benchmark = fixture.0.join("benchmark");
    let release = fixture.0.join("persistent/release-state.json");
    if persistent {
        fs::create_dir(release.parent().unwrap()).unwrap();
        fs::set_permissions(release.parent().unwrap(), fs::Permissions::from_mode(0o700)).unwrap();
        private(&release, record);
    } else {
        fs::create_dir(&benchmark).unwrap();
        fs::set_permissions(&benchmark, fs::Permissions::from_mode(0o700)).unwrap();
        private(&benchmark.join("workload.lock"), &[]);
        private(&benchmark.join("current.json"), record);
    }
    let mut owner = Owner::new(
        Instant::now(),
        crate::release::build::control::Mode::Build,
        120,
    )
    .unwrap();
    let end = owner.end();
    let admitted = Guards::fixture(&benchmark, &release, &mut owner, end)?;
    let checked = admitted.check();
    let closed = admitted.finish();
    checked.and(closed)
}

#[test]
fn required_image_shape_keeps_ignored_boolean_null_and_unicode_values_typed() {
    for token in [
        "true",
        "false",
        "null",
        r#""Unicode: λ 😀""#,
        r#""\ud83d\ude00""#,
        r#""\u0000""#,
        r#""NaN Infinity""#,
    ] {
        let value = json(&image(token)).unwrap();
        image_shape(&value);
        match token {
            "true" => assert!(matches!(ignored(&value), Json::Bool(true))),
            "false" => assert!(matches!(ignored(&value), Json::Bool(false))),
            "null" => assert!(matches!(ignored(&value), Json::Null)),
            r#""\ud83d\ude00""# => assert_eq!(ignored(&value).string(), Some("😀")),
            r#""\u0000""# => assert_eq!(ignored(&value).string(), Some("\0")),
            _ => assert!(matches!(ignored(&value), Json::String(_))),
        }
    }
}

#[test]
fn large_ignored_integers_keep_raw_lexemes_without_u64_or_f64_roundtrip() {
    for token in [
        "18446744073709551616".into(),
        "-9223372036854775809".into(),
        "9007199254740993".into(),
        "9".repeat(400),
        "9".repeat(4300),
        format!("-{}", "9".repeat(4300)),
        "-0".into(),
    ] {
        let value = json(&image(&token)).unwrap();
        assert!(matches!(ignored(&value), Json::Integer(actual) if actual == &token));
        assert_eq!(
            serde_json::to_vec(ignored(&value)).unwrap(),
            token.as_bytes()
        );
    }
    for token in ["9".repeat(4301), format!("-{}", "9".repeat(4301))] {
        assert!(matches!(json(&image(&token)), Err(Failure::Docker)));
    }
}

#[test]
fn finite_float_spelling_and_underflow_survive_but_nonfinite_tokens_do_not() {
    for token in [
        "1.0",
        "-0.0",
        "1e-9999",
        "-1e-9999",
        "0e9999",
        "1e+308",
        "1.00000000000000000000000000000000000001",
    ] {
        let value = json(&image(token)).unwrap();
        assert!(matches!(ignored(&value), Json::Float(actual) if actual == token));
        assert_eq!(
            serde_json::to_vec(ignored(&value)).unwrap(),
            token.as_bytes()
        );
    }
    for token in [
        "NaN",
        "Infinity",
        "-Infinity",
        "+Infinity",
        "1e999",
        "-1e999",
        "1e+99999999999999999999",
    ] {
        assert!(
            matches!(json(&image(token)), Err(Failure::Docker)),
            "{token}"
        );
    }
    assert!(json(&image(&format!("{}.0", "9".repeat(400)))).is_err());
}

#[test]
fn ignored_container_depth_and_total_values_stop_at_the_existing_shared_profile() {
    // The required root object plus 127 nested ignored arrays is 128 containers.
    let nested = |depth: usize| format!("{}null{}", "[".repeat(depth), "]".repeat(depth));
    assert!(json(&image(&nested(127))).is_ok());
    assert!(json(&image(&nested(128))).is_err());

    // Fourteen required image values plus the ignored array and its scalar values.
    let array = |count: usize| format!("[{}]", vec!["0"; count].join(","));
    let value = json(&image(&array(99_985))).unwrap();
    assert_eq!(ignored(&value).array().unwrap().len(), 99_985);
    assert!(json(&image(&array(99_986))).is_err());
}

#[test]
fn image_inspection_uses_its_full_two_mib_bound_instead_of_default_one_mib() {
    let over_default = image_size(1024 * 1024 + IMAGE_PREFIX.len() + 4);
    assert!(crate::json::decode(&over_default).is_err());
    let value = json(&over_default).unwrap();
    image_shape(&value);
    assert_eq!(ignored(&value).string().unwrap().len(), 1024 * 1024 + 1);
    assert!(json(&image_size(INSPECTION)).is_ok());
    assert!(matches!(
        json(&image_size(INSPECTION + 1)),
        Err(Failure::Docker)
    ));
}

#[test]
fn decoded_duplicate_keys_and_lone_surrogates_are_refused_even_when_ignored() {
    for token in [
        r#"{"a":1,"\u0061":2}"#,
        r#"[{"same":null,"same":null}]"#,
        r#""\ud800""#,
        r#""\udfff""#,
        r#""\ud800A""#,
        r#"{"\ud800":"value"}"#,
    ] {
        assert!(
            matches!(json(&image(token)), Err(Failure::Docker)),
            "{token}"
        );
    }
    assert!(decode_metadata(br#"{"schemaVersion":1,"\u0073chemaVersion":1}"#, CURRENT).is_err());
}

#[test]
fn metadata_consumes_utf8_strings_without_byte_encoding_autodetection() {
    let normal = state("null");
    assert!(decode_metadata(&normal, CURRENT).is_ok());
    let mut bom = vec![0xef, 0xbb, 0xbf];
    bom.extend_from_slice(&normal);
    assert!(crate::json::decode(&bom).is_ok());
    assert!(matches!(
        decode_metadata(&bom, CURRENT),
        Err(Failure::Docker)
    ));
    let utf16: Vec<_> = normal.iter().flat_map(|byte| [*byte, 0]).collect();
    assert!(crate::json::decode(&utf16).is_ok());
    assert!(matches!(
        decode_metadata(&utf16, CURRENT),
        Err(Failure::Docker)
    ));
    assert!(matches!(
        decode_metadata(b"\xff", CURRENT),
        Err(Failure::Utf8)
    ));
    let raw_nul = b"{\"ignored\":\"\0\"}";
    assert!(matches!(
        decode_metadata(raw_nul, CURRENT),
        Err(Failure::Docker)
    ));
    assert!(decode_metadata("{\"ignored\":\"\u{feff}\"}".as_bytes(), CURRENT).is_ok());
}

#[tokio::test(flavor = "current_thread")]
async fn actual_guard_accepts_required_typed_fields_and_large_ignored_integer_tokens() {
    for persistent in [false, true] {
        for token in [
            "null".into(),
            "9".repeat(400),
            "9".repeat(4300),
            r#""\ud83d\ude00""#.into(),
            "1e-9999".into(),
        ] {
            guard(&state(&token), persistent).unwrap();
        }
        for record in [
            br#"{"schemaVersion":true,"finalized":true}"#.as_slice(),
            br#"{"schemaVersion":1.0,"finalized":true}"#.as_slice(),
            br#"{"schemaVersion":1,"finalized":1}"#.as_slice(),
            br#"{"schemaVersion":1,"finalized":null}"#.as_slice(),
        ] {
            assert_eq!(guard(record, persistent), Err(Failure::Guard));
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn actual_guard_validates_ignored_values_and_string_encoding_before_policy() {
    for persistent in [false, true] {
        for token in [
            "NaN",
            "Infinity",
            "-Infinity",
            "1e999",
            r#""\ud800""#,
            r#"{"key":1,"\u006bey":2}"#,
        ] {
            assert_eq!(guard(&state(token), persistent), Err(Failure::Guard));
        }
        assert_eq!(
            guard(&state(&"9".repeat(4301)), persistent),
            Err(Failure::Guard)
        );
        let deep = format!("{}null{}", "[".repeat(128), "]".repeat(128));
        assert_eq!(guard(&state(&deep), persistent), Err(Failure::Guard));
        let mut bom = vec![0xef, 0xbb, 0xbf];
        bom.extend_from_slice(&state("null"));
        assert_eq!(guard(&bom, persistent), Err(Failure::Guard));
        let utf16: Vec<_> = state("null").iter().flat_map(|byte| [*byte, 0]).collect();
        assert_eq!(guard(&utf16, persistent), Err(Failure::Guard));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn actual_current_and_persistent_records_keep_their_distinct_encoded_byte_bounds() {
    for (persistent, limit) in [(false, CURRENT), (true, PERSISTENT)] {
        guard(&state_size(limit), persistent).unwrap();
        assert_eq!(
            guard(&state_size(limit + 1), persistent),
            Err(Failure::Guard)
        );
    }
}
