//! Ordinary deploy admission after independently retaining passive native captures.
//! Filesystem custody, process status, dump listing and effect observations stay with the caller.

use super::*;

const BASE_FIELDS: &[&str] = &[
    "action",
    "revision",
    "startedAt",
    "passed",
    "phase",
    "finishedAt",
    "evidence",
    "backupMigrations",
    "browserAssets",
    "quietWaitRequestedSeconds",
    "quietWaitSeconds",
    "roomsActiveAtQuietCheck",
    "backupSha256",
    "roomsActiveBeforeStop",
    "roomsObservedBeforeStopAt",
    "interruptionStartedAt",
];
fn zero(value: &Value) -> bool {
    // Preserve the frozen chained Python numeric equality to zero.
    value.as_f64() == Some(0.0) || value == false
}
fn one(value: &Value) -> bool {
    value.as_f64() == Some(1.0) || value == true
}
fn timestamp(value: &Value) -> bool {
    let Some(value) = value.as_str() else {
        return false;
    };
    let letters = value.chars().collect::<Vec<_>>();
    if letters.len() != 20 {
        return false;
    }
    for (index, letter) in letters.iter().enumerate() {
        let punctuation = match index {
            4 | 7 => Some('-'),
            10 => Some('T'),
            13 | 16 => Some(':'),
            19 => Some('Z'),
            _ => None,
        };
        if let Some(expected) = punctuation {
            if *letter != expected {
                return false;
            }
        } else if crate::release::manifest::decimal(*letter).is_none() {
            return false;
        }
    }
    true
}
fn native_name(value: &str, kind: &str) -> bool {
    value
        .strip_prefix(&format!("scpub-release-{kind}-"))
        .is_some_and(|suffix| hex(suffix, 32))
}

pub(super) fn native_report(value: &Value, target: &str, negative: bool) -> Result<PathBuf> {
    let mut names = BASE_FIELDS.to_vec();
    names.extend(if negative {
        &["rollbackAttempted", "rollbackPassed", "failure"][..]
    } else {
        &["interruptionFinishedAt"][..]
    });
    super::super::protocol::exact(value, &names)?;
    require(
        value["action"] == "deploy"
            && value["revision"] == target
            && value["passed"] == !negative
            && value["phase"]
                == if negative {
                    "replace_application"
                } else {
                    "complete"
                }
            && zero(&value["quietWaitRequestedSeconds"])
            && zero(&value["quietWaitSeconds"]),
    )?;
    if negative {
        require(
            value["rollbackAttempted"] == true
                && value["rollbackPassed"] == true
                && value["failure"] == "Application readiness deadline exceeded",
        )?;
    }
    for field in [
        "startedAt",
        "finishedAt",
        "interruptionStartedAt",
        "roomsObservedBeforeStopAt",
    ] {
        require(timestamp(&value[field]))?;
    }
    require(
        value["backupSha256"]
            .as_str()
            .is_some_and(|value| hex(value, 64)),
    )?;
    let evidence = value["evidence"].as_str().ok_or(Error::Input)?;
    // Match the frozen POSIX Path parent/name comparison, including its
    // normalization of redundant separators and dots. Exactly two leading
    // slashes retain a separate root in Python's PurePosixPath.
    require(!evidence.starts_with("//") || evidence.starts_with("///"))?;
    let attempt = PathBuf::from(evidence);
    require(
        attempt.parent() == Some(Path::new(PUBLIC).join("results").as_path())
            && attempt
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_prefix("release."))
                .is_some_and(|suffix| hex(suffix, 32)),
    )?;
    Ok(attempt)
}
pub(super) fn outcome(report: &Value, observed: &Value) -> Result<()> {
    let mut expected = report.as_object().ok_or(Error::Input)?.clone();
    require(expected.remove("evidence").is_some())?;
    require(observed.as_object() == Some(&expected))
}
fn python_lines(value: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut start = 0;
    let mut letters = value.char_indices().peekable();
    while let Some((index, letter)) = letters.next() {
        if matches!(
            letter,
            '\n' | '\r' | '\x0b' | '\x0c' | '\x1c'..='\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}'
        ) {
            result.push(&value[start..index]);
            start = index + letter.len_utf8();
            if letter == '\r' && letters.peek().is_some_and(|(_, next)| *next == '\n') {
                let (index, letter) = letters.next().expect("observed CRLF tail");
                start = index + letter.len_utf8();
            }
        }
    }
    if start < value.len() {
        result.push(&value[start..]);
    }
    result
}
pub(in super::super) fn ledger(bytes: &[u8]) -> Result<BTreeMap<String, String>> {
    require(bytes.len() <= 2 * 1024 * 1024)?;
    let source = std::str::from_utf8(bytes).map_err(|_| Error::Input)?;
    let source = crate::release::attestation::stage::python_strip(source);
    let mut result = BTreeMap::new();
    for row in python_lines(source) {
        let fields = row.split('|').collect::<Vec<_>>();
        require(fields.len() == 3)?;
        let version = fields[0];
        require(
            version
                .as_bytes()
                .first()
                .is_some_and(|byte| (b'1'..=b'9').contains(byte))
                && version.bytes().all(|byte| byte.is_ascii_digit())
                && fields[1] == "t"
                && hex(fields[2], 96),
        )?;
        require(result.insert(version.into(), fields[2].into()).is_none())?;
    }
    require(!result.is_empty())?;
    Ok(result)
}
pub(super) fn backup(
    receipt: &Value,
    bytes: u64,
    sha: &str,
    header: &[u8],
    reported_sha: &str,
    target: &str,
) -> Result<()> {
    super::super::protocol::exact(
        receipt,
        &[
            "schemaVersion",
            "dump",
            "bytes",
            "sha256",
            "revision",
            "completedAt",
        ],
    )?;
    require(
        one(&receipt["schemaVersion"])
            && receipt["dump"] == "database-before.dump"
            && (1..=4 * 1024 * 1024 * 1024).contains(&bytes)
            && receipt["bytes"].as_u64() == Some(bytes)
            && hex(sha, 64)
            && receipt["sha256"] == sha
            && sha == reported_sha
            && receipt["revision"] == target
            && receipt["completedAt"].is_string()
            && header.starts_with(b"PGDMP"),
    )
}
pub(super) fn name_anchors(rows: &[Value], validator: &str, assets: &str) -> Result<()> {
    require(native_name(validator, "validate") && native_name(assets, "assets"))?;
    let mut last_validator = None;
    let mut last_assets = None;
    for row in rows {
        if row["Type"] != "container" || row["Action"] != "create" {
            continue;
        }
        let Some(name) = row["Actor"]["Attributes"]["name"].as_str() else {
            continue;
        };
        if name.starts_with("scpub-release-validate-") {
            require(native_name(name, "validate"))?;
            last_validator = Some(name);
        } else if name.starts_with("scpub-release-assets-") {
            require(native_name(name, "assets"))?;
            last_assets = Some(name);
        }
    }
    require(last_validator == Some(validator) && last_assets == Some(assets))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const TIME: &str = "2026-10-29T00:00:00Z";
    fn report(negative: bool) -> Value {
        let mut value = json!({"action":"deploy", "revision":"a".repeat(40),
            "startedAt":TIME, "passed":!negative,
            "phase":if negative {"replace_application"} else {"complete"}, "finishedAt":TIME,
            "evidence":format!("{PUBLIC}/results/release.{}", "b".repeat(32)),
            "backupMigrations":{}, "browserAssets":{}, "quietWaitRequestedSeconds":0,
            "quietWaitSeconds":0, "roomsActiveAtQuietCheck":0, "backupSha256":"c".repeat(64),
            "roomsActiveBeforeStop":0, "roomsObservedBeforeStopAt":TIME,
            "interruptionStartedAt":TIME});
        if negative {
            value["rollbackAttempted"] = true.into();
            value["rollbackPassed"] = true.into();
            value["failure"] = "Application readiness deadline exceeded".into();
        } else {
            value["interruptionFinishedAt"] = json!(TIME);
        }
        value
    }
    #[test]
    fn ordinary_reports_keep_all_seventeen_positive_and_nineteen_negative_fields() {
        for negative in [false, true] {
            let original = report(negative);
            assert_eq!(
                original.as_object().unwrap().len(),
                if negative { 19 } else { 17 }
            );
            assert!(native_report(&original, &"a".repeat(40), negative).is_ok());
            for field in original.as_object().unwrap().keys() {
                let mut missing = original.clone();
                missing.as_object_mut().unwrap().remove(field);
                assert!(
                    native_report(&missing, &"a".repeat(40), negative).is_err(),
                    "{field}"
                );
            }
            for (field, hostile) in [
                ("action", json!("other")),
                ("revision", json!("d".repeat(40))),
                ("passed", json!(negative)),
                ("phase", json!("other")),
                ("quietWaitSeconds", json!(1)),
                ("quietWaitRequestedSeconds", json!(1)),
                ("backupSha256", json!("private")),
                ("startedAt", json!("bad")),
                ("finishedAt", json!("bad")),
                ("interruptionStartedAt", json!("bad")),
                ("roomsObservedBeforeStopAt", json!("bad")),
                (
                    "evidence",
                    json!(format!("{PUBLIC}/results/release.{}x", "b".repeat(32))),
                ),
                ("rollbackFailure", json!({"stage":"stop"})),
            ] {
                let mut changed = original.clone();
                changed[field] = hostile;
                assert!(
                    native_report(&changed, &"a".repeat(40), negative).is_err(),
                    "{field}"
                );
            }
        }
        for (field, hostile) in [
            ("rollbackAttempted", json!(false)),
            ("rollbackPassed", json!(false)),
            ("failure", json!("another cause")),
        ] {
            let mut value = report(true);
            value[field] = hostile;
            assert!(native_report(&value, &"a".repeat(40), true).is_err());
        }
        // Frozen timestamps are Unicode-digit shapes, not calendar/time authority.
        assert!(timestamp(&json!("２０２６-９９-９９T９９:９９:９９Z")));
        let mut value = report(false);
        value["quietWaitSeconds"] = json!(false);
        value["quietWaitRequestedSeconds"] = json!(0.0);
        assert!(native_report(&value, &"a".repeat(40), false).is_ok());
        value["evidence"] = json!(format!("{PUBLIC}/results/./release.{}/", "b".repeat(32)));
        assert!(native_report(&value, &"a".repeat(40), false).is_ok());
        value["evidence"] = json!(format!("/{PUBLIC}/results/release.{}", "b".repeat(32)));
        assert!(native_report(&value, &"a".repeat(40), false).is_err());
    }
    #[test]
    fn outcome_removes_only_evidence_and_never_normalizes_other_fields() {
        let report = report(true);
        let mut expected = report.clone();
        expected.as_object_mut().unwrap().remove("evidence");
        assert!(outcome(&report, &expected).is_ok());
        for field in ["failure", "passed", "backupSha256"] {
            let mut changed = expected.clone();
            changed[field] = json!("changed");
            assert!(outcome(&report, &changed).is_err());
        }
        assert!(outcome(&report, &report).is_err());
    }
    #[test]
    fn ledger_requires_real_successful_distinct_versions_and_full_sha384() {
        let hash = "a".repeat(96);
        let bytes = format!("1|t|{hash}\r\n2|t|{hash}\n");
        assert_eq!(ledger(bytes.as_bytes()).unwrap().len(), 2);
        for invalid in [
            String::new(),
            format!("0|t|{hash}"),
            format!("01|t|{hash}"),
            format!("1|f|{hash}"),
            format!("1|t|{hash}\n1|t|{hash}"),
            "1|t|short".into(),
            format!("1|t|{}", "A".repeat(96)),
            format!("1|t|{hash}|extra"),
        ] {
            assert!(ledger(invalid.as_bytes()).is_err());
        }
        assert!(ledger(&[0xff]).is_err());
        assert_eq!(
            ledger(format!("1|t|{hash}\u{2028}2|t|{hash}").as_bytes())
                .unwrap()
                .len(),
            2
        );
    }
    #[test]
    fn retained_backup_receipt_joins_actual_dump_header_size_hash_and_revision() {
        let sha = "c".repeat(64);
        let target = "a".repeat(40);
        let original = json!({"schemaVersion":1, "dump":"database-before.dump", "bytes":5,
            "sha256":sha, "revision":target, "completedAt":"observed string"});
        assert!(backup(&original, 5, &sha, b"PGDMP", &sha, &target).is_ok());
        for (field, hostile) in [
            ("dump", json!("other")),
            ("bytes", json!(true)),
            ("sha256", json!("d".repeat(64))),
            ("revision", json!("e".repeat(40))),
            ("completedAt", Value::Null),
            ("private", json!(true)),
        ] {
            let mut value = original.clone();
            value[field] = hostile;
            assert!(backup(&value, 5, &sha, b"PGDMP", &sha, &target).is_err());
        }
        assert!(backup(&original, 5, &sha, b"plain SQL", &sha, &target).is_err());
        assert!(backup(&original, 0, &sha, b"PGDMP", &sha, &target).is_err());
        assert!(backup(&original, 5, &sha, b"PGDMP", &"d".repeat(64), &target).is_err());
    }
    #[test]
    fn retained_actor_names_match_last_actual_creates_for_each_role() {
        let name =
            |kind, byte: char| format!("scpub-release-{kind}-{}", byte.to_string().repeat(32));
        let validator = name("validate", '2');
        let assets = name("assets", '4');
        let rows = [
            ("validate", '1'),
            ("validate", '2'),
            ("assets", '3'),
            ("assets", '4'),
        ]
        .map(|(kind, byte)| {
            json!({"Type":"container", "Action":"create", "Actor":{
                "Attributes":{"name":name(kind, byte)}}})
        });
        assert!(name_anchors(&rows, &validator, &assets).is_ok());
        assert!(name_anchors(&rows, &name("validate", '1'), &assets).is_err());
        assert!(name_anchors(&rows, &validator, &name("assets", '3')).is_err());
        assert!(name_anchors(&rows[..3], &validator, &assets).is_err());
        assert!(name_anchors(&rows, "private/path", &assets).is_err());
    }
}
