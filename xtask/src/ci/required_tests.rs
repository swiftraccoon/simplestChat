//! Exercise the actual typed policy with explicit JSON, never global env changes.

use super::*;
use serde_json::{Value, json};
use std::path::Path;

const NAMES: [&str; 10] = [
    "automation",
    "rust-lint",
    "rust-test",
    "native-dtls",
    "browser",
    "web",
    "image-database",
    "deployment",
    "security",
    "codeql",
];

fn results() -> Value {
    NAMES
        .iter()
        .map(|name| ((*name).to_owned(), json!({"result":"success"})))
        .collect::<serde_json::Map<_, _>>()
        .into()
}

#[test]
fn only_every_canonical_job_succeeding_establishes_readiness() {
    assert!(successful(Some(&results().to_string())));
    for name in NAMES {
        for conclusion in ["failure", "cancelled", "skipped", "pending", "", "SUCCESS"] {
            let mut input = results();
            input[name]["result"] = conclusion.into();
            assert!(
                !successful(Some(&input.to_string())),
                "{name}: {conclusion}"
            );
        }
        let mut input = results();
        input.as_object_mut().unwrap().remove(name);
        assert!(!successful(Some(&input.to_string())), "missing {name}");
    }
    let mut input = results();
    input["extra"] = json!({"result":"success"});
    assert!(!successful(Some(&input.to_string())));
}

#[test]
fn missing_malformed_and_duplicate_results_fail_closed() {
    assert!(!successful(None));
    for input in [
        "",
        "null",
        "[]",
        "true",
        "1",
        "{",
        "{}",
        "{\"PRIVATE\":true}",
    ] {
        assert!(!successful(Some(input)));
    }
    for value in [Value::Null, json!(true), json!(1), json!([]), json!({})] {
        let mut input = results();
        input["security"] = value.clone();
        assert!(!successful(Some(&input.to_string())));
        input["security"] = json!({"result":value});
        assert!(!successful(Some(&input.to_string())));
    }
    let input = results().to_string();
    for duplicate in [
        input.replacen(
            "\"automation\":{\"result\":\"success\"}",
            "\"automation\":{\"result\":\"failure\"},\"automation\":{\"result\":\"success\"}",
            1,
        ),
        input.replacen(
            "\"result\":\"success\"",
            "\"result\":\"failure\",\"result\":\"success\"",
            1,
        ),
    ] {
        assert!(!successful(Some(&duplicate)));
    }
}

#[test]
fn metadata_cannot_change_conclusions_and_the_input_bound_is_exact() {
    let mut input = results();
    input["codeql"]["outputs"] = json!({"result":"failure", "private":"PRIVATE"});
    input["browser"]["arbitrary"] = json!([null, {"result":"skipped"}]);
    assert!(successful(Some(&input.to_string())));
    input["codeql"]["result"] = "failure".into();
    input["codeql"]["outputs"]["result"] = "success".into();
    assert!(!successful(Some(&input.to_string())));

    let mut input = results().to_string();
    input.extend(std::iter::repeat_n(' ', MAX_RESULTS - input.len()));
    assert!(successful(Some(&input)));
    input.push(' ');
    assert!(!successful(Some(&input)));
}

#[test]
fn canonical_names_match_the_actual_workflow_dependency_graph() {
    #[derive(Deserialize)]
    struct Workflow {
        jobs: BTreeMap<String, Value>,
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let source = std::fs::read(root.join(".github/workflows/ci.yml")).unwrap();
    let jobs = serde_saphyr::from_slice::<Workflow>(&source).unwrap().jobs;
    let needs = jobs["required"]["needs"].as_array().unwrap();
    let names: std::collections::BTreeSet<_> =
        needs.iter().map(|name| name.as_str().unwrap()).collect();
    assert_eq!(names, NAMES.into_iter().collect());
    assert_eq!(needs.len(), GATES.len());
    let observed: std::collections::BTreeSet<_> = jobs
        .keys()
        .map(String::as_str)
        // Main-push Ops production/signing has its own chain, checked by the
        // workflow policy tests; these ten correctness/security gates stay shared.
        .filter(|name| {
            ![
                "required",
                "release-security",
                "release-prepare",
                "ops-build",
                "ops-release-security",
            ]
            .contains(name)
        })
        .collect();
    assert_eq!(observed, names);
}
