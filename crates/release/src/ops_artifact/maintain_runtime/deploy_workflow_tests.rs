//! Active Rust deploy orchestration contracts; no workflow, Ansible or host runs.
use super::{control::Control, options::Purpose};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::Path,
    time::{Duration, Instant},
};

type Check<T> = std::result::Result<T, &'static str>;
fn require(ok: bool, cause: &'static str) -> Check<()> {
    if ok { Ok(()) } else { Err(cause) }
}
fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
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
const PREPARE: &str = "Prepare the free owned guest and pinned controller tools";
const COMPILE: &str = "Compile trusted controllers before exposing the API credential";
const INVOKE: &str = "Authenticate old/current contracts and exercise installed musl deploy";
const UPLOAD: &str = "Preserve only the sanitized deploy summary, including failed trials";
/// Whether `word` names a Python interpreter, by path or bare, with or without a version.
fn python(word: &str) -> bool {
    let name = word.trim_matches(['"', '\'']).rsplit('/').next();
    name.and_then(|name| name.strip_prefix("python"))
        .is_some_and(|version| {
            version
                .bytes()
                .all(|byte| byte == b'.' || byte.is_ascii_digit())
        })
}
/// A step's script runs in the default shell, names no Python source, and starts Python only
/// as `-m <module>` for the modules given; those are the ones that build Ansible's environment.
fn interpreter_contract(step: &Value, modules: &[&str]) -> Check<()> {
    const CAUSE: &str = "python-only-for-ansible";
    require(step.get("shell").is_none(), CAUSE)?;
    let Some(run) = step.get("run") else {
        return Ok(());
    };
    let words = words(run)?;
    require(!words.iter().any(|word| word.contains(".py")), CAUSE)?;
    let mut started = Vec::new();
    for (at, word) in words.iter().enumerate() {
        if python(word) {
            require(words.get(at + 1).is_some_and(|flag| flag == "-m"), CAUSE)?;
            started.push(words.get(at + 2).map(String::as_str).ok_or(CAUSE)?);
        }
    }
    require(started == modules, CAUSE)
}
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
    require(job.get("permissions").is_none(), "read-only-authority")?;
    require(words(&job["if"])? == "${{ github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/main' && github.repository == 'swiftraccoon/simplestChat' }}".split_whitespace().map(str::to_owned).collect::<Vec<_>>(), "manual-main-boundary")?;
    let steps = job["steps"].as_array().ok_or("steps")?;
    require(steps.len() == 7, "fixed-step-roster")?;
    let (_, toolchain) = named(steps, "Install the exact minimal Rust toolchain")?;
    require(words(&toolchain["run"])? == "rustup toolchain install 1.99.0 --profile minimal rustup target add --toolchain 1.99.0 x86_64-unknown-linux-gnu".split_whitespace().map(str::to_owned).collect::<Vec<_>>(), "rust-toolchain-pin")?;
    let (prepared_at, preparation) = named(steps, PREPARE)?;
    require(
        preparation["run"]
            .as_str()
            .is_some_and(|run| run.contains("sudo setfacl -m \"g:$(id -g):rw\" /dev/kvm")),
        "two-epoch-kvm-group-entitlement",
    )?;
    let (compiled_at, compiled) = named(steps, COMPILE)?;
    require(compiled.get("if").is_none(), "mandatory-source-build")?;
    require(words(&compiled["run"])? == "cargo build --locked -p simplestchat-release --bin simplestchat-release cargo build --locked --release --target x86_64-unknown-linux-gnu -p simplestchat-ops --bin simplestchat-ops".split_whitespace().map(str::to_owned).collect::<Vec<_>>(), "unprivileged-source-build")?;
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
        "./target/debug/simplestchat-release".to_owned(),
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
        let modules: &[&str] = if at == prepared_at {
            &["venv", "pip"]
        } else {
            &[]
        };
        interpreter_contract(step, modules)?;
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
            && python["with"]["python-version"] == "3.14.8"
            && python["name"] == "Select the pinned Python of the Ansible controller",
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
/// Whether `task` is a name, the one builtin `module`, and nothing else but `extras`.
fn only_module(task: &Value, module: &str, extras: &[&str]) -> bool {
    let module = format!("ansible.builtin.{module}");
    task.as_object().is_some_and(|fields| {
        fields.contains_key(&module)
            && fields
                .keys()
                .all(|key| key == "name" || *key == module || extras.contains(&key.as_str()))
    })
}
fn preparation_contract(value: &Value) -> Check<()> {
    let plays = value.as_array().ok_or("plays")?;
    require(plays.len() == 1, "one-preparation")?;
    let play = &plays[0];
    require(
        play.as_object()
            .ok_or("play")?
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            == BTreeSet::from([
                "name",
                "hosts",
                "become",
                "gather_facts",
                "vars_files",
                "pre_tasks",
                "tasks",
            ]),
        "preparation-only",
    )?;
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
    // Every task of the play is a name and one admitted module, so the play runs no command
    // or script of its own in the guest and overrides no verdict. What the imported engine
    // closure runs is that file's contract.
    require(
        before
            .iter()
            .zip(["assert", "slurp", "assert"])
            .all(|(task, module)| only_module(task, module, &["register", "no_log"]))
            && tasks
                .iter()
                .zip([
                    "apt",
                    "import_tasks",
                    "file",
                    "file",
                    "copy",
                    "copy",
                    "copy",
                ])
                .all(|(task, module)| only_module(task, module, &["loop"])),
        "preparation-only",
    )?;
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
        (
            vec!["jobs", "release-deploy-runtime", "permissions", "id-token"],
            json!("write"),
        ),
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
        (
            COMPILE,
            "run",
            json!("sudo cargo build --locked -p simplestchat-release"),
        ),
        (INVOKE, "run", json!("python3 -m ansible.cli.playbook")),
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
fn preparation_mutations_cannot_run_a_command_or_replace_fixed_inputs() {
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
    for (list, at, field, value) in [
        (
            "tasks",
            4,
            "ansible.builtin.command",
            json!({"argv":["python3","-I","-"]}),
        ),
        ("tasks", 4, "ansible.builtin.shell", json!("true")),
        ("tasks", 4, "ansible.builtin.script", json!("guest")),
        ("tasks", 4, "ansible.builtin.raw", json!("true")),
        ("tasks", 4, "command", json!("true")),
        ("tasks", 0, "ignore_errors", json!(true)),
        ("tasks", 6, "failed_when", json!(false)),
        ("tasks", 2, "delegate_to", json!("localhost")),
        ("pre_tasks", 0, "ansible.builtin.command", json!("true")),
        ("pre_tasks", 2, "ignore_errors", json!(true)),
    ] {
        let mut changed = original.clone();
        changed[0][list][at][field] = value;
        assert_eq!(
            preparation_contract(&changed),
            Err("preparation-only"),
            "{list} {at}: {field}"
        );
    }
    for section in ["post_tasks", "handlers", "roles"] {
        let mut changed = original.clone();
        changed[0][section] = json!([{"ansible.builtin.command":"true"}]);
        assert_eq!(
            preparation_contract(&changed),
            Err("preparation-only"),
            "{section}"
        );
    }
    // A task without its module has lost what it was admitted for, here the guest identity.
    let mut changed = original;
    changed[0]["pre_tasks"][0]
        .as_object_mut()
        .unwrap()
        .remove("ansible.builtin.assert");
    assert_eq!(preparation_contract(&changed), Err("preparation-only"));
}
#[test]
fn a_step_starts_python_only_to_build_the_ansible_environment() {
    let original = parse(".github/workflows/release-deploy-runtime.yml");
    let steps = original["jobs"]["release-deploy-runtime"]["steps"]
        .as_array()
        .unwrap();
    let prepared_at = named(steps, PREPARE).unwrap().0;
    let script = steps[prepared_at]["run"].as_str().unwrap();
    let venv = "python3 -m venv \"$RUNNER_TEMP/deploy-controller\"";
    let pip = "\"$RUNNER_TEMP/deploy-controller/bin/python\" -m pip --isolated install";
    assert!(script.contains(venv) && script.contains(pip));
    for changed_script in [
        format!("{script}python3 build/controller.py\n"),
        format!("{script}./build/controller.py\n"),
        format!("{script}python3 -c 'import os'\n"),
        format!("{script}python3 - <<'EOF'\nEOF\n"),
        format!("{script}python3.14 -I controller\n"),
        format!("{script}/usr/bin/python3 -m http.server\n"),
        format!("{script}\"$RUNNER_TEMP/deploy-controller/bin/python\" -m ansible\n"),
        format!("{script}python3 -m venv other\n"),
        script.replace(venv, "true"),
        script.replace("python3 -m venv", "python3 controller venv"),
        script.replace("-m pip --isolated", "-m ensurepip --isolated"),
    ] {
        assert_ne!(changed_script, script);
        let mut changed = original.clone();
        changed["jobs"]["release-deploy-runtime"]["steps"][prepared_at]["run"] =
            changed_script.as_str().into();
        assert_eq!(
            workflow_contract(&changed),
            Err("python-only-for-ansible"),
            "{changed_script}"
        );
    }
    for name in [PREPARE, COMPILE, INVOKE] {
        let mut changed = original.clone();
        let at = named(steps, name).unwrap().0;
        changed["jobs"]["release-deploy-runtime"]["steps"][at]["shell"] = "python".into();
        assert_eq!(
            workflow_contract(&changed),
            Err("python-only-for-ansible"),
            "{name}"
        );
    }
    for word in [
        "python",
        "python3",
        "python3.14",
        "/usr/bin/python3",
        "\"$V/bin/python\"",
    ] {
        assert!(python(word), "{word}");
    }
    for word in [
        "python-requirements.txt",
        "pythonic",
        "bpython",
        "python3/bin",
        "-m",
    ] {
        assert!(!python(word), "{word}");
    }
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
