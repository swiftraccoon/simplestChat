//! Hosted Ops production and its exact signer stay separate from the server
//! release, carried over from the Python workflow tests. Each pin inspects
//! the workflow's structure; no action, candidate or tool runs.

use super::{steps, workflow};
use serde_json::{Value, json};

const CHECKOUT: &str = "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1";
const RUST: &str = "dtolnay/rust-toolchain@89b12181fb390509a0842a86cc55eeb8eb928c1d";
const UPLOAD: &str = "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a";
const DOWNLOAD: &str = "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c";
const ATTEST: &str = "actions/attest@1e69f48acb82d1966a394da916b4c1698aa569d6";
const GUARD: &str = "${{ github.event_name == 'push' && github.ref == 'refs/heads/main' }}";
const TARGET: &str = "x86_64-unknown-linux-musl";

/// The fixed unsigned payload: the executable, its four records and the nine
/// evidence members.
fn fixed() -> Vec<String> {
    [
        "simplestchat-ops",
        "ops-release.json",
        "ops-evidence.json",
        "ops-predicate.json",
        "ops-preparation.json",
    ]
    .into_iter()
    .map(str::to_owned)
    .chain((0..9).map(|index| format!("ops-evidence-{index:02}.bin")))
    .collect()
}

/// The lines of an upload's `path`, which must be text.
fn paths(upload: &Value) -> Vec<&str> {
    upload["path"].as_str().unwrap().lines().collect()
}

/// From `test_build_has_no_signing_authority_or_cache_success_shortcut`: the
/// fixed owner runs with authenticated materials on each main push.
#[test]
fn build_has_no_signing_authority_or_cache_success_shortcut() {
    let ci = workflow("ci.yml");
    let job = ci["jobs"]["ops-build"].as_object().unwrap();
    assert_eq!(job["if"], GUARD);
    assert_eq!(job["runs-on"], "ubuntu-24.04");
    assert_eq!(job["timeout-minutes"], 65);
    assert_eq!(job["permissions"], json!({"contents": "read"}));
    assert!(!job.contains_key("needs"));
    let steps = steps(&ci, "ops-build");
    assert_eq!(steps.len(), 4);
    assert_eq!(steps[0]["uses"], CHECKOUT);
    assert_eq!(steps[0]["with"]["persist-credentials"], false);
    assert_eq!(steps[1]["uses"], RUST);
    assert_eq!(steps[1]["with"], json!({"toolchain": "1.99.0"}));
    assert_eq!(
        steps[2]["run"],
        concat!(
            "cargo run --locked -q -p simplestchat-release -- ops-artifact hosted",
            " --output \"$RUNNER_TEMP/ops-hosted\""
        )
    );
    assert_eq!(steps[3]["uses"], UPLOAD);
    let upload = &steps[3]["with"];
    assert!(upload.is_object());
    assert_eq!(
        upload["name"],
        format!("simplestchat-ops-unsigned-${{{{ github.sha }}}}-{TARGET}")
    );
    assert_eq!(
        paths(upload),
        fixed()
            .iter()
            .map(|name| format!("${{{{ runner.temp }}}}/ops-hosted/unsigned/{name}"))
            .collect::<Vec<_>>()
    );
    assert_eq!(upload["if-no-files-found"], "error");
    assert_eq!(upload["compression-level"], 0);
    assert_eq!(upload["retention-days"], 14);
    for step in steps {
        let step = step.as_object().unwrap();
        for key in ["if", "continue-on-error", "env"] {
            assert!(!step.contains_key(key), "{key}");
        }
    }
}

/// From `test_signer_binds_three_original_subjects_and_only_one_flat_payload_graph`:
/// no wildcard, third-party signer or self-verification can enter the job.
#[test]
fn signer_binds_three_original_subjects_and_only_one_flat_payload_graph() {
    let ci = workflow("ci.yml");
    let job = &ci["jobs"]["ops-release-security"];
    assert_eq!(job["if"], GUARD);
    assert_eq!(job["needs"], json!(["required", "ops-build"]));
    assert_eq!(job["runs-on"], "ubuntu-24.04");
    assert_eq!(job["timeout-minutes"], 10);
    assert_eq!(
        job["permissions"],
        json!({"contents": "read", "id-token": "write", "attestations": "write"})
    );
    let steps = steps(&ci, "ops-release-security");
    assert_eq!(steps.len(), 7);
    assert_eq!(steps[0]["uses"], CHECKOUT);
    assert_eq!(steps[0]["with"]["persist-credentials"], false);
    assert_eq!(steps[1]["uses"], RUST);
    assert_eq!(steps[1]["with"], json!({"toolchain": "1.99.0"}));
    assert_eq!(steps[2]["uses"], DOWNLOAD);
    assert_eq!(
        steps[2]["with"],
        json!({
            "name": format!("simplestchat-ops-unsigned-${{{{ github.sha }}}}-{TARGET}"),
            "path": "${{ runner.temp }}/ops-unsigned",
        })
    );
    assert_eq!(
        steps[3]["run"],
        concat!(
            "cargo run --locked -q -p simplestchat-release -- ops-artifact signing-inputs",
            " --directory \"$RUNNER_TEMP/ops-unsigned\" --output \"$RUNNER_TEMP/ops-signed\""
        )
    );
    assert_eq!(steps[4]["uses"], ATTEST);
    assert_eq!(steps[4]["id"], "ops-attest");
    let subjects = [
        "ops-preparation.json",
        "ops-release.json",
        "simplestchat-ops",
    ]
    .map(|name| format!("${{{{ runner.temp }}}}/ops-signed/{name}\n"))
    .concat();
    assert_eq!(
        steps[4]["with"],
        json!({
            "subject-path": subjects,
            "predicate-path": "${{ runner.temp }}/ops-signed/ops-predicate.json",
            "predicate-type":
                "https://github.com/swiftraccoon/simplestChat/attestations/ops-artifact/v1",
            "create-storage-record": false,
        })
    );
    assert_eq!(
        steps[5]["env"],
        json!({"ATTESTATION_BUNDLE": "${{ steps.ops-attest.outputs.bundle-path }}"})
    );
    assert_eq!(
        steps[5]["run"],
        concat!(
            "install -m 600 -- \"$ATTESTATION_BUNDLE\"",
            " \"$RUNNER_TEMP/ops-signed/ops-attestation.jsonl\""
        )
    );
    assert_eq!(steps[6]["uses"], UPLOAD);
    let upload = &steps[6]["with"];
    assert!(upload.is_object());
    assert_eq!(
        upload["name"],
        format!("simplestchat-ops-${{{{ github.sha }}}}-{TARGET}")
    );
    let mut signed = fixed();
    signed.push("ops-attestation.jsonl".to_owned());
    assert_eq!(
        paths(upload),
        signed
            .iter()
            .map(|name| format!("${{{{ runner.temp }}}}/ops-signed/{name}"))
            .collect::<Vec<_>>()
    );
    assert_eq!(upload["if-no-files-found"], "error");
    assert_eq!(upload["compression-level"], 0);
    assert_eq!(upload["retention-days"], 14);
    assert_eq!(
        ci["jobs"]["required"]["needs"],
        json!([
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
        ])
    );
    let server = &ci["jobs"]["release-security"];
    assert_eq!(server["needs"], json!(["required", "release-prepare"]));
    let predicates = super::steps(&ci, "release-security")
        .iter()
        .filter(|step| step["uses"] == ATTEST)
        .map(|step| &step["with"])
        .collect::<Vec<_>>();
    assert_eq!(predicates.len(), 1);
    assert!(predicates[0].is_object());
    assert_eq!(
        predicates[0]["predicate-type"],
        "https://github.com/swiftraccoon/simplestChat/attestations/release/v1"
    );
}
