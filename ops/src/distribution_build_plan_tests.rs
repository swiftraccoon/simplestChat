use super::*;
use crate::test_support::Fixture;
use serde_json::json;

#[test]
fn plan_separates_declared_claims_measured_inventory_and_unresolved_recipe_slots() {
    let root = Fixture::new();
    fixture::populate(&root.0);
    let selection =
        Selection::new("owner/repo", &"a".repeat(40), "0.1.0", 1, TARGET, 123, 2).unwrap();
    let inventory = collect_inputs(&root.0).unwrap();
    let value = serde_json::to_value(plan(&selection, &inventory)).unwrap();
    assert_eq!(value["kind"], "ops-build-plan");
    assert_eq!(value["claims"]["runAttempt"], 2);
    assert_eq!(value["inventory"]["sha256"], inventory.sha256());
    assert_eq!(value["recipe"]["program"], "cargoExecutable");
    assert_eq!(value["recipe"]["workingDirectory"], "stagingSourceRoot");
    assert_eq!(value["recipe"]["environmentPolicy"], "clearThenSet");
    assert_eq!(
        value["recipe"]["arguments"],
        json!([
            {"kind":"literal","value":"build"},
            {"kind":"literal","value":"--frozen"},
            {"kind":"literal","value":"--release"},
            {"kind":"literal","value":"-p"},
            {"kind":"literal","value":"simplestchat-ops"},
            {"kind":"literal","value":"--bin"},
            {"kind":"literal","value":"simplestchat-ops"},
            {"kind":"literal","value":"--target"},
            {"kind":"literal","value":TARGET},
            {"kind":"literal","value":"--no-default-features"},
            {"kind":"literal","value":"--features"},
            {"kind":"literal","value":"simplestchat-ops/maintain-guest"},
            {"kind":"literal","value":"--target-dir"},
            {"kind":"slot","value":"targetDirectory"},
        ])
    );
    let flags = value["recipe"]["environment"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == "CARGO_ENCODED_RUSTFLAGS")
        .unwrap();
    assert_eq!(
        flags["value"]["value"]
            .as_str()
            .unwrap()
            .split('\u{1f}')
            .collect::<Vec<_>>(),
        [
            "-C",
            "target-feature=+crt-static",
            "-C",
            "target-cpu=x86-64",
            "-C",
            "link-self-contained=no",
            "-C",
            "linker-flavor=gcc"
        ]
    );
    let public = value.to_string();
    for private in [
        "PRIVATE-INPUT",
        root.0.to_str().unwrap(),
        "passed",
        "binarySha256",
    ] {
        assert!(!public.contains(private));
    }
    assert_eq!(value["prerequisites"].as_array().unwrap().len(), 10);
    assert_eq!(value["recipe"]["defaultFeatures"], false);
    assert_eq!(value["recipe"]["features"], json!(["maintain-guest"]));
}
