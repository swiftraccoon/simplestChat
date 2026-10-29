//! Active Rust deploy orchestration contracts; no workflow, Ansible or host runs.
use super::{control::Control, options::Purpose};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

type Check<T> = std::result::Result<T, &'static str>;
fn require(ok: bool, cause: &'static str) -> Check<()> {
    if ok { Ok(()) } else { Err(cause) }
}
fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap()
}
fn parse(path: &str) -> Value {
    let bytes = fs::read(root().join(path)).unwrap();
    serde_saphyr::from_slice(&bytes).unwrap()
}
fn named<'a>(steps: &'a [Value], name: &str) -> Check<(usize, &'a Value)> {
    let found = steps
        .iter()
        .enumerate()
        .filter(|(_, value)| value["name"] == name)
        .collect::<Vec<_>>();
    require(found.len() == 1, "unique-step")?;
    Ok(found[0])
}
fn words(value: &Value) -> Check<Vec<String>> {
    Ok(value
        .as_str()
        .ok_or("text")?
        .replace("\\\n", " ")
        .split_whitespace()
        .map(str::to_owned)
        .collect())
}
const INPUTS: [&str; 8] = [
    "run_id",
    "run_attempt",
    "artifact_id",
    "server_artifact_id",
    "previous_revision",
    "previous_run_id",
    "previous_run_attempt",
    "previous_artifact_id",
];
const COMPILE: &str = "Compile trusted controllers before exposing the API credential";
const INVOKE: &str = "Authenticate old/current contracts and exercise installed musl deploy";
const UPLOAD: &str = "Preserve only the sanitized deploy summary, including failed trials";
fn workflow_contract(value: &Value) -> Check<()> {
    require(
        value["permissions"] == json!({"contents":"read","actions":"read"}),
        "read-only-authority",
    )?;
    require(
        value["concurrency"]["group"] == "release-deploy-runtime-${{ github.ref }}"
            && value["concurrency"]["cancel-in-progress"] == false,
        "single-owner",
    )?;
    let inputs = value["on"]["workflow_dispatch"]["inputs"]
        .as_object()
        .ok_or("inputs")?;
    require(
        inputs.len() == INPUTS.len() && INPUTS.iter().all(|name| inputs.contains_key(*name)),
        "exact-selectors",
    )?;
    for input in inputs.values() {
        require(
            input["required"] == true && input["type"] == "string",
            "explicit-input",
        )?;
    }
    let jobs = value["jobs"].as_object().ok_or("jobs")?;
    require(jobs.len() == 1, "one-fixture-job")?;
    let job = &jobs["release-deploy-runtime"];
    require(
        job["runs-on"] == "ubuntu-24.04" && job["timeout-minutes"] == 90,
        "outer-owner",
    )?;
    require(words(&job["if"])? == "${{ github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/main' && github.repository == 'swiftraccoon/simplestChat' }}".split_whitespace().map(str::to_owned).collect::<Vec<_>>(), "manual-main-boundary")?;
    let steps = job["steps"].as_array().ok_or("steps")?;
    require(steps.len() == 7, "fixed-step-roster")?;
    let (_, toolchain) = named(steps, "Install the exact minimal Rust toolchain")?;
    require(words(&toolchain["run"])? == "rustup toolchain install 1.99.0 --profile minimal rustup target add --toolchain 1.99.0 x86_64-unknown-linux-gnu".split_whitespace().map(str::to_owned).collect::<Vec<_>>(), "rust-toolchain-pin")?;
    let (_, preparation) = named(
        steps,
        "Prepare the free owned guest and pinned controller tools",
    )?;
    require(
        preparation["run"]
            .as_str()
            .is_some_and(|run| run.contains("sudo setfacl -m \"g:$(id -g):rw\" /dev/kvm")),
        "two-epoch-kvm-group-entitlement",
    )?;
    let (compiled_at, compiled) = named(steps, COMPILE)?;
    require(compiled.get("if").is_none(), "mandatory-source-build")?;
    require(words(&compiled["run"])? == "cargo build --locked -p xtask --bin xtask cargo build --locked --release --target x86_64-unknown-linux-gnu -p simplestchat-ops --bin simplestchat-ops".split_whitespace().map(str::to_owned).collect::<Vec<_>>(), "unprivileged-source-build")?;
    let (invoked_at, invoke) = named(steps, INVOKE)?;
    require(
        compiled_at < invoked_at && invoke["timeout-minutes"] == 80 && invoke.get("if").is_none(),
        "compile-before-token",
    )?;
    require(
        invoke["env"]["GH_TOKEN"] == "${{ github.token }}",
        "credential-leaf",
    )?;
    let mut expected = vec![
        "./target/debug/xtask".to_owned(),
        "ops-artifact".into(),
        "deploy-runtime".into(),
    ];
    for (input, variable) in INPUTS.into_iter().zip([
        "DEPLOY_RUN_ID",
        "DEPLOY_RUN_ATTEMPT",
        "DEPLOY_ARTIFACT_ID",
        "DEPLOY_SERVER_ARTIFACT_ID",
        "DEPLOY_PREVIOUS_REVISION",
        "DEPLOY_PREVIOUS_RUN_ID",
        "DEPLOY_PREVIOUS_RUN_ATTEMPT",
        "DEPLOY_PREVIOUS_ARTIFACT_ID",
    ]) {
        require(
            invoke["env"][variable] == format!("${{{{ inputs.{input} }}}}"),
            "selector-environment",
        )?;
        expected.extend([
            format!("--{}", input.replace('_', "-")),
            format!("\"${variable}\""),
        ]);
    }
    expected.extend([
        "--output".into(),
        "\"$RUNNER_TEMP/release-deploy-runtime\"".into(),
    ]);
    require(words(&invoke["run"])? == expected, "typed-controller-leaf")?;
    for environment in [&value["env"], &job["env"]] {
        require(
            environment.get("GH_TOKEN").is_none() && environment.get("GITHUB_TOKEN").is_none(),
            "no-global-token",
        )?;
    }
    for (at, step) in steps.iter().enumerate() {
        require(
            step.get("continue-on-error").is_none(),
            "actual-step-verdict",
        )?;
        if at != invoked_at {
            require(
                step["env"].get("GH_TOKEN").is_none() && step["env"].get("GITHUB_TOKEN").is_none(),
                "no-early-token",
            )?;
        }
        if let Some(run) = step["run"].as_str() {
            require(
                ![
                    "security_vm_deploy.py",
                    "security_vm_deploy_guest.py",
                    "security_vm_deploy_injection.py",
                ]
                .iter()
                .any(|name| run.contains(name)),
                "no-frozen-python-execution",
            )?;
        }
    }
    let checkout = steps
        .iter()
        .find(|step| {
            step["uses"]
                .as_str()
                .is_some_and(|name| name.starts_with("actions/checkout@"))
        })
        .ok_or("checkout")?;
    require(
        checkout["uses"] == "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1"
            && checkout["with"]["persist-credentials"] == false,
        "checkout-pin",
    )?;
    let python = steps
        .iter()
        .find(|step| {
            step["uses"]
                .as_str()
                .is_some_and(|name| name.starts_with("actions/setup-python@"))
        })
        .ok_or("python")?;
    require(
        python["uses"] == "actions/setup-python@ece7cb06caefa5fff74198d8649806c4678c61a1"
            && python["with"]["python-version"] == "3.14.8",
        "keeper-python-pin",
    )?;
    let (_, upload) = named(steps, UPLOAD)?;
    require(
        upload["if"] == "${{ always() }}"
            && upload["uses"] == "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a",
        "terminal-summary-pin",
    )?;
    require(
        upload["with"]["path"] == "${{ runner.temp }}/release-deploy-runtime/summary.json"
            && upload["with"]["retention-days"] == 14,
        "summary-only",
    )?;
    Ok(())
}
fn preparation_contract(value: &Value) -> Check<()> {
    let plays = value.as_array().ok_or("plays")?;
    require(plays.len() == 1, "one-preparation")?;
    let play = &plays[0];
    require(
        play["hosts"] == "benchmark_hosts" && play["become"] == true,
        "owned-guest",
    )?;
    require(
        play["vars_files"] == json!(["group_vars/benchmark_hosts.yml"]),
        "fixed-fixture-vars",
    )?;
    let before = play["pre_tasks"].as_array().ok_or("marker-preconditions")?;
    require(
        before.len() == 3
            && before[1]["ansible.builtin.slurp"]["src"] == "/etc/simplestchat-vm-fixture"
            && before[1]["no_log"] == true
            && before[2]["no_log"] == true,
        "original-marker",
    )?;
    let tasks = play["tasks"].as_array().ok_or("tasks")?;
    require(tasks.len() == 7, "fixed-preparation-operations")?;
    require(
        tasks[0]["ansible.builtin.apt"]["install_recommends"] == false
            && tasks[0]["ansible.builtin.apt"]["fail_on_autoremove"] == true
            && tasks[0]["ansible.builtin.apt"]["state"] == "present",
        "signed-bounded-prerequisites",
    )?;
    require(
        tasks[1]["ansible.builtin.import_tasks"] == "tasks/docker.yml",
        "pinned-engine-closure",
    )?;
    for task in tasks {
        require(
            task.get("ansible.builtin.shell").is_none()
                && task.get("ansible.builtin.command").is_none()
                && task.get("ignore_errors").is_none()
                && task.get("failed_when").is_none(),
            "preparation-only",
        )?;
    }
    require(
        tasks[2]["ansible.builtin.file"]["mode"] == "0700"
            && tasks[2]["ansible.builtin.file"]["owner"] == "root"
            && tasks[2]["ansible.builtin.file"]["group"] == "root",
        "private-directories",
    )?;
    require(
        tasks[3]["ansible.builtin.file"]
            == json!({"path":"/usr/local/libexec/simplestchat-bench","state":"directory","owner":"root","group":"root","mode":"0755"}),
        "operational-helper-directory",
    )?;
    for (at, source, target) in [
        (
            4,
            "{{ vm_maintain_bundle_path }}",
            "/root/simplestchat-vm-fixture/maintain-source.bundle",
        ),
        (
            5,
            "{{ vm_current_artifact_dir }}/{{ item }}",
            "/root/simplestchat-vm-fixture/maintain-prerequisite/server-input/{{ item }}",
        ),
        (
            6,
            "{{ vm_previous_artifact_dir }}/{{ item }}",
            "/root/simplestchat-vm-fixture/deploy-runtime/previous-input/{{ item }}",
        ),
    ] {
        let copy = &tasks[at]["ansible.builtin.copy"];
        require(
            copy["src"] == source
                && copy["dest"] == target
                && copy["owner"] == "root"
                && copy["group"] == "root"
                && copy["mode"] == "0600",
            "original-payload-copy",
        )?;
        if at != 4 {
            require(
                tasks[at]["loop"] == json!(["release.json", "image.tar"]),
                "two-original-payloads",
            )?;
        }
    }
    Ok(())
}
#[test]
fn active_workflow_and_guest_preparation_preserve_fixed_authority_and_inputs() {
    workflow_contract(&parse(".github/workflows/release-deploy-runtime.yml")).unwrap();
    preparation_contract(&parse("ops/ansible/security-vm-native-deploy.yml")).unwrap();
}
#[test]
fn workflow_mutations_cannot_bypass_build_token_selector_or_terminal_proof() {
    let original = parse(".github/workflows/release-deploy-runtime.yml");
    for (path, replacement) in [
        (vec!["permissions", "contents"], json!("write")),
        (vec!["env", "GH_TOKEN"], json!("${{ github.token }}")),
        (
            vec!["jobs", "release-deploy-runtime", "timeout-minutes"],
            json!(91),
        ),
    ] {
        let mut changed = original.clone();
        let mut target = &mut changed;
        for key in path {
            if target.is_null() {
                *target = json!({});
            }
            target = &mut target[key];
        }
        *target = replacement;
        assert!(workflow_contract(&changed).is_err());
    }
    for (name, field, replacement) in [
        (COMPILE, "run", json!("sudo cargo build --locked -p xtask")),
        (INVOKE, "run", json!("python3 build/security_vm_deploy.py")),
        (INVOKE, "timeout-minutes", json!(81)),
        (INVOKE, "continue-on-error", json!(true)),
        (
            UPLOAD,
            "with",
            json!({"path":"private/raw.log","retention-days":14}),
        ),
    ] {
        let mut changed = original.clone();
        let steps = changed["jobs"]["release-deploy-runtime"]["steps"]
            .as_array_mut()
            .unwrap();
        let at = named(steps, name).unwrap().0;
        steps[at][field] = replacement;
        assert!(workflow_contract(&changed).is_err(), "{name}: {field}");
    }
    for input in INPUTS {
        let mut changed = original.clone();
        changed["on"]["workflow_dispatch"]["inputs"]
            .as_object_mut()
            .unwrap()
            .remove(input);
        assert!(workflow_contract(&changed).is_err());
    }
}
#[test]
fn preparation_mutations_cannot_execute_frozen_guests_or_replace_fixed_inputs() {
    let original = parse("ops/ansible/security-vm-native-deploy.yml");
    for (at, field, value) in [
        (0, "install_recommends", json!(true)),
        (5, "dest", json!("/srv/foreign/{{ item }}")),
        (6, "mode", json!("0644")),
    ] {
        let mut changed = original.clone();
        let module = if at == 0 {
            "ansible.builtin.apt"
        } else {
            "ansible.builtin.copy"
        };
        changed[0]["tasks"][at][module][field] = value;
        assert!(preparation_contract(&changed).is_err());
    }
    let mut changed = original;
    changed[0]["tasks"][4]["ansible.builtin.command"] =
        json!({"argv":["python3","security_vm_deploy_guest.py"]});
    assert!(preparation_contract(&changed).is_err());
}
#[test]
fn deploy_profile_retains_its_original_entry_clock_and_two_call_debt() {
    let start = Instant::now();
    let mut control = Control::for_purpose(start, Purpose::Deploy).unwrap();
    let facts = (
        control.whole(),
        control.setup_limit(),
        control.debt(),
        control.end(),
        control.whole_body_end(),
        control.body_end(),
    );
    let closed = control.finish();
    assert_eq!(
        facts,
        (
            Duration::from_secs(4500),
            Duration::from_secs(600),
            Duration::from_secs(3720),
            start + Duration::from_secs(4500),
            start + Duration::from_secs(4320),
            start + Duration::from_secs(600)
        )
    );
    assert!(closed.is_ok());
}
