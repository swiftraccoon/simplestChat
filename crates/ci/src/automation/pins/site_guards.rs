//! Two of the benchmark host playbook's preflight guards, which once ran
//! inline Python: an owned container cleanup record must be a finalized one
//! in its private directory, and host maintenance must not interrupt a
//! running public project. Both now run before anything is installed, as
//! file inspections, one fixed command and assertions that Ansible itself
//! evaluates here against every state the retired programs were recorded on.

use super::ansible_modules::{command_result, slurp_result, stat_result};
use super::document;
use super::recorded::{Recorded, replay};
use serde_json::{Value, json};

const RECORDED: &str = include_str!("site_guards_recorded.json");
const CLEANUP: &str = "every_recorded_cleanup_record_state_is_answered_as_the_program_did";
const PROBE: &str = "every_recorded_public_project_probe_is_answered_as_the_program_did";
const FINALIZED: &str = "{\"schemaVersion\":1,\"finalized\":true}";
const PS: &str = concat!(
    "docker --host unix:///var/run/docker.sock ps --quiet ",
    "--filter label=com.docker.compose.project=simplestchat-public"
);

/// The playbook's preflight tasks.
fn guards() -> Vec<Value> {
    let play = document("ops/ansible/site.yml");
    play[0]["pre_tasks"].as_array().unwrap().clone()
}

/// The preflight task named `name` and its position.
fn guard(name: &str) -> (usize, Value) {
    let guards = guards();
    let position = guards.iter().position(|task| task["name"] == name).unwrap();
    (position, guards[position].clone())
}

fn file(text: &str, mode: &str, owner: &str) -> Value {
    json!({"kind": "file", "mode": mode, "owner": owner, "text": text})
}

/// The record and directory states the program was given, in its order.
fn cleanup_cases() -> Vec<Value> {
    let private = json!({"kind": "directory", "mode": "700", "owner": "root"});
    let case = |record: Value, parent: &Value| json!({"record": record, "parent": parent});
    let absent = json!({"kind": "absent"});
    let mut cases = vec![case(absent.clone(), &absent), case(absent, &private)];
    let padded = format!("{FINALIZED:<16384}");
    for text in [
        FINALIZED,
        "{\"finalized\": true, \"schemaVersion\": 1, \"phase\": \"done\"}",
        "{\"schemaVersion\":1,\"finalized\":false}",
        "{\"schemaVersion\":2,\"finalized\":true}",
        "{\"schemaVersion\":1.0,\"finalized\":true}",
        "{\"schemaVersion\":true,\"finalized\":true}",
        "{\"schemaVersion\":\"1\",\"finalized\":true}",
        "{\"schemaVersion\":1e0,\"finalized\":true}",
        "{\"schemaVersion\":1}",
        "{\"finalized\":true}",
        "{\"schemaVersion\":1,\"finalized\":\"true\"}",
        "{\"schemaVersion\":1,\"finalized\":1}",
        "{\"schemaVersion\":1,\"finalized\":null}",
        "{\"schemaVersion\":1,\"finalized\":false,\"finalized\":true}",
        "{\"schemaVersion\":1,\"finalized\":true,\"finalized\":false}",
        "[]",
        "null",
        "\"text\"",
        "1",
        "",
        "not json",
        "{\"schemaVersion\":1,\"finalized\":true} {}",
        "{\"schemaVersion\":1,\"finalized\":true,\"x\":NaN}",
        &format!("\u{feff}{FINALIZED}"),
        &format!(" \n{FINALIZED}\r\n"),
    ] {
        cases.push(case(file(text, "600", "root"), &private));
    }
    let undecodable: String = FINALIZED[..FINALIZED.len() - 1]
        .bytes()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let undecodable = format!("{undecodable}2c2278223a22ff227d");
    cases.push(case(
        json!({"kind": "file", "mode": "600", "owner": "root", "hex": undecodable}),
        &private,
    ));
    cases.push(case(file(&padded, "600", "root"), &private));
    cases.push(case(file(&format!("{padded} "), "600", "root"), &private));
    for mode in ["640", "644", "400", "700", "4600", "2600", "1600"] {
        cases.push(case(file(FINALIZED, mode, "root"), &private));
    }
    cases.push(case(file(FINALIZED, "600", "other"), &private));
    for record in [
        json!({"kind": "directory", "mode": "700", "owner": "root"}),
        json!({"kind": "link", "mode": "600", "owner": "root"}),
        json!({"kind": "dangling"}),
        json!({"kind": "fifo", "mode": "600", "owner": "root"}),
    ] {
        cases.push(case(record, &private));
    }
    let mut parents: Vec<Value> = ["755", "750", "1700", "500"]
        .iter()
        .map(|mode| json!({"kind": "directory", "mode": mode, "owner": "root"}))
        .collect();
    parents.push(json!({"kind": "directory", "mode": "700", "owner": "other"}));
    parents.push(json!({"kind": "link", "to": "directory", "mode": "700", "owner": "root"}));
    for parent in &parents {
        cases.push(case(file(FINALIZED, "600", "root"), parent));
    }
    let unfinished = file("{\"schemaVersion\":1,\"finalized\":false}", "644", "root");
    let open = json!({"kind": "directory", "mode": "755", "owner": "root"});
    cases.push(case(unfinished, &open));
    cases
}

/// The cleanup record is looked at without following links, its directory
/// only when it exists, and it is read only once it is a small private file
/// of root in a private directory of root; then only a finalized record of
/// schema 1 lets provisioning continue. Ansible decides every recorded
/// state as the program decided it: in the program's words where it
/// refused, and refusing where the program threw.
#[test]
fn every_recorded_cleanup_record_state_is_answered_as_the_program_did() {
    let record = "/run/simplestchat-bench/current.json";
    fn look(path: &str) -> Value {
        json!({"path": path, "follow": false, "get_checksum": false, "get_mime": false})
    }
    let (looked, look_record) =
        guard("Look for an owned container cleanup record without following links");
    assert_eq!(look_record["ansible.builtin.stat"], look(record));
    assert_eq!(look_record["register"], "scbench_cleanup_record");
    let (_, look_parent) = guard("Look at the cleanup record's directory without following links");
    assert_eq!(
        look_parent["ansible.builtin.stat"],
        look("/run/simplestchat-bench")
    );
    assert_eq!(look_parent["register"], "scbench_cleanup_directory");
    let (shaped, shape) =
        guard("Require a cleanup record to be a small private file in its private directory");
    let (read, slurp) = guard("Read the owned container cleanup record");
    assert_eq!(slurp["ansible.builtin.slurp"], json!({"src": record}));
    assert_eq!(slurp["register"], "scbench_cleanup_content");
    let (finished, finish) = guard("Refuse provisioning while container cleanup remains uncertain");
    assert!(looked < shaped && shaped < read && read < finished);
    let exists = json!("scbench_cleanup_record.stat.exists");
    for task in [&look_parent, &shape, &slurp, &finish] {
        assert_eq!(task["when"], exists, "{}", task["name"]);
    }
    for task in [&look_record, &look_parent, &shape, &slurp, &finish] {
        assert_eq!(task["tags"], json!(["always"]), "{}", task["name"]);
        assert!(task.get("ignore_errors").is_none() && task.get("failed_when").is_none());
    }
    let results = |case: &Value| {
        let record = &case["record"];
        let mut variables = json!({
            "scbench_cleanup_record": stat_result(record, false, 0),
            "scbench_cleanup_directory": stat_result(&case["parent"], false, 0),
        });
        if record["kind"] == "file" {
            variables["scbench_cleanup_content"] = slurp_result(record);
        }
        Some(variables)
    };
    let recorded = Recorded::new(RECORDED, &[CLEANUP, PROBE], CLEANUP);
    let replayed = replay(
        recorded,
        CLEANUP,
        &[shape, finish],
        &cleanup_cases(),
        results,
        &[],
    );
    assert_eq!(replayed, 49);
}

fn ok(stdout: &str) -> Value {
    json!({"status": 0, "stdout": stdout})
}

/// The socket states and listings the program was given, in its order.
fn probe_cases() -> Vec<Value> {
    let case = |socket: &str, answer: Option<Value>| {
        let answers = answer.map_or(json!({}), |answer| json!({PS: answer}));
        json!({"socket": socket, "answers": answers})
    };
    let mut cases = vec![case("absent", None), case("dangling", None)];
    for socket in ["file", "directory", "link"] {
        cases.push(case(socket, Some(ok(""))));
    }
    for answer in [
        ok("\n"),
        ok(" \t\n\r\n"),
        ok("0123456789ab\n"),
        ok("a\nb\n"),
        ok("x"),
        json!({"status": 0, "stdout": "", "stderr": "warning\n"}),
        ok("\u{a0}\n"),
        ok("\u{1c}"),
        json!({"status": 0, "stdoutHex": "ff0a"}),
        json!({"status": 0, "stdout": "", "stderrHex": "ff"}),
        json!({"status": 1, "stdout": ""}),
        json!({"status": 1, "stdout": "0123456789ab\n"}),
        json!({"status": 0, "stdout": "", "sleep": true}),
    ] {
        cases.push(case("file", Some(answer)));
    }
    let mut gone = case("file", None);
    gone["missing"] = json!(["docker"]);
    cases.push(gone);
    cases
}

/// The socket is looked for through links, as an existing path, and only
/// then are the public project's running containers listed by the fixed
/// command; any listed container refuses host maintenance. A command that
/// fails, cannot start, outlives 15 seconds or answers in something other
/// than UTF-8 fails the task, as the program threw. Ansible decides every
/// recorded state as the program decided it.
#[test]
fn every_recorded_public_project_probe_is_answered_as_the_program_did() {
    let (looked, look) = guard("Look for the local Docker socket");
    assert_eq!(
        look["ansible.builtin.stat"],
        json!({"path": "/var/run/docker.sock", "follow": true, "get_checksum": false,
            "get_mime": false})
    );
    assert_eq!(look["register"], "scbench_docker_socket");
    let (listed, list) = guard("List the running containers of the public project");
    let argv: Vec<&str> = PS.split(' ').collect();
    let mut expected = vec!["/usr/bin/docker"];
    expected.extend(&argv[1..]);
    assert_eq!(list["ansible.builtin.command"], json!({"argv": expected}));
    assert_eq!(list["register"], "scbench_public_running");
    assert_eq!(
        (&list["timeout"], &list["changed_when"], &list["check_mode"]),
        (&json!(15), &json!(false), &json!(false))
    );
    assert!(list.get("failed_when").is_none() && list.get("ignore_errors").is_none());
    let (refused, refuse) =
        guard("Refuse host maintenance while the public application is running");
    assert!(looked < listed && listed < refused);
    let exists = json!("scbench_docker_socket.stat.exists");
    assert_eq!((&list["when"], &refuse["when"]), (&exists, &exists));
    for task in [&look, &list, &refuse] {
        assert_eq!(task["tags"], json!(["always"]), "{}", task["name"]);
    }
    let results = |case: &Value| {
        let kind = case["socket"].as_str().unwrap();
        let state = match kind {
            "file" | "link" => json!({"kind": kind, "mode": "600", "text": ""}),
            "directory" => json!({"kind": kind, "mode": "755"}),
            _ => json!({"kind": kind}),
        };
        let mut variables = json!({"scbench_docker_socket": stat_result(&state, true, 0)});
        if variables["scbench_docker_socket"]["stat"]["exists"] == true {
            // A command that cannot start or exits otherwise than zero fails
            // the task: nothing is left to assert.
            let answer = &case["answers"][PS];
            if case.get("missing").is_some() || answer["status"] != 0 {
                return None;
            }
            variables["scbench_public_running"] = command_result(answer, &Value::Null, false)?;
        }
        Some(variables)
    };
    let recorded = Recorded::new(RECORDED, &[CLEANUP, PROBE], PROBE);
    let replayed = replay(recorded, PROBE, &[refuse], &probe_cases(), results, &[]);
    assert_eq!(replayed, 19);
}
