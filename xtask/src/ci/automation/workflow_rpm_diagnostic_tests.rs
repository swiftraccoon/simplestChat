//! Current deployment retention policy beside the unchanged historical Python assertion.
//! These YAML contracts execute no scanner, encryption tool, workflow or Python loader.

use serde_json::{Value, json};
use std::{fs, path::Path};

type Check = Result<(), &'static str>;
const UPLOAD: &str = "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a";
const SCAN: &str = "Scan the same exported production image";
const ENCRYPT: &str = "Encrypt the optional failed-image RPM diagnostic";
const ENCRYPTED: &str = "Preserve only the encrypted RPM diagnostic";
const SANITIZED: &str = "Preserve sanitized release-container report";
// YAML preserves the line breaks before these more-indented continuation lines.
const ENCRYPT_GUARD: &str = concat!(
    "${{ failure() && github.event_name == 'push' &&\n",
    "    github.ref == 'refs/heads/main' &&\n",
    "    github.repository == 'swiftraccoon/simplestChat' && !env.ACT &&\n",
    "    steps.scanned-image-database.outcome == 'failure' &&\n",
    "    vars.SCPUB_RPM_DIAGNOSTIC_RECIPIENT != '' }}",
);
const UPLOAD_GUARD: &str = concat!(
    "${{ failure() && github.event_name == 'push' &&\n",
    "    github.ref == 'refs/heads/main' &&\n",
    "    github.repository == 'swiftraccoon/simplestChat' && !env.ACT &&\n",
    "    steps.encrypted-rpm-diagnostic.outcome == 'success' &&\n",
    "    steps.encrypted-rpm-diagnostic.outputs.ready == 'true' }}",
);
const ENCRYPT_COMMAND: &str = concat!(
    "cargo run --locked -q -p xtask -- ci rpm-diagnostic \\\n",
    "  --root \"${RUNNER_TEMP}/production-security\" \\\n",
    "  --output \"${RUNNER_TEMP}/encrypted-rpm-diagnostic\"\n",
);
const SCAN_COMMAND: &str = concat!(
    "build/check-security.sh image \"${PRODUCTION_IMAGE}\" \\\n",
    "  --artifact-dir \"${RUNNER_TEMP}/simplestchat-release\" \\\n",
    "  --image-tools \"${RUNNER_TEMP}/release-checks/tools\" \\\n",
    "  --image-database-cache \"${RUNNER_TEMP}/image-database-cache\" \\\n",
    "  --output \"${RUNNER_TEMP}/production-security\"\n",
    "cargo run --locked -q -p xtask -- ci image-cache status production\n",
);

fn require(condition: bool, cause: &'static str) -> Check {
    if condition { Ok(()) } else { Err(cause) }
}

fn named<'a>(steps: &'a [Value], name: &str) -> Result<(usize, &'a Value), &'static str> {
    let mut matches = steps
        .iter()
        .enumerate()
        .filter(|(_, step)| step["name"] == name);
    let found = matches.next().ok_or("missing-step")?;
    require(matches.next().is_none(), "duplicate-step")?;
    Ok(found)
}

fn encrypted_upload() -> Value {
    json!({
        "name": ENCRYPTED,
        "if": UPLOAD_GUARD,
        "uses": UPLOAD,
        "with": {
            "name": "encrypted-rpm-diagnostic-${{ github.run_id }}-${{ github.run_attempt }}",
            "path": concat!(
                "${{ runner.temp }}/encrypted-rpm-diagnostic/rpmdb.cms.der\n",
                "${{ runner.temp }}/encrypted-rpm-diagnostic/manifest.json\n",
            ),
            "if-no-files-found": "error",
            "compression-level": 0,
            "retention-days": 1,
        },
    })
}

fn ordinary_uploads() -> Vec<Value> {
    vec![
        json!({
            "name": "Preserve caddy validation metadata",
            "if": "${{ always() && !env.ACT }}",
            "uses": UPLOAD,
            "with": {
                "name": "config-validation-caddy",
                "path": concat!(
                    "${{ runner.temp }}/simplestchat-config-validation-caddy-*/",
                    "config-validation.json",
                ),
                "if-no-files-found": "ignore",
                "retention-days": 7,
            },
        }),
        json!({
            "name": "Retain the verified production release",
            "if": "${{ success() && github.event_name == 'push' && !env.ACT }}",
            "uses": UPLOAD,
            "with": {
                "name": "simplestchat-unsigned-${{ github.sha }}",
                "path": "${{ runner.temp }}/simplestchat-release",
                "if-no-files-found": "error",
                "compression-level": 0,
                "retention-days": 14,
            },
        }),
        json!({
            "name": "Preserve allowlisted production security evidence",
            "if": "${{ !cancelled() && !env.ACT }}",
            "uses": UPLOAD,
            "with": {
                "name": "production-security-${{ github.sha }}",
                "path": concat!(
                    "${{ runner.temp }}/production-security/image/outcome.json\n",
                    "${{ runner.temp }}/production-security/image/checks.json\n",
                    "${{ runner.temp }}/production-security/image/elf.json\n",
                    "${{ runner.temp }}/production-security/image/runtime-proof.json\n",
                    "${{ runner.temp }}/production-security/image/vex.openvex.json\n",
                    "${{ runner.temp }}/production-security/image/runtime-license-evidence.json\n",
                    "${{ runner.temp }}/production-security/image/native.json\n",
                    "${{ runner.temp }}/production-security/image/database-status.json\n",
                    "${{ runner.temp }}/production-security/image/scanner-result-*.json\n",
                    "${{ runner.temp }}/production-security/image/secret-paths.json\n",
                    "${{ runner.temp }}/production-security/image/sbom/sbom.syft.json\n",
                    "${{ runner.temp }}/production-security/image/spdx/sbom.spdx.json\n",
                ),
                "if-no-files-found": "warn",
                "retention-days": 14,
            },
        }),
        json!({
            "name": SANITIZED,
            "if": "${{ !cancelled() && !env.ACT }}",
            "uses": UPLOAD,
            "with": {
                "name": "release-container",
                "path": "${{ runner.temp }}/release-container/report.json",
                "if-no-files-found": "warn",
                "retention-days": 7,
            },
        }),
        json!({
            "name": "Preserve container diagnostics",
            "if": "${{ !cancelled() && !env.ACT }}",
            "uses": UPLOAD,
            "with": {
                "name": "container-smoke",
                "path": "${{ runner.temp }}/container-smoke",
                "if-no-files-found": "ignore",
                "retention-days": 7,
            },
        }),
    ]
}

fn uploads(steps: &[Value]) -> Vec<Value> {
    steps
        .iter()
        .filter(|step| {
            step["uses"]
                .as_str()
                .is_some_and(|value| value.contains("upload-artifact@"))
        })
        .cloned()
        .collect()
}

fn contract(workflow: &Value) -> Check {
    require(
        workflow["permissions"] == json!({"contents": "read"}),
        "read-only-job",
    )?;
    let job = &workflow["jobs"]["deployment"];
    for field in ["continue-on-error", "permissions", "environment"] {
        require(job.get(field).is_none(), "job-authority")?;
    }
    let steps = job["steps"].as_array().ok_or("steps")?;
    require(steps.iter().all(Value::is_object), "step-shape")?;
    let mut expected = ordinary_uploads();
    expected.insert(1, encrypted_upload());
    require(uploads(steps) == expected, "closed-upload-set")?;
    let (scan_at, scan) = named(steps, SCAN)?;
    require(
        scan == &json!({
            "name": SCAN,
            "id": "scanned-image-database",
            "timeout-minutes": 25,
            "run": SCAN_COMMAND,
        }),
        "required-image-policy",
    )?;
    let (encrypt_at, encrypt) = named(steps, ENCRYPT)?;
    require(
        encrypt
            == &json!({
                "name": ENCRYPT,
                "id": "encrypted-rpm-diagnostic",
                "if": ENCRYPT_GUARD,
                "timeout-minutes": 1,
                "shell": "bash",
                "env": {"RPM_DIAGNOSTIC_RECIPIENT": "${{ vars.SCPUB_RPM_DIAGNOSTIC_RECIPIENT }}"},
                "run": ENCRYPT_COMMAND,
            }),
        "closed-encryption-role",
    )?;
    let (upload_at, _) = named(steps, ENCRYPTED)?;
    let (release_at, _) = named(steps, "Retain the verified production release")?;
    require(
        scan_at + 1 == encrypt_at && encrypt_at + 1 == upload_at,
        "diagnostic-order",
    )?;
    require(upload_at < release_at, "publication-order")
}

fn current() -> Value {
    let bytes = fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join(".github/workflows/ci.yml"),
    )
    .unwrap();
    assert_eq!(
        bytes.as_slice(),
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../.github/workflows/ci.yml"
        ))
    );
    serde_saphyr::from_slice(&bytes).unwrap()
}

fn mutate(name: &str, change: impl FnOnce(&mut Value)) {
    let mut workflow = current();
    assert_eq!(contract(&workflow), Ok(()));
    let steps = workflow["jobs"]["deployment"]["steps"]
        .as_array_mut()
        .unwrap();
    let at = named(steps, name).unwrap().0;
    change(&mut steps[at]);
    assert!(contract(&workflow).is_err(), "{name}");
}

#[test]
fn current_retention_keeps_the_historical_five_uploads_and_only_two_encrypted_files() {
    assert_eq!(contract(&current()), Ok(()));
    let historical: Value =
        serde_saphyr::from_str(include_str!("reference/ci-python.yml")).unwrap();
    let steps = historical["jobs"]["deployment"]["steps"]
        .as_array()
        .unwrap();
    assert_eq!(uploads(steps), ordinary_uploads());
    assert_eq!(named(steps, SANITIZED).unwrap().1, &ordinary_uploads()[3]);
}

#[test]
fn uploads_refuse_missing_duplicate_extra_and_broadened_paths() {
    let workflow = current();
    assert_eq!(contract(&workflow), Ok(()));
    let steps = workflow["jobs"]["deployment"]["steps"].as_array().unwrap();
    for step in uploads(steps) {
        let name = step["name"].as_str().unwrap();
        for path in [
            "${{ runner.temp }}",
            "${{ runner.temp }}/production-security",
            "/tmp/**",
        ] {
            mutate(name, |step| step["with"]["path"] = json!(path));
        }
        mutate(name, |step| {
            step["with"]["path"] = json!(format!(
                "{}\n${{{{ runner.temp }}}}/production-security/image/rpmdb.sqlite",
                step["with"]["path"].as_str().unwrap(),
            ))
        });
        for duplicate in [false, true] {
            let mut changed = current();
            let rows = changed["jobs"]["deployment"]["steps"]
                .as_array_mut()
                .unwrap();
            let at = named(rows, name).unwrap().0;
            if duplicate {
                rows.push(rows[at].clone());
            } else {
                rows.remove(at);
            }
            assert!(contract(&changed).is_err());
        }
    }
    let mut changed = current();
    changed["jobs"]["deployment"]["steps"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "name": "Additional private data", "uses": UPLOAD,
            "with": {"path": "${{ runner.temp }}/encrypted-rpm-diagnostic"},
        }));
    assert!(contract(&changed).is_err());
}

#[test]
fn encrypted_delivery_refuses_every_guard_bypass_and_output_policy_change() {
    for (name, tokens) in [
        (
            ENCRYPT,
            vec![
                "failure()",
                "github.event_name == 'push'",
                "github.ref == 'refs/heads/main'",
                "github.repository == 'swiftraccoon/simplestChat'",
                "!env.ACT",
                "steps.scanned-image-database.outcome == 'failure'",
                "vars.SCPUB_RPM_DIAGNOSTIC_RECIPIENT != ''",
            ],
        ),
        (
            ENCRYPTED,
            vec![
                "failure()",
                "github.event_name == 'push'",
                "github.ref == 'refs/heads/main'",
                "github.repository == 'swiftraccoon/simplestChat'",
                "!env.ACT",
                "steps.encrypted-rpm-diagnostic.outcome == 'success'",
                "steps.encrypted-rpm-diagnostic.outputs.ready == 'true'",
            ],
        ),
    ] {
        for token in tokens {
            mutate(name, |step| {
                let before = step["if"].as_str().unwrap();
                assert!(before.contains(token));
                step["if"] = json!(before.replace(token, "true"));
            });
        }
        mutate(name, |step| {
            step.as_object_mut().unwrap().remove("if");
        });
        mutate(name, |step| step["continue-on-error"] = json!(true));
        mutate(name, |step| step["working-directory"] = json!("/tmp"));
    }
    for (field, value) in [
        ("if-no-files-found", json!("ignore")),
        ("retention-days", json!(14)),
        ("compression-level", json!(9)),
        ("include-hidden-files", json!(true)),
        ("name", json!("encrypted-rpm-diagnostic")),
    ] {
        mutate(ENCRYPTED, |step| step["with"][field] = value);
    }
    mutate(ENCRYPTED, |step| step["run"] = json!("true"));
    mutate(ENCRYPT, |step| step["timeout-minutes"] = json!(2));
    mutate(ENCRYPT, |step| {
        step["env"] = json!({"RPM_DIAGNOSTIC_RECIPIENT": "unjoined"})
    });
    mutate(ENCRYPT, |step| {
        step["run"] = json!(ENCRYPT_COMMAND.replace("--locked ", ""))
    });
}

#[test]
fn diagnostics_cannot_skip_the_scan_change_publication_or_run_before_failure_evidence() {
    mutate(SCAN, |step| step["if"] = json!("success()"));
    mutate(SCAN, |step| step["continue-on-error"] = json!(true));
    mutate(SCAN, |step| step["run"] = json!("true"));
    mutate("Retain the verified production release", |step| {
        step["if"] = json!("always()")
    });
    mutate(SANITIZED, |step| step["run"] = json!("true"));
    for name in [ENCRYPT, ENCRYPTED] {
        let mut workflow = current();
        let steps = workflow["jobs"]["deployment"]["steps"]
            .as_array_mut()
            .unwrap();
        let scan_at = named(steps, SCAN).unwrap().0;
        let at = named(steps, name).unwrap().0;
        steps.swap(scan_at, at);
        assert!(contract(&workflow).is_err());
    }
}
