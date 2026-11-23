//! The migration preparation playbook's controller-local verification,
//! read-only inspection, secret handling, certificate transfer and native
//! signed staging, and the executable a move keeps for its data steps and
//! its DNS handoff. The first pins are carried over from the Python
//! migration tests. Each pin reads a playbook offline; the selection guards
//! are evaluated by Ansible's own assert module.

use super::ansible::{asserted, asserted_on};
use super::{document, first, located};
use serde_json::{Map, Value, json};

/// The single play of the migration preparation playbook.
fn play() -> Value {
    document("ops/ansible/migration-prepare.yml")[0].clone()
}

/// A list whose every task is a mapping.
fn tasks(value: &Value) -> &[Value] {
    let tasks = value.as_array().unwrap();
    assert!(tasks.iter().all(Value::is_object));
    tasks
}

/// The block of the first task selected by one migration action.
fn block<'a>(play: &'a Value, action: &str) -> &'a [Value] {
    let selector = format!("scmig_action == '{action}'");
    let selected = first(tasks(&play["tasks"]), |task| {
        task["when"].to_string().contains(&selector)
    });
    tasks(&selected["block"])
}

/// A list whose every item must be text.
fn strings(value: &Value) -> Vec<&str> {
    let items = value.as_array().unwrap().iter();
    items.map(|item| item.as_str().unwrap()).collect()
}

/// Whether a task uses `module`.
fn uses(task: &Value, module: &str) -> bool {
    task.get(module).is_some()
}

/// Both machines are inspected by the executable this move authenticated,
/// run from a private directory before anything is installed: the source
/// stays live, nothing is interpreted, and physical identities are compared
/// before preparation. The directory is on the root filesystem, because a
/// host's `/run` forbids execution.
#[test]
fn inspection_runs_the_authenticated_executable_uninstalled_and_keeps_its_receipt_local() {
    let play = play();
    let all = tasks(&play["tasks"]);
    let (held_at, holding) = located(all, |task| {
        task["name"] == "Hold the executable this move authenticated before it reaches either host"
    });
    assert_eq!(holding["when"], "scmig_action in ['inspect', 'bootstrap']");
    let retained = tasks(&holding["block"]);
    assert_eq!(
        names(retained),
        [
            "Snapshot the authenticated executable the controller retained",
            "Require exactly the executable this move authenticated",
        ]
    );
    let (inspected_at, inspection) = located(all, |task| {
        task["name"] == "Inspect both selected machines without changing services or stored files"
    });
    let (bootstrapped_at, _) = located(all, |task| {
        task["when"]
            .to_string()
            .contains("scmig_action == 'bootstrap'")
    });
    assert!(held_at < inspected_at && held_at < bootstrapped_at);
    assert_eq!(inspection["when"], "scmig_action == 'inspect'");
    let steps = tasks(&inspection["block"]);
    assert_eq!(
        names(steps),
        [
            "Create a private directory for the executable where it can run",
            "Transfer the authenticated executable without installing it",
            "Join the transferred bytes before their first execution",
            "Require the authenticated bytes on the host",
            "Read the physical identity and source configuration or empty target state",
            "Require two distinct successful physical host inspections",
            "Retain only the inspected identities in private controller evidence",
        ]
    );
    let text = inspection.to_string();
    for retired in [
        ".py",
        "python",
        "/usr/local/libexec",
        "ansible.builtin.fetch",
    ] {
        assert!(!text.contains(retired), "{retired}");
    }
    let local = |task: &Value| task["delegate_to"] == "localhost" && task["become"] == false;
    assert!(local(&retained[0]));
    assert_eq!(
        retained[0]["ansible.builtin.stat"]["path"],
        "{{ scmig_ops_executable }}"
    );
    let scratch =
        json!({"state": "directory", "path": "/var/tmp", "prefix": "simplestchat-inspect-"});
    assert_eq!(steps[0]["ansible.builtin.tempfile"], scratch);
    let held = "{{ scmig_inspector.path }}/simplestchat-ops";
    let transfer = json!({"src": "{{ scmig_ops_executable }}", "dest": held, "owner": "root",
        "group": "root", "mode": "0700", "force": false});
    assert_eq!(steps[1]["ansible.builtin.copy"], transfer);
    assert_eq!(steps[2]["ansible.builtin.stat"]["path"], held);
    let command = &steps[4];
    let role = "{{ 'source' if inventory_hostname == 'migration_source' else 'target' }}";
    let origin = concat!(
        "{{ scmig_source_origin if inventory_hostname == 'migration_source' ",
        "else scmig_destination_origin }}"
    );
    assert_eq!(
        strings(&command["ansible.builtin.command"]["argv"]),
        [held, "migrate", "inspect", role, origin]
    );
    let environment = json!({"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "GH_TOKEN": "",
        "GITHUB_TOKEN": ""});
    assert_eq!(command["environment"], environment);
    assert_eq!(
        (&command["changed_when"], &command["timeout"]),
        (&json!(false), &json!(60))
    );
    assert_eq!(command["register"], "scmig_inspected");
    for step in &steps[..6] {
        assert!(step.get("delegate_to").is_none(), "{}", step["name"]);
    }
    let rules = strings(&steps[5]["ansible.builtin.assert"]["that"]).join(" ");
    assert!(rules.contains("machineId !="));
    let receipt = &steps[6];
    assert!(local(receipt));
    assert_eq!(receipt["ansible.builtin.copy"]["mode"], "0600");
    assert_eq!(receipt["ansible.builtin.copy"]["force"], false);
    // Whatever happened, the executable does not stay on either machine.
    let always = tasks(&inspection["always"]);
    let removal = json!({"path": "{{ scmig_inspector.path }}", "state": "absent"});
    assert_eq!(always.len(), 1);
    assert_eq!(always[0]["ansible.builtin.file"], removal);
    assert_eq!(always[0]["when"], "scmig_inspector.path is defined");
    assert_eq!(play["strategy"], "linear");
    assert_eq!(play["any_errors_fatal"], true);
    // The two guards, as Ansible evaluates them.
    let digest = "e".repeat(64);
    let stat = |checksum: &str, change: &dyn Fn(&mut Value)| {
        let mut stat = json!({"isreg": true, "islnk": false, "nlink": 1, "uid": 0,
            "mode": "0700", "checksum": checksum});
        change(&mut stat);
        json!({"stat": stat})
    };
    let evidence = "/private/results/migration.1";
    let executable = format!("{evidence}/ops/verified/simplestchat-ops");
    let controller = |executable: &str, pin: &str, original: Value| {
        json!({"scmig_evidence": evidence, "scmig_ops_executable": executable,
            "scmig_ops_sha256": pin, "scmig_inspector_original": original})
    };
    let cases = [
        (
            controller(&executable, &digest, stat(&digest, &|_| {})),
            true,
        ),
        (
            controller(
                "/private/elsewhere/simplestchat-ops",
                &digest,
                stat(&digest, &|_| {}),
            ),
            false,
        ),
        (controller(&executable, "E", stat("E", &|_| {})), false),
        (
            controller(&executable, &digest, stat(&"f".repeat(64), &|_| {})),
            false,
        ),
        (
            controller(
                &executable,
                &digest,
                stat(&digest, &|stat| stat["islnk"] = json!(true)),
            ),
            false,
        ),
        (
            controller(
                &executable,
                &digest,
                stat(&digest, &|stat| stat["isreg"] = json!(false)),
            ),
            false,
        ),
        (
            controller(
                &executable,
                &digest,
                stat(&digest, &|stat| stat["nlink"] = json!(2)),
            ),
            false,
        ),
    ];
    let variables: Vec<Value> = cases.iter().map(|(case, _)| case.clone()).collect();
    let verdicts = asserted(&retained[1]["ansible.builtin.assert"]["that"], &variables);
    for (accepted, (case, expected)) in verdicts.iter().zip(&cases) {
        assert_eq!(accepted, expected, "{case}");
    }
    let host = |copy: Value| json!({"scmig_ops_sha256": digest, "scmig_inspector_copy": copy});
    let cases = [
        (host(stat(&digest, &|_| {})), true),
        (host(stat(&"f".repeat(64), &|_| {})), false),
        (
            host(stat(&digest, &|stat| stat["uid"] = json!(1000))),
            false,
        ),
        (
            host(stat(&digest, &|stat| stat["mode"] = json!("0755"))),
            false,
        ),
        (
            host(stat(&digest, &|stat| stat["islnk"] = json!(true))),
            false,
        ),
    ];
    let variables: Vec<Value> = cases.iter().map(|(case, _)| case.clone()).collect();
    let verdicts = asserted(&steps[3]["ansible.builtin.assert"]["that"], &variables);
    for (accepted, (case, expected)) in verdicts.iter().zip(&cases) {
        assert_eq!(accepted, expected, "{case}");
    }
}

/// Where a move keeps its authenticated executable on each machine.
const KEPT: &str = "/srv/simplestchat-public/migrations/{{ scmig_run_id }}/simplestchat-ops";

/// Preparation leaves the authenticated executable beside the request on
/// both machines, never replacing bytes that are already there, and joins
/// them before anything runs them. A source keeps its installed release.
#[test]
fn bootstrap_keeps_the_authenticated_executable_beside_the_request_on_both_machines() {
    let play = play();
    let steps = block(&play, "bootstrap");
    let (bound, _) = located(steps, |task| {
        task["name"] == "Bind a new private migration request without replacing prior evidence"
    });
    let kept = &steps[bound + 1];
    assert_eq!(
        kept["name"],
        "Keep the authenticated executable beside the request of this move"
    );
    let copy = json!({"src": "{{ scmig_ops_executable }}", "dest": KEPT, "owner": "root",
        "group": "root", "mode": "0700", "force": false});
    assert_eq!(kept["ansible.builtin.copy"], copy);
    assert!(kept.get("when").is_none() && kept.get("delegate_to").is_none());
    let joined = &steps[bound + 2];
    assert_eq!(
        joined["ansible.builtin.include_tasks"],
        "tasks/migration-ops.yml"
    );
    assert!(joined.get("when").is_none());
    // The shared check, as Ansible evaluates it.
    let check = document("ops/ansible/tasks/migration-ops.yml");
    let check = tasks(&check);
    assert_eq!(check.len(), 2);
    let stat = json!({"path": KEPT, "follow": false, "get_checksum": true,
        "checksum_algorithm": "sha256"});
    assert_eq!(check[0]["ansible.builtin.stat"], stat);
    assert_eq!(check[0]["register"], "scmig_ops_kept");
    let digest = "e".repeat(64);
    let case = |pin: &str, change: &dyn Fn(&mut Value)| {
        let mut stat = json!({"isreg": true, "islnk": false, "nlink": 1, "uid": 0,
            "mode": "0700", "checksum": digest});
        change(&mut stat);
        json!({"scmig_ops_sha256": pin, "scmig_ops_kept": {"stat": stat}})
    };
    let cases = [
        (case(&digest, &|_| {}), true),
        (case("E", &|stat| stat["checksum"] = json!("E")), false),
        (
            case(&digest, &|stat| stat["checksum"] = json!("f".repeat(64))),
            false,
        ),
        (case(&digest, &|stat| stat["uid"] = json!(1000)), false),
        (case(&digest, &|stat| stat["mode"] = json!("0755")), false),
        (case(&digest, &|stat| stat["nlink"] = json!(2)), false),
        (case(&digest, &|stat| stat["islnk"] = json!(true)), false),
        (case(&digest, &|stat| stat["isreg"] = json!(false)), false),
    ];
    let variables: Vec<Value> = cases.iter().map(|(case, _)| case.clone()).collect();
    let verdicts = asserted(&check[1]["ansible.builtin.assert"]["that"], &variables);
    for (accepted, (case, expected)) in verdicts.iter().zip(&cases) {
        assert_eq!(accepted, expected, "{case}");
    }
}

/// The bootstrap checks of a move, in their order, each changing nothing.
const BOOTSTRAP_CHECKS: [&str; 10] = [
    "Inspect the existing bootstrap paths without following links",
    "Refuse unsafe existing bootstrap paths",
    "Inspect the workload lock without following links",
    "Refuse an unsafe workload lock",
    "Refuse a held workload lock",
    "Inspect the owned operation journals without following links",
    "Read each small private owned operation journal",
    "Refuse unfinished owned work",
    "Look for an earlier operation of this run without following links",
    "Refuse an existing operation of this run",
];

/// Bootstrap refuses unsafe paths and unfinished owned work before it
/// writes anything: its first steps only look, take a free workload lock
/// and release it at once, and assert. It creates the root-owned directory a
/// release later delivers the Ops executable into and copies nothing there:
/// every step of a move runs the executable it keeps beside its request,
/// and no step names a Python file or runs an interpreter.
#[test]
fn bootstrap_refuses_unfinished_work_and_installs_no_helper() {
    let play = play();
    let steps = block(&play, "bootstrap");
    assert_eq!(names(&steps[..BOOTSTRAP_CHECKS.len()]), BOOTSTRAP_CHECKS);
    assert_eq!(
        steps[BOOTSTRAP_CHECKS.len()]["name"],
        "Create private migration and immutable release storage"
    );
    for step in &steps[..BOOTSTRAP_CHECKS.len()] {
        let looks = [
            "ansible.builtin.stat",
            "ansible.builtin.slurp",
            "ansible.builtin.assert",
        ];
        let command = step.get("ansible.builtin.command");
        assert!(looks.iter().any(|module| uses(step, module)) || command.is_some());
        if command.is_some() {
            assert_eq!(step["changed_when"], false, "{}", step["name"]);
        }
        for softened in ["ignore_errors", "failed_when", "delegate_to"] {
            assert!(step.get(softened).is_none(), "{} {softened}", step["name"]);
        }
    }
    let (_, flock) = located(steps, |task| task["name"] == BOOTSTRAP_CHECKS[4]);
    assert_eq!(
        flock["ansible.builtin.command"],
        json!({"argv": ["/usr/bin/flock", "--exclusive", "--nonblock",
            "/run/simplestchat-bench/workload.lock", "/bin/true"]})
    );
    assert_eq!(flock["when"], "scmig_workload_lock.stat.exists");
    let (_, directory) = located(steps, |task| {
        task["name"] == "Create the protected Ops executable directory"
    });
    let protected = json!({"path": "/usr/local/libexec/simplestchat-public",
        "state": "directory", "owner": "root", "group": "root", "mode": "0755"});
    assert_eq!(directory["ansible.builtin.file"], protected);
    for task in steps {
        let destination = task["ansible.builtin.copy"]["dest"].as_str();
        let installed = destination.is_some_and(|path| path.starts_with("/usr/local/libexec"));
        assert!(!installed, "{}", task["name"]);
    }
    let text = Value::from(steps.to_vec()).to_string();
    assert!(!text.contains(".py") && !text.contains("python"));
}

/// Every data step of a move runs the executable the move keeps, with the
/// step and the request it names, after its bytes were joined again on that
/// machine. Nothing is interpreted, and the bounded units stay as they were.
#[test]
fn every_data_step_runs_the_executable_the_move_keeps() {
    let plays = document("ops/ansible/migration-data.yml");
    let plays = plays.as_array().unwrap();
    let request = "/srv/simplestchat-public/migrations/{{ scmig_run_id }}/request.json";
    let text = Value::from(plays.clone()).to_string();
    for retired in [".py", "python", "/usr/local/libexec"] {
        assert!(!text.contains(retired), "{retired}");
    }
    let mut steps = Vec::new();
    for play in &plays[1..] {
        let tasks = tasks(&play["tasks"]);
        let mut joined: Option<&Value> = None;
        for task in tasks {
            if task.get("ansible.builtin.include_tasks").is_some() {
                assert_eq!(
                    task["ansible.builtin.include_tasks"],
                    "tasks/migration-ops.yml"
                );
                joined = Some(&task["when"]);
                continue;
            }
            let Some(argv) = task["ansible.builtin.command"]["argv"].as_array() else {
                continue;
            };
            // Another command of the play may count in numbers.
            let word = |word: &Value| {
                word.as_str()
                    .map_or_else(|| word.to_string(), str::to_owned)
            };
            let argv: Vec<String> = argv.iter().map(word).collect();
            let Some(at) = argv.iter().position(|word| word == KEPT) else {
                continue;
            };
            assert_eq!(argv[at + 1], "migrate");
            assert_eq!(argv[at + 3..], ["--request", request]);
            // The check came first in this play, for at least this action.
            let condition = task["when"].as_str().unwrap();
            let checked = joined.unwrap_or_else(|| panic!("{}", task["name"]));
            let actions = condition.split('\'').skip(1).step_by(2);
            for action in actions {
                assert!(checked.to_string().contains(action), "{}", task["name"]);
            }
            let bounded = argv[0] == "systemd-run";
            steps.push((
                play["hosts"].as_str().unwrap(),
                argv[at + 2].clone(),
                bounded,
            ));
        }
    }
    let on = |hosts: &'static str, step: &str, bounded: bool| (hosts, step.to_owned(), bounded);
    let either = "{{ 'recover-source' if scmig_action == 'resume-source' else 'finalize-source' }}";
    assert_eq!(
        steps,
        [
            on("migration_target", "verify-target", false),
            on("migration_source", "freeze-source", true),
            on("migration_source", "seal-source", false),
            on("migration_target", "restore-target", true),
            on("migration_source", either, true),
        ]
    );
}

/// From `test_selection_guard_refuses_check_mode_extra_hosts_and_bad_operation`:
/// the first guard, evaluated by Ansible on the hosts a run selects, admits
/// the controller's two hosts with well-formed identities, and refuses a
/// check run, a third or a missing host, another host's name, an unknown
/// action and each malformed identity, origin, evidence path or deployment.
#[test]
fn selection_guard_refuses_check_mode_extra_hosts_and_bad_operation() {
    let play = play();
    let guard = &tasks(&play["pre_tasks"])[0];
    assert_eq!(
        guard["name"],
        "Require the controller's exact two-host migration selection"
    );
    let rules = &guard["ansible.builtin.assert"]["that"];
    let baseline = json!({
        "scmig_action": "inspect",
        "scmig_run_id": "a".repeat(32),
        "scmig_revision": "b".repeat(40),
        "scmig_source_origin": "https://chat.example.test",
        "scmig_destination_origin": "https://next.chat.example.test",
        "scmig_evidence": "/private/controller/evidence",
        "scpub_enabled": true,
        "scpub_config": "/etc/simplestchat-public",
        "scpub_root": "/srv/simplestchat-public",
    });
    let with = |key: &str, value: Value| {
        let mut case = baseline.clone();
        case[key] = value;
        case
    };
    let mut cases = vec![
        ("the baseline", baseline.clone(), true),
        // A move may keep its origin.
        (
            "one origin",
            with(
                "scmig_destination_origin",
                json!("https://chat.example.test"),
            ),
            true,
        ),
    ];
    for (key, value) in [
        ("inventory_hostname", json!("other")),
        ("scmig_action", json!("deploy")),
        ("scmig_run_id", json!("../elsewhere")),
        ("scmig_revision", json!("main")),
        ("scmig_source_origin", json!("http://chat.example.test")),
        (
            "scmig_destination_origin",
            json!("https://next.example.test/x"),
        ),
        ("scmig_evidence", json!("private/controller/evidence")),
        ("scpub_enabled", json!(false)),
        ("scpub_config", json!("/etc/simplestchat-other")),
        ("scpub_root", json!("/srv/simplestchat-other")),
    ] {
        cases.push((key, with(key, value), false));
    }
    let variables: Vec<Value> = cases.iter().map(|(_, case, _)| case.clone()).collect();
    let both = ["migration_source", "migration_target"];
    let verdicts = asserted_on(&both, &[], rules, &variables);
    for (host, verdicts) in both.iter().zip(&verdicts) {
        for (accepted, (label, _, expected)) in verdicts.iter().zip(&cases) {
            assert_eq!(accepted, expected, "{host}: {label}");
        }
    }
    // The check mode and the play's hosts are Ansible's own, whatever a
    // case says.
    let baseline = [baseline];
    let third = ["migration_source", "migration_target", "other"];
    for (hosts, options) in [
        (&both[..], &["--check"][..]),
        (&third[..], &[][..]),
        (&["migration_target"][..], &[][..]),
    ] {
        let verdicts = asserted_on(hosts, options, rules, &baseline);
        assert_eq!(verdicts.len(), hosts.len());
        for (host, verdict) in hosts.iter().zip(&verdicts) {
            assert_eq!(verdict, &[false], "{host} of {hosts:?} with {options:?}");
        }
    }
}

/// From `test_secret_copy_requires_exact_digest_without_owner_or_secret_logging`:
/// credentials stay private, and an existing differing destination is never
/// overwritten.
#[test]
fn secret_copy_requires_exact_digest_without_owner_or_secret_logging() {
    let play = play();
    let tasks = block(&play, "bootstrap");
    let fetch = first(tasks, |task| uses(task, "ansible.builtin.fetch"));
    assert_eq!(fetch["no_log"], true);
    let secrets = "/etc/simplestchat-public/secrets.json";
    let copies = tasks
        .iter()
        .enumerate()
        .filter(|(_, task)| task["ansible.builtin.copy"]["dest"] == secrets)
        .collect::<Vec<_>>();
    let [(copied, target)] = copies[..] else {
        panic!("exactly one task copies the secrets");
    };
    assert_eq!(target["when"], "inventory_hostname == 'migration_target'");
    assert_eq!(target["no_log"], true);
    assert_eq!(target["ansible.builtin.copy"]["force"], false);
    assert_eq!(target["ansible.builtin.copy"]["mode"], "0600");
    assert!(
        !Value::from(tasks.to_vec())
            .to_string()
            .contains("owner.json")
    );
    let guards = tasks
        .iter()
        .enumerate()
        .filter(|(_, task)| uses(task, "ansible.builtin.assert"))
        .filter(|(_, task)| task.to_string().contains("checksum"))
        .collect::<Vec<_>>();
    assert_eq!(guards.len(), 3);
    assert!(guards.iter().all(|(_, task)| task["no_log"] == true));
    assert!(guards[1].0 < copied);
    assert!(guards[2].0 > copied);
}

/// From `test_same_origin_tls_transfer_is_private_and_precedes_staging`: the
/// exact hostname subtree is validated on both hosts without logging its
/// payload.
#[test]
fn same_origin_tls_transfer_is_private_and_precedes_staging() {
    let play = play();
    let tasks = tasks(&play["tasks"]);
    let (transferred, tls) = located(tasks, |task| {
        task["name"] == "Preserve the unchanged hostname certificate before DNS cutover"
    });
    assert_eq!(
        tls["when"],
        json!([
            "scmig_action == 'bootstrap'",
            "scmig_source_origin == scmig_destination_origin"
        ])
    );
    let steps = self::tasks(&tls["block"]);
    assert!(steps.iter().all(|step| step["no_log"] == true));
    let last = &steps[steps.len() - 1];
    assert_eq!(steps[0]["when"], "inventory_hostname == 'migration_source'");
    assert_eq!(last["when"], "inventory_hostname == 'migration_target'");
    for (step, action) in [(&steps[0], "export"), (last, "import")] {
        assert_eq!(
            strings(&step["ansible.builtin.command"]["argv"]),
            [KEPT, "migrate", "tls", action, "{{ scmig_run_id }}"]
        );
    }
    let copy = first(steps, |step| uses(step, "ansible.builtin.copy"));
    assert_eq!(copy["ansible.builtin.copy"]["force"], false);
    assert_eq!(copy["ansible.builtin.copy"]["mode"], "0600");
    let (staged, stage) = located(tasks, |task| {
        task["name"] == "Transfer and stage the exact verified CI artifact only on the destination"
    });
    assert!(transferred < staged);
    // Only a signed release is staged; no other route selects this step.
    let only = [
        "scmig_action == 'stage'",
        "inventory_hostname == 'migration_target'",
    ];
    assert_eq!(stage["when"], json!(only));
    let staging = tasks
        .iter()
        .filter(|task| task["when"].to_string().contains("'stage'"));
    assert_eq!(staging.count(), 1);
}

/// The bridge for cached name answers is started by the executable the move
/// keeps on its sealed source, after its bytes were joined again there, with
/// exactly the five values the guard admitted. Nothing is interpreted.
#[test]
fn the_dns_handoff_runs_the_executable_the_move_keeps() {
    let plays = document("ops/ansible/dns-handoff.yml");
    let plays = plays.as_array().unwrap();
    assert_eq!(plays.len(), 1);
    let play = &plays[0];
    let text = play.to_string();
    for retired in [".py", "python", "/usr/local/libexec", "import "] {
        assert!(!text.contains(retired), "{retired}");
    }
    assert_eq!(play["any_errors_fatal"], true);
    let guard = tasks(&play["pre_tasks"]);
    assert_eq!(guard.len(), 1);
    let rules = strings(&guard[0]["ansible.builtin.assert"]["that"]);
    for rule in [
        "not ansible_check_mode",
        "ansible_play_hosts_all | length == 1",
        "ansible_host == schandoff_source_ip",
        "schandoff_source_ip != schandoff_target_ip",
        "scmig_run_id is match('^[a-f0-9]{32}$')",
        "scmig_ops_sha256 is match('^[a-f0-9]{64}$')",
        "schandoff_ttl_seconds | int == 7200",
    ] {
        assert!(rules.contains(&rule), "{rule}");
    }
    let steps = tasks(&play["tasks"]);
    assert_eq!(steps.len(), 2);
    assert_eq!(
        steps[0]["ansible.builtin.include_tasks"],
        "tasks/migration-ops.yml"
    );
    assert!(steps[0].get("when").is_none());
    let command = &steps[1];
    assert_eq!(
        strings(&command["ansible.builtin.command"]["argv"]),
        [
            KEPT,
            "migrate",
            "handoff",
            "{{ scmig_run_id }}",
            "{{ schandoff_source_ip }}",
            "{{ schandoff_target_ip }}",
            "{{ schandoff_domain }}",
            "{{ schandoff_ttl_seconds }}",
        ]
    );
    assert_eq!(command["changed_when"], true);
    assert!(command.get("when").is_none());
}

/// The names of a block's tasks, in order.
fn names(tasks: &[Value]) -> Vec<&str> {
    tasks
        .iter()
        .map(|task| task["name"].as_str().unwrap())
        .collect()
}

/// Typed selections a signed move passes to every play. Fictional values.
fn selection() -> Map<String, Value> {
    let revision = "a".repeat(40);
    let values = json!({
        "scmig_revision": revision,
        "scpub_release_repository": "swiftraccoon/simplestChat",
        "scpub_release_artifact_id": 1234,
        "scpub_release_ci_run": 5678,
        "scpub_release_expected_revision": revision,
        "scpub_release_revision": revision,
        "scpub_release_verified_directory": "/private/results/verify.1/artifact",
        "scpub_ops_artifact_id": 4321,
        "scpub_ops_selection": {"repository": "swiftraccoon/simplestChat",
            "revision": revision, "packageVersion": "0.1.0", "protocolVersion": 1,
            "target": "x86_64-unknown-linux-musl", "buildRunId": 5678, "runAttempt": 2},
        "scpub_ops_release_controller": "/private/controller/simplestchat-release",
        "scpub_ops_gh_executable": "/private/tools/gh",
        "ansible_host": "192.0.2.11",
        "ansible_user": "root",
        "ansible_ssh_private_key_file": "/private/controller key",
    });
    values.as_object().unwrap().clone()
}

/// The baseline with one variable replaced, or removed when `value` is null.
fn changed(key: &str, value: Value) -> Value {
    let mut values = selection();
    match value {
        Value::Null => drop(values.remove(key)),
        value => drop(values.insert(key.into(), value)),
    }
    Value::Object(values)
}

/// A selection whose Ops half differs in one field, or lacks or adds one.
fn other_ops(key: &str, value: Value) -> Value {
    let mut ops = selection()["scpub_ops_selection"].clone();
    match value {
        Value::Null => drop(ops.as_object_mut().unwrap().remove(key)),
        value => ops[key] = value,
    }
    changed("scpub_ops_selection", ops)
}

/// What the controller's own verification prints, as Ansible registers it.
fn verified(change: impl Fn(&mut Value)) -> Value {
    let mut summary = json!({"directory": "/private/results/verify.1/artifact",
        "image.tar": "c".repeat(64), "release.json": "d".repeat(64),
        "revision": "a".repeat(40)});
    change(&mut summary);
    json!({"rc": 0, "stdout": summary.to_string()})
}

/// The signed application is verified by the controller alone, before any
/// task that contacts a host, and only its identity is retained.
#[test]
fn verification_runs_on_the_controller_before_any_host_and_retains_its_record() {
    let play = play();
    let step = &tasks(&play["tasks"])[0];
    let name = "Verify the signed application on the controller before either host is contacted";
    assert_eq!(step["name"], name);
    let only = [
        "scmig_action == 'verify'",
        "inventory_hostname == 'migration_source'",
    ];
    assert_eq!(step["when"], json!(only));
    assert_eq!(
        (&step["delegate_to"], &step["become"]),
        (&json!("localhost"), &json!(false))
    );
    let guard = &tasks(&play["pre_tasks"])[0]["ansible.builtin.assert"]["that"];
    let actions = "scmig_action in ['verify', 'inspect', 'bootstrap', 'stage']";
    assert!(strings(guard).contains(&actions));
    let steps = tasks(&step["block"]);
    let modules = [
        "assert", "command", "assert", "copy", "set_fact", "copy", "command", "command", "stat",
        "assert", "copy",
    ]
    .map(|module| format!("ansible.builtin.{module}"));
    assert_eq!(steps.len(), modules.len());
    for (index, (step, module)) in steps.iter().zip(&modules).enumerate() {
        assert!(uses(step, module), "{module}");
        // Only the token is read conditionally: when the caller supplied none.
        assert!(step.get("delegate_to").is_none(), "{module}");
        assert_eq!(step.get("when").is_some(), index == 6, "{module}");
    }
    let command = &steps[1];
    assert_eq!(
        strings(&command["ansible.builtin.command"]["argv"]),
        [
            "{{ scpub_ops_release_controller }}",
            "verify",
            "--repository",
            "{{ scpub_release_repository }}",
            "--artifact-id",
            "{{ scpub_release_artifact_id }}",
            "--revision",
            "{{ scmig_revision }}",
            "--ci-run",
            "{{ scpub_release_ci_run }}",
            "--artifact-dir",
            "{{ scpub_release_verified_directory | default('') }}",
            "--output-parent",
            "{{ (playbook_dir ~ '/../../results') | realpath }}",
            "--timeout-seconds",
            "600",
            "--finalization-seconds",
            "60",
        ]
    );
    let path = "{{ scpub_ops_gh_executable | dirname }}:/usr/bin:/bin:/usr/sbin:/sbin";
    assert_eq!(command["environment"], json!({"PATH": path}));
    assert_eq!(
        (&command["changed_when"], &command["timeout"]),
        (&json!(false), &json!(660))
    );
    assert_eq!(command["register"], "scmig_verified_application");
    let record = &steps[3]["ansible.builtin.copy"];
    let kept = json!({"content": "{{ scmig_verified_application.stdout | from_json | to_json }}",
        "dest": "{{ scmig_evidence }}/application.json", "mode": "0600", "force": false});
    assert_eq!(record, &kept);
    // The selectors: exact integers, this repository, absolute trusted tools.
    let mut cases = vec![(Value::Object(selection()), true)];
    for (key, value) in [
        ("scpub_release_repository", json!("example/simplestChat")),
        ("scpub_release_artifact_id", json!("1234")),
        ("scpub_release_artifact_id", json!(0)),
        ("scpub_release_ci_run", json!(9007199254740992u64)),
        ("scpub_release_expected_revision", json!("b".repeat(40))),
        (
            "scpub_ops_release_controller",
            json!("simplestchat-release"),
        ),
        ("scpub_ops_release_controller", Value::Null),
        ("scpub_ops_gh_executable", json!("/private/tools/hub")),
        ("scpub_ops_gh_executable", json!("/private/tools:/other/gh")),
        ("scpub_ops_gh_executable", json!("gh")),
    ] {
        cases.push((changed(key, value), false));
    }
    let variables: Vec<Value> = cases.iter().map(|(case, _)| case.clone()).collect();
    let verdicts = asserted(&steps[0]["ansible.builtin.assert"]["that"], &variables);
    for (accepted, (case, expected)) in verdicts.iter().zip(&cases) {
        assert_eq!(accepted, expected, "{case}");
    }
    // The result: exactly the four fields, for exactly this revision.
    let parsed = steps[2]["vars"]["scmig_application"].clone();
    let result = |registered: Value| {
        let mut values = selection();
        values.insert("scmig_verified_application".into(), registered);
        values.insert("scmig_application".into(), parsed.clone());
        Value::Object(values)
    };
    let cases = [
        (verified(|_| {}), true),
        (
            verified(|summary| summary["revision"] = json!("b".repeat(40))),
            false,
        ),
        (
            verified(|summary| summary["directory"] = json!("results/verify.1")),
            false,
        ),
        (
            verified(|summary| summary["image.tar"] = json!("c".repeat(63))),
            false,
        ),
        (
            verified(|summary| summary["release.json"] = json!("D".repeat(64))),
            false,
        ),
        (verified(|summary| summary["extra"] = json!(true)), false),
        (
            verified(|summary| drop(summary.as_object_mut().unwrap().remove("image.tar"))),
            false,
        ),
        (
            json!({"rc": 1, "stdout": verified(|_| {})["stdout"]}),
            false,
        ),
        (json!({"rc": 0, "stdout": "[]"}), false),
    ];
    let variables: Vec<Value> = cases.iter().map(|(case, _)| result(case.clone())).collect();
    let verdicts = asserted(&steps[2]["ansible.builtin.assert"]["that"], &variables);
    for (accepted, (case, expected)) in verdicts.iter().zip(&cases) {
        assert_eq!(accepted, expected, "{case}");
    }
}

/// A fresh destination is staged as a routine release stages a prepared one,
/// without the prepared-host preflight: the originals are verified again,
/// the signed Ops executable is delivered, and only then does the host fetch
/// and import the release through it. Nothing is built, started or
/// interpreted.
#[test]
fn stage_reverifies_delivers_the_signed_executable_then_fetches_and_imports_natively() {
    let play = play();
    let tasks = block(&play, "stage");
    assert_eq!(
        names(tasks),
        [
            "Require exact signed-release selectors and direct key-based SSH",
            "Verify the retained signed originals again through the native controller",
            "Require the completed native application verification result",
            "Use the completed native controller verification result",
            "Snapshot the controller-verified application predicate",
            "Snapshot the controller-verified application manifest",
            "Require the verified application originals before any Ops delivery",
            "Read the controller-verified application predicate",
            "Deliver the signed current Ops executable through native policy",
            "Create private immutable storage for the selected release",
            "Verify the exact GitHub release and fetch it directly onto the destination",
            "Import the verified production image without launching the app",
            "Copy only the original verified predicate without replacing existing evidence",
            "Recheck the retained predicate and the staged manifest",
            "Require the staged release to hold the exact signed originals",
        ]
    );
    let text = Value::from(tasks.to_vec()).to_string();
    for retired in [".py", "python", "release-public", "build/"] {
        assert!(!text.contains(retired), "{retired}");
    }
    let local = |task: &Value| task["delegate_to"] == "localhost" && task["become"] == false;
    let directory = "{{ scpub_release_verified_directory }}";
    let verify = strings(&tasks[1]["ansible.builtin.command"]["argv"]);
    assert_eq!(
        verify[..2],
        ["{{ scpub_ops_release_controller }}", "verify"]
    );
    let at = verify
        .iter()
        .position(|word| *word == "--artifact-dir")
        .unwrap();
    assert_eq!(verify[at + 1], directory);
    assert!(local(&tasks[1]) && tasks[1]["changed_when"] == false);
    assert_eq!(tasks[1]["register"], "scpub_native_validated_release");
    // The delivery's inputs are the registered originals, all read locally.
    let inputs = [
        (4, "scpub_app_predicate_file"),
        (5, "scpub_app_manifest_file"),
        (7, "scpub_app_predicate_bytes"),
    ];
    for (index, register) in inputs {
        assert!(local(&tasks[index]), "{register}");
        assert_eq!(tasks[index]["register"], register);
    }
    assert_eq!(tasks[7]["no_log"], true);
    assert_eq!(
        tasks[8]["ansible.builtin.import_tasks"],
        "tasks/signed-ops.yml"
    );
    let fetch = strings(&tasks[10]["ansible.builtin.command"]["argv"]);
    assert_eq!(fetch[..2], ["{{ scpub_ops_release_controller }}", "fetch"]);
    for (flag, value) in [
        ("--revision", "{{ scmig_revision }}"),
        ("--artifact-id", "{{ scpub_release_artifact_id }}"),
        ("--ci-run", "{{ scpub_release_ci_run }}"),
        ("--host", "{{ ansible_host }}"),
        ("--identity", "{{ ansible_ssh_private_key_file }}"),
        ("--verified-directory", directory),
        ("--timeout-seconds", "1020"),
    ] {
        let at = fetch.iter().position(|word| *word == flag).unwrap();
        assert_eq!(fetch[at + 1], value, "{flag}");
    }
    assert!(local(&tasks[10]));
    assert_eq!(tasks[10]["timeout"], 1080);
    let stage = &tasks[11];
    assert_eq!(
        strings(&stage["ansible.builtin.command"]["argv"]),
        [
            "systemd-run",
            "--unit=simplestchat-migration-stage-{{ scmig_run_id }}",
            "--property=Type=exec",
            "--property=RuntimeMaxSec=600",
            "--property=TimeoutStopSec=60",
            "--wait",
            "--collect",
            "/usr/local/libexec/simplestchat-public/simplestchat-ops",
            "release",
            "stage",
            "--revision",
            "{{ scmig_revision }}",
            "--timeout-seconds",
            "600",
            "--finalization-seconds",
            "60",
        ]
    );
    let environment = json!({"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "GH_TOKEN": "",
        "GITHUB_TOKEN": ""});
    assert_eq!(stage["environment"], environment);
    assert!(stage.get("delegate_to").is_none());
    let predicate = &tasks[12]["ansible.builtin.copy"];
    assert_eq!(
        (&predicate["force"], &predicate["mode"]),
        (&json!(false), &json!("0600"))
    );
    let retained = "{{ scpub_root }}/releases/{{ scmig_revision }}/release-predicate.json";
    assert_eq!(predicate["dest"], retained);
    // The selectors, as Ansible evaluates them.
    let mut cases = vec![
        (Value::Object(selection()), true),
        (
            changed("scpub_ops_artifact_zip", json!("/private/ops/artifact.zip")),
            true,
        ),
        (changed("ansible_port", json!(2222)), true),
    ];
    for (key, value) in [
        ("scpub_release_repository", json!("example/simplestChat")),
        ("scpub_release_artifact_id", json!("1234")),
        ("scpub_release_ci_run", json!(0)),
        ("scpub_release_expected_revision", json!("b".repeat(40))),
        ("scpub_release_revision", json!("b".repeat(40))),
        (
            "scpub_release_verified_directory",
            json!("results/verify.1/artifact"),
        ),
        ("scpub_release_verified_directory", Value::Null),
        ("scpub_ops_artifact_id", Value::Null),
        ("scpub_ops_artifact_id", json!(true)),
        ("scpub_ops_selection", Value::Null),
        (
            "scpub_ops_release_controller",
            json!("simplestchat-release"),
        ),
        ("scpub_ops_gh_executable", json!("/private/tools/hub")),
        ("scpub_ops_artifact_zip", json!("artifact.zip")),
        ("ansible_host", json!("-oProxyCommand=id")),
        ("ansible_user", json!("")),
        ("ansible_ssh_private_key_file", json!("~/.ssh/id_ed25519")),
        ("ansible_port", json!(65536)),
        ("ansible_password", json!("")),
        ("ansible_become_pass", json!("")),
        ("ansible_ssh_common_args", json!("-J jump.example.test")),
    ] {
        cases.push((changed(key, value), false));
    }
    for (key, value) in [
        ("repository", json!("example/simplestChat")),
        ("revision", json!("b".repeat(40))),
        ("packageVersion", json!("0.2.0")),
        ("protocolVersion", json!(2)),
        ("target", json!("aarch64-unknown-linux-musl")),
        ("buildRunId", json!(5679)),
        ("runAttempt", json!(0)),
        ("runAttempt", Value::Null),
        ("signature", json!("unreviewed")),
    ] {
        cases.push((other_ops(key, value), false));
    }
    let variables: Vec<Value> = cases.iter().map(|(case, _)| case.clone()).collect();
    let verdicts = asserted(&tasks[0]["ansible.builtin.assert"]["that"], &variables);
    for (accepted, (case, expected)) in verdicts.iter().zip(&cases) {
        assert_eq!(accepted, expected, "{case}");
    }
    // The second verification must name the very directory the first did.
    let parsed = tasks[2]["vars"]["scpub_native_verified_release"].clone();
    let result = |registered: Value| {
        let mut values = selection();
        values.insert("scpub_native_validated_release".into(), registered);
        values.insert("scpub_native_verified_release".into(), parsed.clone());
        Value::Object(values)
    };
    let cases = [
        (verified(|_| {}), true),
        (
            verified(|summary| summary["directory"] = json!("/private/results/verify.2/artifact")),
            false,
        ),
        (
            verified(|summary| summary["revision"] = json!("b".repeat(40))),
            false,
        ),
        (
            verified(|summary| summary["release.json"] = json!("d".repeat(63))),
            false,
        ),
        (
            json!({"rc": 2, "stdout": verified(|_| {})["stdout"]}),
            false,
        ),
    ];
    let variables: Vec<Value> = cases.iter().map(|(case, _)| result(case.clone())).collect();
    let verdicts = asserted(&tasks[2]["ansible.builtin.assert"]["that"], &variables);
    for (accepted, (case, expected)) in verdicts.iter().zip(&cases) {
        assert_eq!(accepted, expected, "{case}");
    }
}

/// The executable the inspection runs is authenticated on the controller in
/// the same host-free step: one pinned request, the token only in the child's
/// environment, and an executable that must hash to the digest the signed
/// attestation's policy names.
#[test]
fn the_ops_executable_is_authenticated_on_the_controller_before_any_host() {
    let play = play();
    let step = &tasks(&play["tasks"])[0];
    let steps = tasks(&step["block"]);
    assert_eq!(
        names(&steps[4..]),
        [
            "Serialize one original Ops acquisition request",
            "Retain the original acquisition request privately",
            "Read the user's gh token only when GH_TOKEN was not supplied",
            "Authenticate the signed Ops executable on the controller",
            "Snapshot the authenticated executable",
            "Require the executable the verifier's signed policy pins",
            "Retain the authenticated executable's identity in private controller evidence",
        ]
    );
    let request = steps[4]["ansible.builtin.set_fact"]["scmig_ops_request"]
        .as_str()
        .unwrap();
    for part in [
        "'schemaVersion': 1",
        "'selection': scpub_ops_selection",
        "'artifactId': scpub_ops_artifact_id",
        "'output': scmig_evidence ~ '/ops'",
        "'authentication': 'gh-token'",
        "{'artifactZip': scpub_ops_artifact_zip}",
        "| to_json",
    ] {
        assert!(request.contains(part), "{part}");
    }
    let retained = json!({"content": "{{ scmig_ops_request }}",
        "dest": "{{ scmig_evidence }}/ops-request.json", "mode": "0600", "force": false});
    assert_eq!(steps[5]["ansible.builtin.copy"], retained);
    let token = &steps[6];
    assert_eq!(
        token["when"],
        "lookup('ansible.builtin.env', 'GH_TOKEN') | length == 0"
    );
    assert_eq!(token["no_log"], true);
    let acquire = &steps[7];
    assert_eq!(
        strings(&acquire["ansible.builtin.command"]["argv"]),
        [
            "{{ scpub_ops_release_controller }}",
            "ops-artifact",
            "acquire",
            "--request",
            "{{ scmig_evidence }}/ops-request.json",
            "--request-sha256",
            "{{ scmig_ops_request | hash('sha256') }}",
        ]
    );
    let environment = acquire["environment"].as_object().unwrap();
    let keys: Vec<&str> = environment.keys().map(String::as_str).collect();
    assert_eq!(keys, ["GH_TOKEN", "GITHUB_TOKEN", "PATH"]);
    assert_eq!(environment["GITHUB_TOKEN"], "");
    let path = "{{ scpub_ops_gh_executable | realpath | dirname }}:/usr/bin:/bin";
    assert_eq!(environment["PATH"], path);
    assert_eq!(
        (&acquire["no_log"], &acquire["changed_when"]),
        (&json!(true), &json!(false))
    );
    assert_eq!(acquire["register"], "scmig_ops_acquired");
    let held = "{{ scmig_evidence }}/ops/verified/simplestchat-ops";
    assert_eq!(steps[8]["ansible.builtin.stat"]["path"], held);
    assert_eq!(steps[8]["ansible.builtin.stat"]["follow"], false);
    let record = &steps[10]["ansible.builtin.copy"];
    assert_eq!(record["dest"], "{{ scmig_evidence }}/ops.json");
    assert_eq!(
        (&record["mode"], &record["force"]),
        (&json!("0600"), &json!(false))
    );
    for field in [
        "'path': scmig_evidence ~ '/ops/verified/simplestchat-ops'",
        "'sha256'",
        "'bytes'",
    ] {
        assert!(
            record["content"].as_str().unwrap().contains(field),
            "{field}"
        );
    }
    let always = tasks(&step["always"]);
    assert_eq!(always.len(), 1);
    assert_eq!(
        always[0]["ansible.builtin.set_fact"]["scmig_ops_credential"],
        json!({})
    );
    assert_eq!(always[0]["no_log"], true);
    // The observation and the retained bytes, as Ansible judges them.
    let digest = "e".repeat(64);
    let observed = |change: &dyn Fn(&mut Value)| {
        let mut observation = json!({
            "kind": "ops-artifact-acquisition-observation",
            "scope": "AuthenticatedRetainedBytesWithoutExecutionOrInstallation",
            "artifact": {"artifactId": 4321},
            "selection": selection()["scpub_ops_selection"],
            "verification": {"matchedVerifierPolicy": {"revision": "a".repeat(40),
                "buildRunId": 5678, "binarySha256": digest}}});
        change(&mut observation);
        json!({"rc": 0, "stdout": observation.to_string()})
    };
    let binary = |change: &dyn Fn(&mut Value)| {
        let mut stat = json!({"isreg": true, "islnk": false, "nlink": 1, "size": 22229136,
            "checksum": digest});
        change(&mut stat);
        json!({"stat": stat})
    };
    let parsed = steps[9]["vars"]["scmig_ops_observed"].clone();
    let case = |acquired: Value, stat: Value| {
        let mut values = selection();
        values.insert("scmig_ops_acquired".into(), acquired);
        values.insert("scmig_ops_observed".into(), parsed.clone());
        values.insert("scmig_ops_binary".into(), stat);
        Value::Object(values)
    };
    let policy = "/verification/matchedVerifierPolicy";
    let cases = [
        (case(observed(&|_| {}), binary(&|_| {})), true),
        (
            case(
                observed(&|value| value["kind"] = json!("other")),
                binary(&|_| {}),
            ),
            false,
        ),
        (
            case(
                observed(&|value| value["scope"] = json!("Unverified")),
                binary(&|_| {}),
            ),
            false,
        ),
        (
            case(
                observed(&|value| value["artifact"]["artifactId"] = json!(4322)),
                binary(&|_| {}),
            ),
            false,
        ),
        (
            case(
                observed(&|value| value["selection"]["runAttempt"] = json!(3)),
                binary(&|_| {}),
            ),
            false,
        ),
        (
            case(
                observed(&|value| {
                    value.pointer_mut(policy).unwrap()["revision"] = json!("b".repeat(40))
                }),
                binary(&|_| {}),
            ),
            false,
        ),
        (
            case(
                observed(&|value| value.pointer_mut(policy).unwrap()["buildRunId"] = json!(5679)),
                binary(&|_| {}),
            ),
            false,
        ),
        (
            case(
                observed(&|value| {
                    value.pointer_mut(policy).unwrap()["binarySha256"] = json!("f".repeat(64))
                }),
                binary(&|_| {}),
            ),
            false,
        ),
        (
            case(
                observed(&|_| {}),
                binary(&|stat| stat["checksum"] = json!("f".repeat(64))),
            ),
            false,
        ),
        (
            case(observed(&|_| {}), binary(&|stat| stat["size"] = json!(0))),
            false,
        ),
        (
            case(
                observed(&|_| {}),
                binary(&|stat| stat["size"] = json!(268435457)),
            ),
            false,
        ),
        (
            case(
                observed(&|_| {}),
                binary(&|stat| stat["islnk"] = json!(true)),
            ),
            false,
        ),
        (
            case(observed(&|_| {}), binary(&|stat| stat["nlink"] = json!(2))),
            false,
        ),
        (
            case(
                json!({"rc": 1, "stdout": observed(&|_| {})["stdout"]}),
                binary(&|_| {}),
            ),
            false,
        ),
    ];
    let variables: Vec<Value> = cases.iter().map(|(case, _)| case.clone()).collect();
    let verdicts = asserted(&steps[9]["ansible.builtin.assert"]["that"], &variables);
    for (index, (accepted, (_, expected))) in verdicts.iter().zip(&cases).enumerate() {
        assert_eq!(accepted, expected, "case {index}");
    }
}

const RECORDED: &str = include_str!("migration_prepare_recorded.json");
const BOOTSTRAP: &str = "every_recorded_bootstrap_state_is_answered_as_the_program_did";
const EVIDENCE: &str = "every_recorded_controller_evidence_is_answered_as_the_program_did";
const FINALIZED: &str = "{\"schemaVersion\":1,\"finalized\":true}";
/// The bootstrap paths in the order they are checked, with their modes.
const BOOTSTRAP_PATHS: [(&str, &str); 7] = [
    ("/etc/simplestchat-public", "0700"),
    ("/srv/simplestchat-public", "0700"),
    ("/srv/simplestchat-public/releases", "0700"),
    ("/srv/simplestchat-public/results", "0700"),
    ("/srv/simplestchat-public/migrations", "0700"),
    ("/run/simplestchat-bench", "0700"),
    ("/usr/local/libexec/simplestchat-public", "0755"),
];
const JOURNALS: [&str; 2] = [
    "/run/simplestchat-bench/current.json",
    "/srv/simplestchat-public/release-state.json",
];

/// The machine states the bootstrap program was given, in its order.
fn bootstrap_cases() -> Vec<Value> {
    let baseline = || {
        let mut paths = Map::new();
        for (path, mode) in BOOTSTRAP_PATHS {
            let mode = &mode[1..];
            paths.insert(
                path.to_owned(),
                json!({"kind": "directory", "mode": mode, "owner": "root", "group": "root"}),
            );
        }
        json!({"paths": paths,
            "lock": {"kind": "file", "mode": "600", "owner": "root", "held": false},
            "journals": {"current.json": {"kind": "absent"},
                "release-state.json": {"kind": "absent"}},
            "request": {"kind": "absent"}})
    };
    let variant = |change: &dyn Fn(&mut Value)| {
        let mut case = baseline();
        change(&mut case);
        case
    };
    let merge = |target: &mut Value, change: &Value| {
        for (key, value) in change.as_object().unwrap() {
            target[key] = value.clone();
        }
    };
    let mut cases = vec![variant(&|case| {
        for (path, _) in BOOTSTRAP_PATHS {
            case["paths"][path] = json!({"kind": "absent"});
        }
        case["lock"] = json!({"kind": "absent"});
    })];
    cases.push(baseline());
    for (path, mode) in BOOTSTRAP_PATHS {
        let libexec = mode == "0755";
        for change in [
            json!({"mode": if libexec { "700" } else { "755" }}),
            json!({"mode": if libexec { "1755" } else { "1700" }}),
            json!({"owner": "other"}),
            json!({"group": "other"}),
            json!({"kind": "file", "text": ""}),
            json!({"kind": "link", "to": "directory"}),
            json!({"kind": "dangling"}),
        ] {
            cases.push(variant(&|case| merge(&mut case["paths"][path], &change)));
        }
    }
    for change in [
        json!({"held": true}),
        json!({"mode": "644"}),
        json!({"mode": "4600"}),
        json!({"owner": "other"}),
        json!({"group": "other"}),
        json!({"kind": "directory", "mode": "700"}),
        json!({"kind": "fifo"}),
        json!({"kind": "link"}),
        json!({"kind": "dangling"}),
        json!({"kind": "absent"}),
    ] {
        cases.push(variant(&|case| merge(&mut case["lock"], &change)));
    }
    fn file(mode: &str, owner: &str, text: &str) -> Value {
        json!({"kind": "file", "mode": mode, "owner": owner, "text": text})
    }
    let cut: String = FINALIZED[..FINALIZED.len() - 1]
        .bytes()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let undecodable = format!("{cut}2c2278223a22ff227d");
    let mut grouped = file("600", "root", FINALIZED);
    grouped["group"] = json!("other");
    let journals = [
        file("600", "root", FINALIZED),
        file("600", "root", "{\"schemaVersion\":1,\"finalized\":false}"),
        file("600", "root", "{\"schemaVersion\":1.0,\"finalized\":true}"),
        file("600", "root", "{\"schemaVersion\":true,\"finalized\":true}"),
        file(
            "600",
            "root",
            "{\"schemaVersion\":\"1\",\"finalized\":true}",
        ),
        file("600", "root", "{\"schemaVersion\":2,\"finalized\":true}"),
        file("600", "root", "{\"finalized\":true}"),
        file("600", "root", "{\"schemaVersion\":1,\"finalized\":1}"),
        file("600", "root", "[]"),
        file("600", "root", "not json"),
        file("600", "root", ""),
        json!({"kind": "file", "mode": "600", "owner": "root", "hex": undecodable}),
        file("600", "root", &format!("{FINALIZED:<16384}")),
        file("600", "root", &format!("{FINALIZED:<16385}")),
        file("640", "root", FINALIZED),
        file("600", "other", FINALIZED),
        grouped,
        json!({"kind": "link", "mode": "600", "owner": "root", "text": FINALIZED}),
        json!({"kind": "directory", "mode": "700", "owner": "root"}),
    ];
    for journal in ["current.json", "release-state.json"] {
        for state in &journals {
            cases.push(variant(&|case| case["journals"][journal] = state.clone()));
        }
    }
    for state in [
        json!({"kind": "directory", "mode": "700", "owner": "root"}),
        file("600", "root", ""),
        json!({"kind": "dangling"}),
    ] {
        cases.push(variant(&|case| case["request"] = state.clone()));
    }
    let unfinished = file("600", "root", "{\"schemaVersion\":1,\"finalized\":false}");
    cases.push(variant(&|case| {
        case["paths"]["/etc/simplestchat-public"]["mode"] = json!("755");
        case["lock"]["held"] = json!(true);
    }));
    cases.push(variant(&|case| {
        case["lock"]["mode"] = json!("644");
        case["journals"]["current.json"] = unfinished.clone();
    }));
    cases.push(variant(&|case| {
        case["journals"]["release-state.json"] = unfinished.clone();
        case["request"] = json!({"kind": "directory", "mode": "700", "owner": "root"});
    }));
    cases
}

/// Whether a path the bootstrap reads lies below a listed path that is a
/// regular file, where the stat module fails instead of finding nothing.
fn below_a_file(case: &Value, path: &str) -> bool {
    BOOTSTRAP_PATHS.iter().any(|(parent, _)| {
        path.starts_with(&format!("{parent}/")) && case["paths"][*parent]["kind"] == "file"
    })
}

/// What the bootstrap's stat, slurp and lock steps register for a case, or
/// nothing where a stat fails below a regular file.
fn bootstrap_results(case: &Value) -> Option<Value> {
    use super::ansible_modules::{slurp_result, stat_result};
    let absent = json!({"kind": "absent"});
    // Below an absent or dangling path there is nothing; below a link to a
    // directory there is what the target holds, here what the case names.
    let beneath = |path: &str| {
        let gone = BOOTSTRAP_PATHS.iter().any(|(parent, _)| {
            path.starts_with(&format!("{parent}/"))
                && ["absent", "dangling"]
                    .contains(&case["paths"][*parent]["kind"].as_str().unwrap())
        });
        !gone
    };
    let mut paths = Vec::new();
    for (path, mode) in BOOTSTRAP_PATHS {
        if below_a_file(case, path) {
            return None;
        }
        let state = if beneath(path) {
            &case["paths"][path]
        } else {
            &absent
        };
        let mut result = stat_result(state, false, 0);
        result["item"] = json!({"path": path, "mode": mode});
        paths.push(result);
    }
    let work = "/run/simplestchat-bench/workload.lock";
    let lock = if beneath(work) {
        &case["lock"]
    } else {
        &absent
    };
    let mut journal_states = Vec::new();
    let mut journal_contents = Vec::new();
    for (path, name) in JOURNALS.iter().zip(["current.json", "release-state.json"]) {
        let state = if beneath(path) {
            &case["journals"][name]
        } else {
            &absent
        };
        let mut stat = stat_result(state, false, 0);
        stat["item"] = json!(path);
        let shaped = state["kind"] == "file"
            && state["mode"] == "600"
            && state["owner"] != "other"
            && (1..=16384).contains(&super::ansible_modules::content(state).len());
        let mut content = if shaped {
            slurp_result(state)
        } else {
            json!({"changed": false, "skipped": true})
        };
        content["item"] = stat.clone();
        journal_states.push(stat);
        journal_contents.push(content);
    }
    let request = format!(
        "/srv/simplestchat-public/migrations/{}",
        "0123456789abcdef".repeat(2)
    );
    let request = if beneath(&request) {
        &case["request"]
    } else {
        &absent
    };
    Some(json!({
        "scmig_bootstrap_paths": {"changed": false, "results": paths},
        "scmig_workload_lock": stat_result(lock, false, 0),
        "scmig_journal_states": {"changed": false, "results": journal_states},
        "scmig_journal_contents": {"changed": false, "results": journal_contents},
        "scmig_existing_request": stat_result(request, false, 0),
        "scmig_run_id": "0123456789abcdef".repeat(2),
    }))
}

/// The bootstrap paths, the workload lock, the two journals and the run's
/// request are looked at without following links, each in the program's
/// order: an unsafe path, an unsafe or held lock, an unsafe or unfinished
/// journal and an existing request each refuse in the program's words, a
/// journal is read only once it is a small private root file, and a held
/// lock is found by taking it without waiting. Ansible decides every
/// recorded state as the program decided it; where a listed path is a
/// regular file, the look below it fails before the program's words.
#[test]
fn every_recorded_bootstrap_state_is_answered_as_the_program_did() {
    use super::ansible::{Verdict, first_failures};
    use super::recorded::{Ending, Recorded, agrees, ending};
    let play = play();
    let steps = block(&play, "bootstrap");
    let step = |name: &str| located(steps, |task| task["name"] == name).1.clone();
    let looked = step(BOOTSTRAP_CHECKS[0]);
    let listed: Vec<Value> = BOOTSTRAP_PATHS
        .iter()
        .map(|(path, mode)| json!({"path": path, "mode": mode}))
        .collect();
    assert_eq!(looked["loop"], json!(listed));
    assert_eq!(looked["register"], "scmig_bootstrap_paths");
    fn no_follow(path: &str) -> Value {
        json!({"path": path, "follow": false, "get_checksum": false, "get_mime": false})
    }
    assert_eq!(looked["ansible.builtin.stat"], no_follow("{{ item.path }}"));
    let lock = step(BOOTSTRAP_CHECKS[2]);
    assert_eq!(
        lock["ansible.builtin.stat"],
        no_follow("/run/simplestchat-bench/workload.lock")
    );
    let journals = step(BOOTSTRAP_CHECKS[5]);
    assert_eq!(journals["ansible.builtin.stat"], no_follow("{{ item }}"));
    assert_eq!(journals["loop"], json!(JOURNALS));
    let read = step(BOOTSTRAP_CHECKS[6]);
    assert_eq!(
        read["ansible.builtin.slurp"],
        json!({"src": "{{ item.item }}"})
    );
    assert_eq!(read["loop"], "{{ scmig_journal_states.results }}");
    let request = step(BOOTSTRAP_CHECKS[8]);
    assert_eq!(
        request["ansible.builtin.stat"],
        no_follow("/srv/simplestchat-public/migrations/{{ scmig_run_id }}")
    );
    let decided = [1, 3, 7, 9].map(|index| step(BOOTSTRAP_CHECKS[index]));
    let cases = bootstrap_cases();
    let variables: Vec<Option<Value>> = cases.iter().map(bootstrap_results).collect();
    let evaluated: Vec<Value> = variables.iter().flatten().cloned().collect();
    let mut verdicts = first_failures(&decided, &evaluated).into_iter();
    // Which of the slurp step's reads a case makes, as Ansible decides it.
    let when = read["when"].as_str().unwrap();
    let reads: Vec<Value> = evaluated
        .iter()
        .flat_map(|variables| {
            variables["scmig_journal_states"]["results"]
                .as_array()
                .unwrap()
        })
        .map(|item| json!({"item": item}))
        .collect();
    let decided_reads = super::ansible::evaluate(&[when], &reads);
    let mut reads = decided_reads.iter();
    let mut recorded = Recorded::new(RECORDED, &[BOOTSTRAP, EVIDENCE], BOOTSTRAP);
    let mut tally = std::collections::BTreeMap::<&str, usize>::new();
    for (index, (case, variables)) in cases.iter().zip(&variables).enumerate() {
        let ending = ending(&recorded.answer(case));
        let Some(variables) = variables else {
            assert!(matches!(ending, Ending::Refused(_)), "case {index}");
            *tally.entry("refused below a file").or_default() += 1;
            continue;
        };
        for item in variables["scmig_journal_contents"]["results"]
            .as_array()
            .unwrap()
        {
            let decided = reads.next().unwrap()[0].as_bool().unwrap();
            assert_eq!(decided, item.get("content").is_some(), "case {index}");
        }
        let mut verdict = verdicts.next().unwrap();
        // The lock is taken after its own check and before the journals.
        let held = case["lock"]["held"] == true
            && variables["scmig_workload_lock"]["stat"]["exists"] == true;
        let taken_first = !matches!(verdict, Verdict::Failed(position, _) if position <= 1);
        if held && taken_first {
            verdict = Verdict::Failed(2, "the lock is held".to_owned());
            assert_eq!(ending, Ending::Thrown, "case {index}");
        }
        assert!(
            agrees(&ending, Some(&verdict)),
            "case {index}: {ending:?} {verdict:?}"
        );
        let kind = match &ending {
            Ending::Accepted => "accepted",
            Ending::Refused(_) => "refused",
            Ending::Thrown => "thrown",
        };
        *tally.entry(kind).or_default() += 1;
    }
    assert_eq!(recorded.finish(), 105);
    let tally: Vec<(&str, usize)> = tally.into_iter().collect();
    assert_eq!(
        tally,
        [
            ("accepted", 10),
            ("refused", 87),
            ("refused below a file", 1),
            ("thrown", 7)
        ]
    );
}

/// The evidence paths the controller program was given, in its order.
fn evidence_cases() -> Vec<Value> {
    let private = json!({"kind": "directory", "mode": "700", "owner": "root"});
    let mut cases = Vec::new();
    for state in [
        private.clone(),
        json!({"kind": "directory", "mode": "750", "owner": "root"}),
        json!({"kind": "directory", "mode": "755", "owner": "root"}),
        json!({"kind": "directory", "mode": "1700", "owner": "root"}),
        json!({"kind": "directory", "mode": "700", "owner": "other"}),
        json!({"kind": "file", "mode": "700", "owner": "root"}),
        json!({"kind": "link", "to": "directory", "mode": "700"}),
        json!({"kind": "dangling"}),
        json!({"kind": "absent"}),
    ] {
        cases.push(json!({"evidence": "results/migration.1", "state": state}));
    }
    for evidence in [
        "results/migration.1/",
        "results/./migration.1",
        "results//migration.1",
        "results/../results/migration.1",
        "results/nested/migration.1",
        "elsewhere/migration.1",
        "results",
        "relative",
        "linked/migration.1",
    ] {
        cases.push(json!({"evidence": evidence, "state": private}));
    }
    cases
}

/// The controller's evidence is inspected without following links and must
/// be a canonical absolute path directly in the checkout's results
/// directory, a directory of the controller's user with mode 0700. The real
/// stat module and assertion run on this controller against each recorded
/// state, built in a scratch checkout, and decide as the program decided,
/// except that a path the program normalized (a trailing slash, `.` or a
/// doubled slash) is refused as not canonical.
#[test]
fn every_recorded_controller_evidence_is_answered_as_the_program_did() {
    use super::ansible::{Scratch, Verdict, first_failures_from};
    use super::ansible_modules::build;
    use super::recorded::{Ending, Recorded, agrees, ending};
    use std::os::unix::fs::MetadataExt;
    let guard = tasks(&play()["pre_tasks"]).to_vec();
    let named = |name: &str| located(&guard, |task| task["name"] == name);
    let (looked, look) = named("Inspect the private controller evidence without following links");
    let (read, user) = named("Read the controller user's identity");
    let (checked, check) =
        named("Require existing private controller evidence before contacting either host");
    assert!(0 < looked && looked < read && read < checked);
    assert_eq!(
        look["ansible.builtin.stat"],
        json!({"path": "{{ scmig_evidence }}", "follow": false, "get_checksum": false,
            "get_mime": false})
    );
    assert_eq!(look["register"], "scmig_evidence_directory");
    assert_eq!(
        user["ansible.builtin.command"],
        json!({"argv": ["/usr/bin/id", "-u"]})
    );
    assert_eq!(user["register"], "scmig_controller_user");
    for task in [look, user, check] {
        assert_eq!(task["when"], "inventory_hostname == 'migration_source'");
        if task.get("ansible.builtin.assert").is_none() {
            assert_eq!(
                (&task["delegate_to"], &task["become"]),
                (&json!("localhost"), &json!(false))
            );
        }
    }
    // Every case is a host of its own, so the selected-host rule is lifted.
    let unselected = |task: &Value| {
        let mut task = task.clone();
        task.as_object_mut().unwrap().remove("when");
        task
    };
    let scratch = Scratch::new();
    let checkout = scratch.path().join("checkout");
    let playbooks = checkout.join("ops/ansible");
    std::fs::create_dir_all(&playbooks).unwrap();
    for directory in ["results/nested", "elsewhere"] {
        std::fs::create_dir_all(checkout.join(directory)).unwrap();
    }
    std::os::unix::fs::symlink(checkout.join("results"), checkout.join("linked")).unwrap();
    let uid = std::fs::metadata(&checkout).unwrap().uid();
    let cases = evidence_cases();
    let mut variables = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        let spelled = case["evidence"].as_str().unwrap();
        let own = spelled.replace("migration.1", &format!("migration.{index}"));
        let (argument, target) = if spelled == "relative" {
            let relative = format!("results/migration.{index}");
            (relative.clone(), checkout.join(relative))
        } else {
            let mut target = std::path::PathBuf::new();
            for part in std::path::Path::new(&own.replace("linked", "results")).components() {
                match part {
                    std::path::Component::ParentDir => {
                        target.pop();
                    }
                    part => target.push(part),
                }
            }
            (
                format!("{}/{own}", checkout.display()),
                checkout.join(target),
            )
        };
        if !target.exists() {
            let holder = scratch.path().join(format!("holder-{index}"));
            std::fs::create_dir(&holder).unwrap();
            build(&case["state"], &target, &holder);
        }
        let controller = if case["state"]["owner"] == "other" {
            uid + 1
        } else {
            uid
        };
        variables.push(json!({"scmig_evidence": argument,
            "scmig_controller_user": {"stdout": controller.to_string()}}));
    }
    let decided = [unselected(look), unselected(check)];
    let verdicts = first_failures_from(&playbooks, &decided, &variables, &scratch);
    let mut recorded = Recorded::new(RECORDED, &[BOOTSTRAP, EVIDENCE], EVIDENCE);
    let normalized = [
        "results/migration.1/",
        "results/./migration.1",
        "results//migration.1",
    ];
    for (index, (case, verdict)) in cases.iter().zip(&verdicts).enumerate() {
        let ending = ending(&recorded.answer(case));
        if normalized.contains(&case["evidence"].as_str().unwrap()) {
            assert_eq!(ending, Ending::Accepted, "case {index}");
            assert!(!matches!(verdict, Verdict::Passed), "case {index}");
            continue;
        }
        assert!(
            agrees(&ending, Some(verdict)),
            "case {index}: {ending:?} {verdict:?}"
        );
    }
    assert_eq!(recorded.finish(), 18);
}
