use super::*;
use crate::test_support::Fixture;
use serde_json::{Value, json};
use std::io::Write;
use std::process::{Command, Stdio};

fn fixture() -> Value {
    json!({"schemaVersion":1,"revision":"a".repeat(40),"platform":"linux/amd64",
        "archiveSha256":"b".repeat(64),"imageTag":format!("simplestchat-release/production:{}","a".repeat(40)),
        "migrations":{"1":"c".repeat(96)},"createdAt":"2026-10-21T01:02:03Z"})
}
fn oracle(cases: &[String]) {
    oracle_bytes(
        &cases
            .iter()
            .map(|s| s.as_bytes().to_vec())
            .collect::<Vec<_>>(),
    );
}
fn oracle_bytes(cases: &[Vec<u8>]) {
    let script = r#"
import json,pathlib,sys,tempfile
sys.path.insert(0,sys.argv[1]+'/ops/ansible/files')
import release_artifact as p
mapping={
'Release manifest must be a bounded regular file':'manifest_input_refused',
'Invalid artifact JSON':'invalid_json','Unsupported release manifest fields':'release_manifest_fields',
'Unsupported release schema':'release_schema','Release revision must be a full Git commit ID':'release_revision',
'Only linux/amd64 releases are supported':'release_platform','Invalid archive SHA256':'archive_digest',
'Release tag does not match its revision':'release_tag',
'Release migrations must be a nonempty bounded map':'release_migrations',
'Migration versions must be canonical positive decimal integers':'migration_version',
'Invalid migration SHA384':'migration_digest',
'Release timestamp must be UTC with second precision':'release_timestamp_shape',
'Invalid release timestamp':'release_timestamp'}
results=[]
with tempfile.TemporaryDirectory(prefix='ops-manifest-oracle-') as directory:
 path=pathlib.Path(directory)/'release.json'
 for source in json.load(sys.stdin):
  path.write_bytes(bytes(source))
  try:results.append({'ok':True,'value':p.validate_manifest(path)})
  except p.ArtifactError as error:
   text=str(error)
   results.append({'ok':False,'error':'ambiguous_json' if text.startswith('Duplicate JSON key:') else mapping[text]})
json.dump(results,sys.stdout)
"#;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut child = Command::new("python3")
        .args(["-I", "-c", script])
        .arg(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(cases).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    eprintln!("checked {} actual Python manifest cases", cases.len());
    for (index, (source, expected)) in cases.iter().zip(expected).enumerate() {
        let actual = match Manifest::decode(source) {
            Ok(value) => json!({"ok":true,"value":value.fields()}),
            Err(error) => json!({"ok":false,"error":error.code()}),
        };
        assert_eq!(actual, expected, "case{index}: {source:?}");
    }
}

#[test]
fn actual_python_oracle_checks_types_field_order_migrations_and_dates() {
    let mut cases = vec![fixture().to_string()];
    for key in KEYS {
        let mut missing = fixture();
        missing.as_object_mut().unwrap().remove(key);
        cases.push(missing.to_string());
        for value in [
            Value::Null,
            json!(false),
            json!(1.0),
            json!([]),
            json!({}),
            json!("wrong"),
        ] {
            let mut input = fixture();
            input[key] = value;
            cases.push(input.to_string());
        }
    }
    for (key, values) in [
        (
            "schemaVersion",
            vec![json!(0), json!(2), json!(true), json!(-1)],
        ),
        (
            "revision",
            vec![
                json!("A".repeat(40)),
                json!("a".repeat(39)),
                json!("a".repeat(41)),
            ],
        ),
        (
            "archiveSha256",
            vec![json!("A".repeat(64)), json!("a".repeat(63))],
        ),
        (
            "migrations",
            vec![
                json!({}),
                json!({"0":"c".repeat(96)}),
                json!({"01":"c".repeat(96)}),
                json!({"9223372036854775807":"c".repeat(96)}),
                json!({"9223372036854775808":"c".repeat(96)}),
                json!({"-1":"c".repeat(96)}),
                json!({"1":"C".repeat(96)}),
                json!({"1":false}),
            ],
        ),
        (
            "createdAt",
            vec![
                json!("0000-01-01T00:00:00Z"),
                json!("0001-01-01T00:00:00Z"),
                json!("9999-12-31T23:59:59Z"),
                json!("2024-02-29T00:00:00Z"),
                json!("2026-02-29T00:00:00Z"),
                json!("2026-10-21T23:59:60Z"),
                json!("2026-10-21T00:00:00+00:00"),
                json!("2026-10-21T00:00:00.0Z"),
            ],
        ),
    ] {
        for value in values {
            let mut input = fixture();
            input[key] = value;
            cases.push(input.to_string());
        }
    }
    let valid = fixture().to_string();
    for token in [
        "NaN",
        "Infinity",
        "-Infinity",
        "1e999",
        "-1e999",
        "9007199254740993",
        "1.00000000000000000000000000000000000001",
        "-0",
        "-0.0",
    ] {
        cases.push(valid.replace("\"schemaVersion\":1", &format!("\"schemaVersion\":{token}")));
    }
    for raw in [
        "",
        "null",
        "[]",
        "{}",
        "null trailing",
        "[1,]",
        "{\"a\":1,\"\\u0061\":2}",
        "{\"a\":[{\"private\":1,\"private\":2}]}",
    ] {
        cases.push(raw.into());
    }
    cases.push(format!("\u{feff}{valid}"));
    for digits in [4300, 4301] {
        cases.push("1".repeat(digits));
        cases.push(valid.replace(
            "\"schemaVersion\":1",
            &format!("\"schemaVersion\":{}", "1".repeat(digits)),
        ));
    }
    let unknown_nonfinite = valid.replacen("{", "{\"private\":1e999,", 1);
    cases.push(unknown_nonfinite);
    let mut unknown = fixture();
    unknown["private"] = json!({"nested":[1,true]});
    cases.push(unknown.to_string());
    // The first bad migration follows source order, not lexical key sorting.
    let bad = valid.replace("\"1\":", "\"2\":false,\"01\":");
    cases.push(bad);
    oracle(&cases);
}

#[test]
fn every_unicode_decimal_set_and_timestamp_position_matches_python() {
    let hir = regex_syntax::Parser::new().parse(r"\d").unwrap();
    let HirKind::Class(Class::Unicode(class)) = hir.kind() else {
        unreachable!()
    };
    let mut cases = Vec::new();
    for range in class.ranges() {
        let mut start = u32::from(range.start());
        while start <= u32::from(range.end()) {
            let make = |number: u32| char::from_u32(start + number).unwrap();
            let mut input = fixture();
            input["createdAt"] = json!(format!(
                "{}{}{}{}-10-21T01:02:03Z",
                make(2),
                make(0),
                make(2),
                make(6)
            ));
            cases.push(input.to_string());
            for position in [5, 6, 8, 9, 11, 12, 14, 15, 17, 18] {
                let mut chars: Vec<_> = "2026-10-21T01:02:03Z".chars().collect();
                chars[position] = make(chars[position].to_digit(10).unwrap());
                input["createdAt"] = json!(chars.into_iter().collect::<String>());
                cases.push(input.to_string());
            }
            start += 10;
        }
    }
    oracle(&cases);
}

#[test]
fn original_bytes_and_sha_are_not_replaced_by_the_validated_projection() {
    let bytes = serde_json::to_vec_pretty(&fixture()).unwrap();
    let manifest = Manifest::decode(&bytes).unwrap();
    assert_eq!(manifest.as_json(), bytes);
    assert_eq!(
        manifest.sha256(),
        Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    let root = Fixture::new();
    let path = root.0.join("release.json");
    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(Manifest::read(&path).unwrap().as_json(), bytes);
}

#[test]
fn bounds_encoding_and_raw_value_depth_are_explicit() {
    let valid = fixture().to_string();
    let mut exact = valid.clone().into_bytes();
    exact.resize(MAX_BYTES, b' ');
    assert!(Manifest::decode(&exact).is_ok());
    exact.push(b' ');
    assert_eq!(Manifest::decode(&exact).unwrap_err(), Error::Input);
    assert_eq!(Manifest::decode(b"\xff").unwrap_err(), Error::Json);
    assert!(json::decode(format!("{}0{}", "[".repeat(128), "]".repeat(128)).as_bytes()).is_ok());
    assert!(json::decode(format!("{}{}", "[".repeat(129), "]".repeat(129)).as_bytes()).is_err());
    for depth in [129, 2000] {
        let source = format!("{}0{}", "[".repeat(depth), "]".repeat(depth));
        assert_eq!(
            Manifest::decode(source.as_bytes()).unwrap_err(),
            Error::Json
        );
    }
    let source = format!("[{}]", vec!["0"; 100_001].join(","));
    assert_eq!(
        Manifest::decode(source.as_bytes()).unwrap_err(),
        Error::Json
    );
    let mut input = fixture();
    input["migrations"] = json!(
        (1..=1000)
            .map(|v| (v.to_string(), "c".repeat(96)))
            .collect::<BTreeMap<_, _>>()
    );
    assert!(Manifest::decode(input.to_string().as_bytes()).is_ok());
    input["migrations"]["1001"] = json!("c".repeat(96));
    assert_eq!(
        Manifest::decode(input.to_string().as_bytes()).unwrap_err(),
        Error::Migrations
    );
}

#[test]
fn byte_encoding_detection_matches_python_and_keeps_original_bytes() {
    let source = fixture().to_string();
    let mut cases = vec![source.as_bytes().to_vec()];
    let mut bom = vec![0xef, 0xbb, 0xbf];
    bom.extend_from_slice(source.as_bytes());
    cases.push(bom);
    for little in [false, true] {
        for width in [2, 4] {
            let mut bytes = Vec::new();
            if width == 2 {
                for unit in source.encode_utf16() {
                    bytes.extend_from_slice(&if little {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    });
                }
            } else {
                for unit in source.chars().map(u32::from) {
                    bytes.extend_from_slice(&if little {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    });
                }
            }
            cases.push(bytes.clone());
            let mut bom = match (width, little) {
                (2, true) => vec![0xff, 0xfe],
                (2, false) => vec![0xfe, 0xff],
                (4, true) => vec![0xff, 0xfe, 0, 0],
                _ => vec![0, 0, 0xfe, 0xff],
            };
            bom.extend_from_slice(&bytes);
            cases.push(bom);
        }
    }
    let unicode = source.replace("2026-10-21T01:02:03Z", "𝟐𝟎𝟐𝟔-10-21T01:02:03Z");
    let mut paired = vec![0xff, 0xfe];
    for unit in unicode.encode_utf16() {
        paired.extend_from_slice(&unit.to_le_bytes());
    }
    cases.push(paired);
    oracle_bytes(&cases);
    for bytes in &cases {
        let manifest = Manifest::decode(bytes).unwrap();
        assert_eq!(manifest.as_json(), bytes);
        assert_eq!(
            manifest.sha256(),
            Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
    }
    // Truncation, invalid scalar/codepoint and lone-surrogate buffers are refused
    // without ever exposing their bytes through public error text.
    let malformed = vec![
        vec![0xff, 0xfe, 0x7b],
        vec![0, 0, 0xfe, 0xff, 0, 0, 0x7b],
        vec![0xff, 0xfe, 0, 0xd8],
        vec![0, 0, 0xfe, 0xff, 0, 0x11, 0, 0],
        vec![0xef, 0xbb, 0xbf],
    ];
    oracle_bytes(&malformed);
    for bytes in malformed {
        assert_eq!(Manifest::decode(&bytes).unwrap_err(), Error::Json);
    }
    let unpaired = source.replace("2026-10-21T01:02:03Z", r"\ud800");
    assert_eq!(
        Manifest::decode(unpaired.as_bytes()).unwrap_err(),
        Error::Json
    );
}
