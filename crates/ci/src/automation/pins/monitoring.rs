//! The monitoring and database-logging playbooks and the two monitoring
//! units. Their own work runs only through the installed Ops executable, on a
//! host that a signed release has already delivered it to. Nothing connects
//! to a host.

use super::ansible::{asserted, evaluate};
use super::{checkout_file, document};
use serde_json::{Value, json};

const OPS: &str = "/usr/local/libexec/simplestchat-public/simplestchat-ops";
const MONITORING: &str = "ops/ansible/monitoring.yml";
const LOGGING: &str = "ops/ansible/database-logging.yml";
const LOOK: &str = "Look for the installed Ops executable without following links";
const REQUIRE: &str = "Require the Ops executable a signed release delivers";
const INSTRUCTION: &str = concat!(
    "Deliver the signed Ops executable first: run release.yml for a signed ",
    "revision (staging alone is enough), then repeat this playbook.",
);

/// The single play of a playbook.
fn play(path: &str) -> Value {
    let plays = document(path);
    assert_eq!(plays.as_array().unwrap().len(), 1, "{path}");
    plays[0].clone()
}

/// The tasks of one section, each a named mapping.
fn section<'a>(play: &'a Value, name: &str) -> &'a [Value] {
    let tasks = play[name].as_array().unwrap();
    assert!(tasks.iter().all(|task| task["name"].is_string()), "{name}");
    tasks
}

fn named<'a>(tasks: &'a [Value], name: &str) -> &'a Value {
    let mut found = tasks.iter().filter(|task| task["name"] == name);
    let task = found.next().unwrap_or_else(|| panic!("{name}"));
    assert!(found.next().is_none(), "{name}");
    task
}

fn names(tasks: &[Value]) -> Vec<&str> {
    tasks
        .iter()
        .map(|task| task["name"].as_str().unwrap())
        .collect()
}

/// The fixed environment of every Ops command a playbook issues.
fn environment() -> Value {
    json!({"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "GH_TOKEN": "", "GITHUB_TOKEN": ""})
}

/// The look and the requirement both playbooks place before any effect.
fn requirement(pre_tasks: &[Value], register: &str) -> Value {
    let look = named(pre_tasks, LOOK);
    assert_eq!(
        look["ansible.builtin.stat"],
        json!({"path": OPS, "follow": false, "get_checksum": false})
    );
    assert_eq!(look["register"], register);
    let require = named(pre_tasks, REQUIRE);
    assert_eq!(require["ansible.builtin.assert"]["fail_msg"], INSTRUCTION);
    let rules = require["ansible.builtin.assert"]["that"].clone();
    let expected = ["exists", "isreg", "uid == 0", "mode == '0755'"];
    let expected = expected.map(|rule| format!("{register}.stat.{rule}"));
    assert_eq!(rules, json!(expected));
    for task in [look, require] {
        assert!(task.get("when").is_none() && task.get("ignore_errors").is_none());
    }
    rules
}

/// Monitoring's own steps are Ops commands with a fixed environment, placed
/// after the requirement; the only helper modules it still installs belong
/// to the relay step.
#[test]
fn monitoring_runs_its_own_steps_through_the_installed_ops_executable() {
    let play = play(MONITORING);
    let pre_tasks = section(&play, "pre_tasks");
    assert_eq!(
        names(pre_tasks),
        [
            "Require an explicitly selected supported public deployment",
            LOOK,
            REQUIRE,
            "Refuse monitoring setup during unfinished owned host operations",
        ]
    );
    requirement(pre_tasks, "scmon_ops");
    let preflight = &pre_tasks[3];
    assert_eq!(
        preflight["ansible.builtin.command"],
        json!({"argv": [OPS, "release", "preflight"]})
    );
    assert_eq!(preflight["changed_when"], false);
    assert_eq!(preflight["check_mode"], false);
    let tasks = section(&play, "tasks");
    let token = named(
        tasks,
        "Copy only the prepared metrics token into the scrape credential file",
    );
    assert_eq!(
        token["ansible.builtin.command"],
        json!({"argv": [OPS, "monitor", "credential"]})
    );
    assert_eq!(token["no_log"], true);
    assert_eq!(token["when"], "not ansible_check_mode");
    let prepare = named(
        tasks,
        "Back up the database and prepare only the private versioned operations schema",
    );
    assert_eq!(
        prepare["ansible.builtin.command"],
        json!({"argv": [OPS, "monitor", "prepare"]})
    );
    for command in [preflight, token, prepare] {
        assert_eq!(command["environment"], environment(), "{}", command["name"]);
        for softened in ["failed_when", "ignore_errors", "ignore_unreachable"] {
            assert!(command.get(softened).is_none(), "{}", command["name"]);
        }
    }
    let installed = named(
        tasks,
        "Install the reviewed monitoring statements and the relay step's helper modules",
    );
    assert_eq!(
        installed["loop"],
        json!([
            "monitoring-alerts.sql",
            "monitoring-schema.sql",
            "monitoring-external-schema.sql",
            "monitoring-external.sql",
            "monitoring-report.sql",
            "restore_verify.py",
            "restore-verify.sql",
            "turn_public.py",
            "bounded_process.py",
            "runtime_profile.py",
            "release_public.py",
            "release_artifact.py",
            "release_json.py",
        ])
    );
    let text = checkout_file(MONITORING);
    assert!(!text.contains("monitoring_") && !text.contains("release_preflight"));
    // The relay step is the one remaining interpreter call.
    assert_eq!(text.matches("python3").count(), 1);
    let relay = named(
        tasks,
        "Enable native relay metrics through the reviewed relay-only transaction",
    );
    assert!(
        relay["ansible.builtin.command"]["argv"]
            .as_str()
            .unwrap()
            .contains("python3")
    );
}

/// What each step reports as a change is the command's own answer, decided
/// by Ansible.
#[test]
fn monitoring_steps_report_only_the_changes_their_commands_name() {
    let play = play(MONITORING);
    let tasks = section(&play, "tasks");
    let conditions = ["scmon_token", "scmon_schema"].map(|register| {
        let task = tasks.iter().find(|task| task["register"] == register);
        task.unwrap()["changed_when"].as_str().unwrap().to_owned()
    });
    let answer = |text: &str| json!({"stdout": text});
    let answers = |token: &str, schema: &str| {
        json!({
            "scmon_token": answer(token),
            "scmon_schema": answer(schema),
        })
    };
    let cases = [
        answers("changed", "created"),
        answers("retained", "retained"),
        answers("", ""),
    ];
    let conditions = conditions.each_ref().map(String::as_str);
    let verdicts = evaluate(&conditions, &cases);
    assert_eq!(
        verdicts,
        [
            [json!(true), json!(true)],
            [json!(false), json!(false)],
            [json!(false), json!(false)],
        ]
    );
}

/// Both playbooks refuse, with the same instruction, a host whose executable
/// is absent, replaced by a link, not root's, or writable by others.
#[test]
fn a_host_without_the_signed_ops_executable_is_refused_before_any_effect() {
    let monitoring = play(MONITORING);
    let logging = play(LOGGING);
    for (pre_tasks, register) in [
        (section(&monitoring, "pre_tasks"), "scmon_ops"),
        (section(&logging, "pre_tasks"), "scpub_logging_ops"),
    ] {
        let rules = requirement(pre_tasks, register);
        let stat = |value: Value| json!({register: {"stat": value}});
        let delivered =
            json!({"exists": true, "isreg": true, "islnk": false, "uid": 0, "mode": "0755"});
        let with = |key: &str, value: Value| {
            let mut changed = delivered.clone();
            changed[key] = value;
            stat(changed)
        };
        let cases = [
            stat(delivered.clone()),
            stat(json!({"exists": false})),
            stat(json!({"exists": true, "isreg": false, "islnk": true, "uid": 0, "mode": "0777"})),
            stat(json!({"exists": true, "isreg": false, "isdir": true, "uid": 0, "mode": "0755"})),
            with("uid", json!(1000)),
            with("mode", json!("0775")),
            with("mode", json!("4755")),
            json!({}),
        ];
        let verdicts = asserted(&rules, &cases);
        assert_eq!(
            verdicts,
            [true, false, false, false, false, false, false, false],
            "{register}"
        );
    }
}

/// The logging playbook copies nothing to the host and issues one command;
/// it reports the change the command's own outcome names.
#[test]
fn database_logging_installs_nothing_and_runs_one_ops_command() {
    let play = play(LOGGING);
    assert_eq!(play["become"], true);
    assert_eq!(play["serial"], 1);
    let pre_tasks = section(&play, "pre_tasks");
    assert_eq!(
        names(pre_tasks),
        ["Require the canonical prepared public host", LOOK, REQUIRE]
    );
    let tasks = section(&play, "tasks");
    assert_eq!(
        names(tasks),
        ["Apply and verify the two PostgreSQL logging limits under the workload lock"]
    );
    let apply = &tasks[0];
    assert_eq!(
        apply["ansible.builtin.command"],
        json!({"argv": [OPS, "database", "logging", "disable"]})
    );
    assert_eq!(apply["environment"], environment());
    assert_eq!(apply["when"], "not ansible_check_mode");
    assert_eq!(apply["register"], "scpub_logging_result");
    for softened in ["failed_when", "ignore_errors", "ignore_unreachable"] {
        assert!(apply.get(softened).is_none());
    }
    let condition = apply["changed_when"].as_str().unwrap();
    let outcome = |rc: u8, text: &str| json!({"scpub_logging_result": {"rc": rc, "stdout": text}});
    let cases = [
        outcome(0, "{\"passed\":true,\"changed\":true}"),
        outcome(0, "{\"passed\":true,\"changed\":false}"),
        outcome(1, "{\"passed\":false,\"changed\":true}"),
    ];
    let verdicts = evaluate(&[condition], &cases);
    assert_eq!(verdicts, [[json!(true)], [json!(false)], [json!(false)]]);
    let text = checkout_file(LOGGING);
    for absent in [
        "python",
        ".py",
        "ansible.builtin.copy",
        "ansible.builtin.template",
    ] {
        assert!(!text.contains(absent), "{absent}");
    }
}

/// The collector and the importer run the installed executable under their
/// unchanged sandboxes.
#[test]
fn monitoring_units_run_the_installed_ops_executable_in_their_sandboxes() {
    for (unit, action, seconds) in [
        ("monitoring-collector.service.j2", "collect", 55),
        ("monitoring-external.service.j2", "external", 90),
    ] {
        let text = checkout_file(&format!("ops/ansible/templates/{unit}"));
        let lines = text.lines().collect::<Vec<_>>();
        assert!(
            lines.contains(&format!("ExecStart={OPS} monitor {action}").as_str()),
            "{unit}"
        );
        let executions = lines.iter().filter(|line| line.starts_with("Exec"));
        assert_eq!(executions.count(), 1, "{unit}");
        let limit = format!("TimeoutStartSec={seconds}");
        for kept in [
            "Type=oneshot",
            "Requires=docker.service",
            "UMask=0077",
            limit.as_str(),
            "MemoryMax=128M",
            "TasksMax=32",
            "CPUQuota=20%",
            "NoNewPrivileges=true",
            "PrivateTmp=true",
            "ProtectSystem=strict",
            "ProtectHome=true",
            "ReadWritePaths=/var/lib/simplestchat-monitoring",
            "RestrictSUIDSGID=true",
            "LockPersonality=true",
        ] {
            assert!(lines.contains(&kept), "{unit}: {kept}");
        }
        assert!(!text.contains("python") && !text.contains("{{") && !text.contains("{%"));
    }
}

/// From `test_scrape_limit_matches_server_cardinality_budget`: the scrape
/// accepts the server's worst-case finite instrumentation budget.
#[test]
fn scrape_limit_matches_the_servers_cardinality_budget() {
    let template = "ops/ansible/templates/monitoring-prometheus.yml.j2";
    let text = checkout_file(template);
    assert!(!text.contains("{{") && !text.contains("{%"));
    let configuration = document(template);
    let jobs = configuration["scrape_configs"].as_array().unwrap();
    let mut application = jobs.iter().filter(|job| job["job_name"] == "app");
    let limit = application.next().unwrap()["sample_limit"]
        .as_u64()
        .unwrap();
    assert!(application.next().is_none());
    let source = checkout_file("src/signaling/telemetry.rs");
    let declaration = "const SCRAPE_SAMPLE_LIMIT: usize = ";
    let mut declared = source.split(declaration).skip(1);
    let value = declared.next().unwrap().split(';').next().unwrap();
    assert!(declared.next().is_none());
    assert_eq!(value.replace('_', "").parse::<u64>().unwrap(), limit);
}

/// From `test_operational_python_gates_survive_optimization`: an interpreter
/// run with optimization removes `assert` statements, so no embedded check
/// may be one; every fail-closed branch is a bare `raise SystemExit` of one
/// plain message, which no optimization level removes.
#[test]
fn embedded_interpreter_checks_fail_closed_without_assertions() {
    let root = super::ansible::root().join("ops/ansible");
    let mut paths = Vec::new();
    for (directory, extension) in [("", "yml"), ("templates", "j2")] {
        for entry in std::fs::read_dir(root.join(directory)).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|found| found == extension) {
                paths.push(path);
            }
        }
    }
    paths.push(root.join("files/deploy-public.sh"));
    let mut checked = 0;
    for path in paths {
        let text = std::fs::read_to_string(&path).unwrap();
        for line in text.lines().map(str::trim) {
            let assertion = line
                .strip_prefix("assert")
                .is_some_and(|rest| rest.starts_with(char::is_whitespace));
            assert!(
                !assertion,
                "optimization-removable check: {}",
                path.display()
            );
            if !line.contains("raise SystemExit(") {
                continue;
            }
            let message = line.strip_prefix("raise SystemExit('");
            let message = message.and_then(|rest| rest.strip_suffix("')"));
            let message = message.unwrap_or_else(|| panic!("{}: {line}", path.display()));
            let plain = |character: char| {
                character.is_ascii_alphanumeric() || character == ' ' || character == '-'
            };
            assert!(!message.is_empty() && message.chars().all(plain), "{line}");
            checked += 1;
        }
    }
    assert!(checked > 20, "{checked}");
}
