//! Pure request and raw-catalogue boundary regressions.

use super::*;
use serde_json::{Value, json};

pub(super) fn wire() -> Value {
    json!({"schemaVersion":1,"selection":{"repository":"owner/repo",
        "revision":"c".repeat(40),"packageVersion":"0.1.0","protocolVersion":1,
        "target":"x86_64-unknown-linux-musl","buildRunId":1,"runAttempt":1},
        "repository":"/materials/repository","git":{"path":"/tools/git",
        "sha256":"a".repeat(64),"bytes":42},"output":"/output/fresh"})
}
fn decode(value: &Value) -> Result<Request> {
    let bytes = serde_json::to_vec(value).unwrap();
    Request::decode(&bytes, &sha(&bytes))
}
#[test]
fn request_original_bytes_strict_types_scope_and_private_debug() {
    let value = wire();
    let selected = decode(&value).unwrap();
    assert_eq!(selected.revision, "c".repeat(40));
    assert!(!format!("{selected:?}").contains("materials"));
    let pretty = serde_json::to_vec_pretty(&value).unwrap();
    assert!(Request::decode(&pretty, &selected.sha256).is_err());
    for (field, invalid) in [
        ("schemaVersion", json!(1.0)),
        ("selection", json!({})),
        ("PRIVATE_KEY", json!("PRIVATE_SECRET")),
        ("output", json!("/materials/repository/run")),
        ("output", json!("/materials")),
        ("output", json!("/tools")),
        ("repository", json!("/materials/../repo")),
        ("git", json!({})),
    ] {
        let mut value = wire();
        value[field] = invalid;
        assert!(decode(&value).is_err(), "{field}");
    }
    for invalid in [
        json!(true),
        json!(1.0),
        json!(0),
        json!(-1),
        json!(67108865u64),
    ] {
        let mut value = wire();
        value["git"]["bytes"] = invalid;
        assert!(decode(&value).is_err());
    }
    let original = serde_json::to_string(&wire()).unwrap();
    for invalid in [
        original.replacen('{', "{\"schemaVersion\":1,", 1),
        format!("{original}{}", " ".repeat(65536)),
    ] {
        assert!(Request::decode(invalid.as_bytes(), &sha(invalid.as_bytes())).is_err());
    }
}
fn row(mode: &str, kind: &str, size: &str, path: &str) -> Vec<u8> {
    format!("{mode} {kind} {} {size}\t{path}\0", "a".repeat(40)).into_bytes()
}
#[test]
fn catalogue_preserves_directories_empty_trees_modes_and_original_names() {
    let bytes = [
        row("040000", "tree", "-", "empty"),
        row("040000", "tree", "-", "nested"),
        row("100644", "blob", "0", "nested/space name"),
        row("100755", "blob", "1", "run"),
    ]
    .concat();
    let parsed = catalog::parse(&bytes).unwrap();
    assert!(parsed["empty"].directory);
    assert!(parsed["run"].executable);
    assert_eq!(parsed["nested/space name"].bytes, 0);
    assert!(catalog::parse(&[]).unwrap().is_empty());
}
#[test]
fn catalogue_refuses_links_submodules_unsafe_names_missing_parents_and_conflicts() {
    let mut cases = vec![
        row("120000", "blob", "1", "link"),
        row("160000", "commit", "-", "module"),
        row("100644", "blob", "1", "/absolute"),
        row("100644", "blob", "1", "../escape"),
        row("100644", "blob", "1", ".git/config"),
        row("100644", "blob", "1", "a//b"),
        row("100644", "blob", "1", "missing/child"),
        row("100644", "blob", "16777217", "big"),
        row("100644", "blob", "-1", "bad"),
    ];
    cases.push(
        [
            row("100644", "blob", "1", "same"),
            row("100644", "blob", "1", "same"),
        ]
        .concat(),
    );
    cases.push(
        [
            row("100644", "blob", "1", "a"),
            row("100644", "blob", "1", "a/b"),
        ]
        .concat(),
    );
    let mut truncated = row("100644", "blob", "1", "a");
    truncated.pop();
    cases.push(truncated);
    for value in cases {
        assert!(catalog::parse(&value).is_err());
    }
}
#[test]
fn catalogue_caps_actual_records_total_bytes_names_and_exact_ids() {
    let many = (0..=catalog::ENTRIES)
        .flat_map(|i| row("100644", "blob", "0", &format!("f{i}")))
        .collect::<Vec<_>>();
    assert!(catalog::parse(&many).is_err());
    let bytes = (0..17)
        .flat_map(|i| row("100644", "blob", "16777216", &format!("f{i}")))
        .collect::<Vec<_>>();
    assert!(catalog::parse(&bytes).is_err());
    assert!(catalog::parse(&vec![b' '; catalog::CATALOG as usize + 1]).is_err());
    for value in [
        "a".repeat(39),
        "A".repeat(40),
        format!("{}\n\n", "a".repeat(40)),
    ] {
        assert!(catalog::oid(value.as_bytes()).is_err());
    }
}
