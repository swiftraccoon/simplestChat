//! The benchmark host automation: rendered shell and unit templates, the
//! inventory and tag behaviour of the real Ansible command line, and the
//! release-state guard shared by scripts and playbooks. Carried over from the
//! Python automation tests; nothing connects to a host or starts Docker.

use super::ansible::{Scratch, ansible, evaluate, render, succeeded};
use super::{checkout_file, document, first, located};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

const REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const GROUP_VARS: &str = "ops/ansible/group_vars/benchmark_hosts.yml";
const BUILD_IMAGES: &str = "build-images.sh.j2";
const RUN_BENCHMARK: &str = "run-benchmark.sh.j2";

/// The benchmark templates rendered once with fixed, non-production values.
fn rendered(name: &str) -> &'static str {
    static RENDERED: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    let all = RENDERED.get_or_init(|| {
        let names = [
            BUILD_IMAGES,
            RUN_BENCHMARK,
            "benchmark.service.j2",
            "image-build.service.j2",
            "docker.preferences.j2",
        ];
        let overrides = json!({
            "scbench_revision": REVISION,
            "scbench_release_dir": format!("{{{{ scbench_root }}}}/sources/{REVISION}"),
        });
        render(&names, GROUP_VARS, &overrides)
    });
    &all[name]
}

/// The exact inline release-state guard a script embeds.
fn embedded_guard(script: &str) -> &str {
    let guard = regex::Regex::new(r"(?s)/bin/sh -eu <<'GUARD'\n(.*?)\nGUARD\n").unwrap();
    let found = guard.captures(script).expect("a release-state guard");
    found.get(1).unwrap().as_str()
}

/// The recorded answers of the retired inline Python guard, with the record
/// states they were given.
fn recorded_guard() -> Value {
    serde_json::from_str(include_str!("release_state_guard_recorded.json")).unwrap()
}

/// Build one recorded state under `scratch` and return the root directory the
/// guard is pointed at.
fn guard_state(case: &Value, scratch: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let mode = |text: &Value| {
        let bits = u32::from_str_radix(text.as_str().unwrap(), 8).unwrap();
        std::fs::Permissions::from_mode(bits)
    };
    let parent = scratch.join("root");
    std::fs::create_dir(&parent).unwrap();
    let record = parent.join("release-state.json");
    let content = case["content"].as_str().unwrap_or_default();
    match case["kind"].as_str().unwrap() {
        "file" => {
            let width = case["pad"].as_u64().map_or(0, |pad| pad as usize);
            std::fs::write(&record, format!("{content:<width$}")).unwrap();
            std::fs::set_permissions(&record, mode(&case["mode"])).unwrap();
        }
        "directory" => std::fs::create_dir(&record).unwrap(),
        "link" => {
            let target = parent.join("target.json");
            std::fs::write(&target, content).unwrap();
            std::fs::set_permissions(&target, mode(&json!("600"))).unwrap();
            symlink(&target, &record).unwrap();
        }
        "dangling" => symlink(parent.join("missing.json"), &record).unwrap(),
        "pipe" => {
            let mut made = Command::new("mkfifo");
            made.args(["-m", "600"]).arg(&record);
            assert!(made.status().unwrap().success());
        }
        "none" => {}
        other => panic!("unknown recorded state {other}"),
    }
    let parent_mode = case.get("parentMode").cloned().unwrap_or(json!("700"));
    std::fs::set_permissions(&parent, mode(&parent_mode)).unwrap();
    if case["parentLink"] == true {
        symlink(&parent, scratch.join("link")).unwrap();
        return scratch.join("link");
    }
    parent
}

/// Whether a program accepts `script` on standard input.
fn accepts(program: &str, arguments: &[&str], script: &str) -> bool {
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    input.write_all(script.as_bytes()).unwrap();
    drop(input);
    child.wait().unwrap().success()
}

/// From `test_shell_templates_parse_and_pass_shellcheck`.
#[test]
fn shell_templates_parse_and_pass_shellcheck() {
    for name in [BUILD_IMAGES, RUN_BENCHMARK] {
        let script = rendered(name);
        assert!(!script.contains("{{ scbench_"), "{name}");
        assert!(accepts("bash", &["-n"], script), "{name}: bash -n");
        assert!(
            accepts("shellcheck", &["--shell=bash", "-"], script),
            "{name}: shellcheck"
        );
    }
}

/// From `test_inventory_host_values_override_safe_defaults`: the real
/// inventory command merges a host's values over the group defaults.
#[test]
fn inventory_host_values_override_safe_defaults() {
    let scratch = Scratch::new();
    let inventory = scratch.path().join("inventory.yml");
    let hosts = json!({"benchmark_hosts": {"hosts": {"fixture": {
        "ansible_connection": "local",
        "scbench_revision": REVISION,
        "scbench_upgrade_packages": true,
        "scbench_reboot": true,
        "scbench_workload": {"clients": 8, "workers": 2},
    }}}});
    std::fs::write(&inventory, hosts.to_string()).unwrap();
    let arguments = [
        "--playbook-dir".as_ref(),
        ".".as_ref(),
        "-i".as_ref(),
        inventory.as_os_str(),
        "--host".as_ref(),
        "fixture".as_ref(),
    ];
    let output = succeeded(&ansible("ansible-inventory", &arguments, &scratch));
    let values: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(values["scbench_upgrade_packages"], true);
    assert_eq!(values["scbench_reboot"], true);
    assert_eq!(values["scbench_workload"]["workers"], 2);
    let defaults = document(GROUP_VARS);
    assert_eq!(defaults["scbench_upgrade_packages"], false);
    assert_eq!(defaults["scbench_reboot"], false);
}

/// From `test_tagged_runs_cannot_skip_safety_preflight`.
#[test]
fn tagged_runs_cannot_skip_safety_preflight() {
    let scratch = Scratch::new();
    let arguments = [
        "-i",
        "inventory.example.yml",
        "site.yml",
        "--list-tasks",
        "--tags",
        "benchmark",
    ];
    let listed = succeeded(&ansible("ansible-playbook", &arguments, &scratch));
    for guard in [
        "Require the supported host",
        "Refuse provisioning during an active benchmark",
        "Refuse provisioning during an active image build",
        "Refuse provisioning while container cleanup remains uncertain",
        "Refuse provisioning while a public release remains unfinished",
    ] {
        assert!(listed.contains(guard), "{guard}");
    }
    assert!(
        listed.find("Refuse provisioning").unwrap()
            < listed
                .find("Install the owned container lifecycle helpers")
                .unwrap()
    );
}

/// From `test_services_are_explicit_and_preserve_owned_cleanup`: starting a
/// build must not stop a running benchmark, image verification must not leave
/// daemon-owned processes, and a failed verification must not publish the
/// stable manifest.
#[test]
fn services_are_explicit_and_preserve_owned_cleanup() {
    let benchmark = rendered("benchmark.service.j2");
    for unit in [benchmark, rendered("image-build.service.j2")] {
        for forbidden in ["[Install]", "Restart=always", "Conflicts="] {
            assert!(!unit.contains(forbidden), "{forbidden}");
        }
    }
    for text in [
        "ExecStopPost=/usr/local/libexec/simplestchat-bench/cleanup-container-benchmark.sh",
        "RuntimeMaxSec=900",
        "RuntimeDirectoryPreserve=yes",
    ] {
        assert!(benchmark.contains(text), "{text}");
    }
    let script = rendered(BUILD_IMAGES);
    let publish = "ln \"$attempt/images.json\" \"$image_manifest\"";
    for text in [
        "/run/simplestchat-bench/workload.lock",
        ".finalized == true",
        publish,
    ] {
        assert!(script.contains(text), "{text}");
    }
    assert!(!script.contains("docker run"));
    let verified = script
        .find("verify_images \"$attempt/images.json\"")
        .unwrap();
    assert!(verified < script.find(publish).unwrap());
}

/// From `test_package_preferences_cover_only_the_five_selected_versions`.
#[test]
fn package_preferences_cover_only_the_five_selected_versions() {
    let preferences = rendered("docker.preferences.j2");
    let records = preferences
        .split("\n\n")
        .filter(|block| block.contains("Package:"));
    let expected = [
        ("docker-ce", "5:29.8.2-1~debian.13~trixie"),
        ("docker-ce-cli", "5:29.8.2-1~debian.13~trixie"),
        ("containerd.io", "2.3.6-1~debian.13~trixie"),
        ("docker-buildx-plugin", "0.37.1-1~debian.13~trixie"),
        ("docker-compose-plugin", "5.6.0-1~debian.13~trixie"),
    ];
    assert_eq!(records.count(), expected.len());
    for (package, version) in expected {
        let pin = format!("Package: {package}\nPin: version {version}\nPin-Priority: 1000");
        assert!(preferences.contains(&pin), "{package}");
    }
    let tasks = checkout_file("ops/ansible/tasks/docker.yml");
    assert!(tasks.find("docker.preferences.j2").unwrap() < tasks.find("docker-ce={{").unwrap());
    assert!(tasks.contains("allow_downgrade: false"));
}

/// From `test_image_build_refuses_a_running_public_project_before_image_work`:
/// both the unknown and the running public state fail closed, and the
/// preflight may only inspect containers.
#[test]
fn image_build_refuses_a_running_public_project_before_image_work() {
    let script = rendered(BUILD_IMAGES);
    let at = |text: &str| script.find(text).unwrap();
    let start = at("running_public=$(timeout --signal=TERM --kill-after=1s 8s docker ps --quiet");
    let end = at("cd \"$source_root\"");
    let guard = &script[start..end];
    for text in [
        "--filter 'label=com.docker.compose.project=simplestchat-public'",
        "Cannot verify whether public chat is running",
        "[[ -z \"$running_public\" ]] || {",
        "Public chat is running; stop it explicitly",
    ] {
        assert!(guard.contains(text), "{text}");
    }
    assert_eq!(guard.matches("exit 1;").count(), 2);
    assert_eq!(guard.matches("docker ").count(), 1);
    assert!(at("flock -n 9") < start);
    assert!(at(".finalized == true") < start);
    assert!(end < at("verify_images()"));
    assert!(end < at("docker build --pull"));
}

/// From `test_persistent_release_guards_are_identical_and_precede_work`: the
/// four scripts and the two playbooks carry one identical guard, placed
/// before any work.
#[test]
fn persistent_release_guards_are_identical_and_precede_work() {
    let scripts = [
        rendered(BUILD_IMAGES).to_owned(),
        rendered(RUN_BENCHMARK).to_owned(),
        checkout_file("build/benchmark-container.sh"),
        checkout_file("ops/ansible/files/deploy-public.sh"),
    ];
    let guard = embedded_guard(&scripts[0]);
    for script in &scripts {
        assert_eq!(embedded_guard(script), guard);
    }
    for filename in ["site.yml", "public.yml"] {
        let playbook = document(&format!("ops/ansible/{filename}"));
        let tasks = playbook[0]["pre_tasks"].as_array().unwrap();
        let task = first(tasks, |task| {
            task.get("ansible.builtin.command")
                .is_some_and(|command| command.to_string().contains("release-state.json"))
        });
        assert_eq!(task["tags"], json!(["always"]), "{filename}");
        assert_eq!(task["changed_when"], false, "{filename}");
        assert_eq!(task["check_mode"], false, "{filename}");
        let argv = task["ansible.builtin.command"]["argv"].as_array().unwrap();
        assert_eq!(argv.len(), 4, "{filename}");
        assert_eq!(argv[..3], [json!("/bin/sh"), json!("-eu"), json!("-c")]);
        let embedded = argv[argv.len() - 1].as_str().unwrap();
        assert_eq!(embedded.trim_end(), guard, "{filename}");
    }
    for (script, before, after) in [
        (&scripts[0], "flock -n 9", "running_public=$("),
        (&scripts[2], "flock --exclusive --nonblock 9", "endpoint="),
        (&scripts[3], "flock -n 9", "compose()"),
    ] {
        let guarded = script.find(guard).unwrap();
        assert!(script.find(before).unwrap() < guarded, "{before}");
        assert!(guarded < script.find(after).unwrap(), "{after}");
    }
    assert!(scripts[1].find(guard).unwrap() < scripts[1].find("manifest=").unwrap());
}

/// The shell guard gives the answer the retired Python guard gave for every
/// recorded state of the release record: absent or finalized passes, an
/// unfinished release asks for recovery, and anything else about the file,
/// its directory or its shape refuses the precondition. Only the fixed root
/// and owner are substituted, for a scratch directory and the current user.
#[test]
fn release_state_guard_answers_as_the_recorded_python_guard_did() {
    use std::os::unix::fs::MetadataExt;
    let recorded = recorded_guard();
    let guard = embedded_guard(rendered(RUN_BENCHMARK));
    assert!(accepts("shellcheck", &["--shell=sh", "-"], guard));
    assert!(!guard.contains("python") && !guard.contains("{{") && !guard.contains("{%"));
    let cases = recorded["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 39);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let scratch = Scratch::new();
        let root = guard_state(case, scratch.path());
        let owner = std::fs::metadata(scratch.path()).unwrap().uid();
        let fixed = "root=/srv/simplestchat-public\n";
        assert_eq!(guard.matches(fixed).count(), 1);
        assert_eq!(guard.matches("-user 0 ").count(), 2);
        // The record's owner is the first test and its directory's the second;
        // a state "owned by another user" expects an owner this user is not.
        let expected = |field: &str| owner + u32::from(case[field] == "other");
        let program = guard
            .replace(fixed, &format!("root='{}'\n", root.display()))
            .replacen(
                "-user 0 ",
                &format!("-user {} ", expected("recordOwner")),
                1,
            )
            .replacen(
                "-user 0 ",
                &format!("-user {} ", expected("parentOwner")),
                1,
            );
        let output = Command::new("/bin/sh")
            .args(["-eu", "-c", &program])
            .output()
            .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        if name == "schema version as an exponent" {
            // Python read `1e0` as a float and refused the precondition; jq
            // reads the integer one, so this finalized record passes.
            assert_eq!(case["stderr"], "Operational precondition failed\n");
            assert_eq!(output.status.code(), Some(0), "{name}");
            continue;
        }
        // Malformed text ended the Python guard with a traceback; the shell
        // guard names the same refusal as for any other unreadable shape.
        let refusal = match case["stderr"].as_str().unwrap() {
            "<traceback>" => "Operational precondition failed\n",
            message => message,
        };
        let status = output.status.code().map(i64::from);
        assert_eq!(status, case["status"].as_i64(), "{name}: {stderr}");
        assert_eq!(stdout, case["stdout"].as_str().unwrap(), "{name}");
        assert_eq!(stderr, refusal, "{name}");
    }
}

/// From `test_static_units_are_inspected_without_repeated_disable_changes`:
/// the units are inspected without a change, and only an enabled unit is
/// disabled. The conditions are evaluated by Ansible itself.
#[test]
fn static_units_are_inspected_without_repeated_disable_changes() {
    let document = document("ops/ansible/tasks/benchmark.yml");
    let tasks = document.as_array().unwrap();
    assert!(tasks.iter().all(Value::is_object));
    let (inspected, inspection) =
        located(tasks, |task| task["register"] == "scbench_unit_enablement");
    let (guarded, guard) = located(tasks, |task| task.get("ansible.builtin.assert").is_some());
    let (disabled, disable) = located(tasks, |task| {
        task.get("ansible.builtin.systemd_service").is_some()
    });
    let (reloaded, _) = located(tasks, |task| {
        task["ansible.builtin.meta"] == "flush_handlers"
    });
    assert_eq!(
        inspection["ansible.builtin.command"]["argv"],
        json!([
            "/usr/bin/systemctl",
            "is-enabled",
            "simplestchat-{{ item }}.service"
        ])
    );
    assert_eq!(inspection["loop"], json!(["image-build", "benchmark"]));
    assert_eq!(inspection["changed_when"], false);
    assert_eq!(inspection["check_mode"], false);
    assert!(reloaded < inspected && inspected < guarded && guarded < disabled);
    assert_eq!(guard["loop"], "{{ scbench_unit_enablement.results }}");
    assert_eq!(disable["loop"], guard["loop"]);
    assert_eq!(
        disable["ansible.builtin.systemd_service"],
        json!({"name": "simplestchat-{{ item.item }}.service", "enabled": false})
    );
    // An actual disable keeps normal change reporting.
    assert!(disable.get("changed_when").is_none());

    let accepted = guard["ansible.builtin.assert"]["that"].as_array().unwrap();
    let mut expressions = accepted
        .iter()
        .map(|rule| rule.as_str().unwrap())
        .collect::<Vec<_>>();
    let failed_when = inspection["failed_when"].as_str().unwrap();
    expressions.extend([disable["when"].as_str().unwrap(), failed_when]);
    let expected = [
        ("enabled\n", 0, true),
        ("enabled-runtime\n", 0, true),
        ("disabled\n", 1, false),
        ("static\n", 0, false),
    ];
    let unexpected = [
        "masked",
        "masked-runtime",
        "linked",
        "linked-runtime",
        "indirect",
        "generated",
        "transient",
        "not-found",
        "",
    ];
    let case = |stdout: &str, rc: i32| {
        let result = json!({"stdout": stdout, "rc": rc, "item": "benchmark"});
        json!({"item": result, "scbench_unit_enablement": result})
    };
    let mut cases = expected.map(|(stdout, rc, _)| case(stdout, rc)).to_vec();
    cases.extend(unexpected.map(|stdout| case(stdout, 1)));
    let rows = evaluate(&expressions, &cases);
    let truth = |value: &Value| value.as_bool().expect("a condition is a real boolean");
    for (row, (state, _, needs_change)) in rows.iter().zip(expected) {
        let (rules, rest) = row.split_at(accepted.len());
        assert!(rules.iter().all(truth), "{state:?}");
        assert_eq!(truth(&rest[0]), needs_change, "{state:?}");
        assert!(!truth(&rest[1]), "{state:?}");
    }
    for (row, state) in rows[expected.len()..].iter().zip(unexpected) {
        let (rules, rest) = row.split_at(accepted.len());
        assert!(!rules.iter().all(truth), "{state:?}");
        assert!(!truth(&rest[0]), "{state:?}");
    }
    let failed = evaluate(
        &[failed_when],
        &[json!({"scbench_unit_enablement": {"rc": 4}})],
    );
    assert!(truth(&failed[0][0]));
}
