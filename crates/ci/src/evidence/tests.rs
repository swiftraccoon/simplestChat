//! Real workflow coverage, with the answers the retired Python producer gave
//! for the same fixtures recorded as explicit expectations.

use super::*;
use sha2::Digest;
use std::fs;
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    context: Context,
    path: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .to_owned();
        let path = std::env::temp_dir().join(format!(
            "simplestchat-evidence-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        for name in [".github/workflows", "security", "native"] {
            fs::create_dir_all(path.join(name)).unwrap();
        }
        for name in workflows::NAMES {
            let name = format!(".github/workflows/{name}");
            fs::copy(source.join(&name), path.join(&name)).unwrap();
        }
        let toolchain = "security/codeql-toolchain.json";
        fs::copy(source.join(toolchain), path.join(toolchain)).unwrap();
        let event = path.join("event.json");
        fs::write(&event, json!({"before": "b".repeat(40)}).to_string()).unwrap();
        let context = Context {
            root: path.clone(),
            environment: BTreeMap::from([
                ("ACT".into(), "true".into()),
                ("LOCAL_CI_DISPOSABLE".into(), "1".into()),
                ("LOCAL_CI_RUN_ID".into(), "1".repeat(32)),
                ("GITHUB_SHA".into(), "a".repeat(40)),
                ("GITHUB_EVENT_PATH".into(), event.to_str().unwrap().into()),
                (
                    "LOCAL_CI_EVIDENCE".into(),
                    path.join("native").to_str().unwrap().into(),
                ),
            ]),
        };
        Self { context, path }
    }

    fn populate(&mut self) {
        let inventory = workflows::Inventory::read(&self.path).unwrap();
        self.context.environment.insert(
            "GATE_RESULTS".into(),
            Value::Object(
                inventory
                    .gates
                    .iter()
                    .map(|name| (name.clone(), json!({"result":"success"})))
                    .collect(),
            )
            .to_string(),
        );
        let identity = self.context.identity().unwrap();
        let directory = self.path.join("native/receipts");
        fs::create_dir(&directory).unwrap();
        for identifier in &inventory.checks {
            fs::write(
                directory.join(format!("{identifier}.json")),
                serde_json::to_vec(&identity.matrix_receipt(identifier)).unwrap(),
            )
            .unwrap();
        }
    }

    fn change(&self, name: &str, before: &str, after: &str) {
        let path = self.path.join(name);
        let source = fs::read_to_string(&path).unwrap();
        assert!(
            source.contains(before),
            "fixture source no longer contains {before:?}"
        );
        fs::write(path, source.replacen(before, after, 1)).unwrap();
    }

    fn assert_failure(&self, operation: &str, identifier: &str, code: &str) {
        let result = self.context.execute(operation, identifier);
        assert!(result.is_err(), "native accepted {code}");
        assert!(result.unwrap_err().to_string().contains(code));
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn real_workflows_complete_with_byte_identical_receipts_and_exclusive_publication() {
    let mut fixture = Fixture::new();
    fixture.populate();
    fixture.context.complete().unwrap();
    let bytes = fs::read(fixture.path.join("native/required.json")).unwrap();
    let report = files::document(&fixture.path.join("native/required.json")).unwrap();
    // The Python producer wrote sorted keys with a two-space indent and one
    // final newline, and its bytes were identical for this fixture.
    let expected = format!("{}\n", serde_json::to_string_pretty(&report).unwrap());
    assert_eq!(String::from_utf8(bytes).unwrap(), expected);
    assert_eq!(report["schema"], 1);
    assert_eq!(report["status"], "passed");
    assert_eq!(report["runId"], "1".repeat(32));
    assert_eq!(report["revision"], "a".repeat(40));
    assert_eq!(report["base"], "b".repeat(40));
    for (name, digest) in report["workflows"].as_object().unwrap() {
        let source = fs::read(fixture.path.join(".github/workflows").join(name)).unwrap();
        let expected = sha2::Sha256::digest(source)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(digest, expected.as_str(), "{name}");
    }
    // The receipt names every first-party coverage contract, every gate the
    // workflow's final job needs, and the three workflows it read.
    assert_eq!(
        report["checks"],
        json!([
            "browser-accounts",
            "browser-media",
            "browser-stress",
            "codeql-actions",
            "codeql-javascript-typescript",
            "codeql-python",
            "codeql-rust",
            "security-fast",
        ])
    );
    let ci = files::bounded(&fixture.path.join(".github/workflows/ci.yml")).unwrap();
    let mut gates = workflows::jobs(&ci).unwrap()["required"]["needs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    gates.sort_unstable();
    assert_eq!(report["gates"], json!(gates));
    assert_eq!(
        report["workflows"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>(),
        ["ci.yml", "codeql.yml", "security.yml"].into()
    );
    assert!(fixture.context.complete().is_err());
}

#[test]
fn successful_records_bind_identity_match_bytes_and_cannot_be_replaced() {
    let fixture = Fixture::new();
    fixture.context.record("browser-media").unwrap();
    let path = fixture.path.join("native/receipts/browser-media.json");
    // These are the bytes the Python producer wrote for the same identity.
    let expected = format!(
        concat!(
            "{{\n  \"base\": \"{}\",\n  \"check\": \"browser-media\",\n",
            "  \"revision\": \"{}\",\n  \"runId\": \"{}\",\n",
            "  \"schema\": 1,\n  \"status\": \"passed\"\n}}\n"
        ),
        "b".repeat(40),
        "a".repeat(40),
        "1".repeat(32)
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), expected);
    // The receipt carries exactly the caller's identity.
    assert_eq!(
        files::document(&path).unwrap(),
        json!({
            "schema": 1,
            "status": "passed",
            "check": "browser-media",
            "runId": "1".repeat(32),
            "revision": "a".repeat(40),
            "base": "b".repeat(40),
        })
    );
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(fixture.context.record("browser-media").is_err());
}

#[test]
fn missing_extra_failed_skipped_and_cancelled_jobs_never_complete() {
    for status in ["failure", "skipped", "cancelled"] {
        let mut fixture = Fixture::new();
        fixture.populate();
        let mut needs: Value =
            serde_json::from_str(&fixture.context.environment["GATE_RESULTS"]).unwrap();
        needs["deployment"]["result"] = Value::from(status);
        fixture
            .context
            .environment
            .insert("GATE_RESULTS".into(), needs.to_string());
        fixture.assert_failure("complete", "", "local_ci_gate_failure");
    }
    for extra in [false, true] {
        let mut fixture = Fixture::new();
        fixture.populate();
        let mut needs: Value =
            serde_json::from_str(&fixture.context.environment["GATE_RESULTS"]).unwrap();
        if extra {
            needs["invented"] = json!({"result":"success"});
        } else {
            needs.as_object_mut().unwrap().remove("deployment");
        }
        fixture
            .context
            .environment
            .insert("GATE_RESULTS".into(), needs.to_string());
        fixture.assert_failure("complete", "", "local_ci_gate_inventory");
    }
}

#[test]
fn every_matrix_family_is_required_and_unknown_receipts_are_rejected() {
    for missing in ["browser-media", "codeql-rust", "security-fast", "unknown"] {
        let mut fixture = Fixture::new();
        fixture.populate();
        let path = fixture
            .path
            .join("native/receipts")
            .join(format!("{missing}.json"));
        if missing == "unknown" {
            fs::write(path, "{}").unwrap();
        } else {
            fs::remove_file(path).unwrap();
        }
        fixture.assert_failure("complete", "", "local_ci_incomplete_matrix_receipts");
        assert!(!fixture.path.join("native/required.json").exists());
    }
    // Linux accepts non-UTF-8 filenames; macOS refuses creating this fixture.
    #[cfg(target_os = "linux")]
    {
        let mut fixture = Fixture::new();
        fixture.populate();
        let filename = std::ffi::OsString::from_vec(b"unknown\xff.json".to_vec());
        fs::write(fixture.path.join("native/receipts").join(filename), "{}").unwrap();
        fixture.assert_failure("complete", "", "local_ci_incomplete_matrix_receipts");
    }
}

#[test]
fn stale_or_unknown_receipt_fields_and_identity_never_complete() {
    for field in ["runId", "revision", "base", "status", "extra"] {
        let mut fixture = Fixture::new();
        fixture.populate();
        let path = fixture.path.join("native/receipts/browser-media.json");
        let mut value = files::document(&path).unwrap();
        value[field] = Value::from("stale");
        fs::write(path, value.to_string()).unwrap();
        fixture.assert_failure("complete", "", "local_ci_receipt_identity");
    }
}

#[test]
fn launcher_identity_and_record_identifiers_use_exact_original_validation() {
    for (field, value, code) in [
        ("ACT", "false", "local_ci_runner_only"),
        ("LOCAL_CI_DISPOSABLE", "0", "local_ci_runner_only"),
        ("LOCAL_CI_RUN_ID", "A123", "local_ci_run_id"),
        ("GITHUB_SHA", "main", "local_ci_revision"),
    ] {
        let mut fixture = Fixture::new();
        fixture
            .context
            .environment
            .insert(field.into(), value.into());
        fixture.assert_failure("record", "browser-media", code);
    }
    for identifier in [
        "",
        "../escape",
        "Upper",
        "with_underscore",
        "two\nlines",
        &"a".repeat(81),
    ] {
        Fixture::new().assert_failure("record", identifier, "local_ci_check_id");
    }
}

#[test]
fn bounded_regular_reads_reject_symlinks_empty_and_oversized_documents() {
    let fixture = Fixture::new();
    let original = fixture.path.join("event.json");
    let moved = fixture.path.join("event-original.json");
    fs::rename(&original, &moved).unwrap();
    symlink(&moved, &original).unwrap();
    assert!(fixture.context.record("browser-media").is_err());
    fs::remove_file(&original).unwrap();
    fs::write(&original, "").unwrap();
    fixture.assert_failure("record", "browser-media", "tool_file_size");
    fs::write(original, vec![b' '; files::MAX_DOCUMENT as usize + 1]).unwrap();
    fixture.assert_failure("record", "browser-media", "tool_file_size");
}

#[test]
fn json_ascii_escaping_and_python_numeric_schema_equality_are_explicit() {
    assert_eq!(
        String::from_utf8(files::json_bytes(&json!({"a": "é 🦊\u{007f}", "b": []})).unwrap())
            .unwrap(),
        "{\n  \"a\": \"\\u00e9 \\ud83e\\udd8a\\u007f\",\n  \"b\": []\n}\n"
    );
    let identity = Identity {
        run_id: "1".repeat(32),
        revision: "a".repeat(40),
        base: "b".repeat(40),
    };
    for schema in [json!(1), json!(1.0), json!(true)] {
        let mut receipt = identity.matrix_receipt("browser-media");
        receipt["schema"] = schema;
        assert!(identity.matches_matrix(&receipt, "browser-media"));
    }
    let mut receipt = identity.matrix_receipt("browser-media");
    receipt["schema"] = json!(false);
    assert!(!identity.matches_matrix(&receipt, "browser-media"));
}

#[test]
fn actual_workflow_matrices_keep_numeric_parallelism_and_all_declared_entries() {
    let fixture = Fixture::new();
    for (name, job, dimension, limit) in [
        ("ci.yml", "browser", "group", 3),
        ("codeql.yml", "source-analysis", "language", 4),
    ] {
        let bytes = files::bounded(&fixture.path.join(".github/workflows").join(name)).unwrap();
        let jobs = workflows::jobs(&bytes).unwrap();
        let strategy = &jobs[job]["strategy"];
        assert_eq!(strategy["max-parallel"], json!(limit));
        assert_eq!(
            strategy["matrix"][dimension].as_array().unwrap().len(),
            limit
        );
    }
}

#[test]
fn filtered_duplicate_or_expanded_matrices_and_vendor_scans_fail_before_evidence() {
    for (file, before, after, code) in [
        (
            "ci.yml",
            "group: [accounts, media, stress]",
            "group: [accounts, media]",
            "local_ci_browser_matrix_ports",
        ),
        (
            "ci.yml",
            "group: [accounts, media, stress]",
            "group: [accounts, media, stress]\n        extra: [unknown]",
            "local_ci_browser_matrix_shape",
        ),
        (
            "codeql.yml",
            "language: [actions, javascript-typescript, python, rust]",
            "language: [actions, javascript-typescript, python]",
            "local_ci_codeql_coverage",
        ),
        (
            "codeql.yml",
            "language: [actions, javascript-typescript, python, rust]",
            "language: [actions, javascript-typescript, python, rust, rust]",
            "local_ci_codeql_coverage",
        ),
        (
            "codeql.yml",
            "language: [actions, javascript-typescript, python, rust]",
            "language: [actions, javascript-typescript, python, rust]\n        include: []",
            "local_ci_codeql_matrix_shape",
        ),
        (
            "security.yml",
            "jobs:",
            "jobs:\n  native-security: {}",
            "local_ci_vendor_scan_is_optional",
        ),
        (
            "codeql.yml",
            "jobs:",
            "jobs:\n  native-analysis: {}",
            "local_ci_vendor_scan_is_optional",
        ),
    ] {
        let mut fixture = Fixture::new();
        fixture.populate();
        fixture.change(&format!(".github/workflows/{file}"), before, after);
        fixture.assert_failure("complete", "", code);
        assert!(!fixture.path.join("native/required.json").exists());
    }
}

#[test]
fn yaml_aliases_merges_duplicate_keys_and_legacy_booleans_match_safe_loader_semantics() {
    let jobs = workflows::jobs(
        br"
jobs:
  defaults: &defaults
    strategy: {matrix: {group: &groups [accounts, media]}}
  browser:
    <<: *defaults
    enabled: yes
  required:
    needs: [stale]
    needs: *groups
",
    )
    .unwrap();
    assert_eq!(
        jobs["browser"]["strategy"]["matrix"]["group"],
        json!(["accounts", "media"])
    );
    assert_eq!(jobs["browser"]["enabled"], true);
    assert_eq!(jobs["required"]["needs"], json!(["accounts", "media"]));
}

#[test]
fn yaml_depth_alias_replay_documents_and_tags_have_explicit_bounds() {
    let deep = format!("jobs: {{browser: {}0{}}}", "[".repeat(70), "]".repeat(70));
    assert!(workflows::jobs(deep.as_bytes()).is_err());
    assert!(workflows::jobs(b"jobs: {}\n---\njobs: {}\n").is_err());
    assert!(workflows::jobs(b"jobs: !include /etc/passwd\n").is_err());
    assert!(workflows::jobs(b"jobs: {browser: &loop [*loop]}\n").is_err());
    let aliases = format!(
        "jobs:\n  base: &base [a]\n  many: [{}]\n",
        vec!["*base"; 1_025].join(", ")
    );
    assert!(workflows::jobs(aliases.as_bytes()).is_err());
}
