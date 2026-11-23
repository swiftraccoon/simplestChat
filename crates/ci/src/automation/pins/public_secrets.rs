//! The public deployment's six independent secrets, which an inline Python
//! program once generated and checked: they are made once from the
//! controller's system random source, never printed and never replaced,
//! and every later run requires a small private root file with exactly the
//! six names, each 64 lowercase hexadecimal digits. The real copy task makes
//! them here, and Ansible decides every file state the retired program was
//! recorded on as the program decided it.

use super::ansible::{Scratch, ansible, evaluate, succeeded};
use super::ansible_modules::{slurp_result, stat_result};
use super::document;
use super::recorded::{Recorded, replay};
use serde_json::{Value, json};

const RECORDED: &str = include_str!("public_secrets_recorded.json");
const TEST: &str = "every_recorded_secret_file_state_is_answered_as_the_program_did";
/// The six names, in the order the file lists them.
const NAMES: [&str; 6] = [
    "postgres_admin_password",
    "migration_password",
    "database_password",
    "metrics_token",
    "proxy_secret",
    "owner_password",
];
const PATH: &str = "/etc/simplestchat-public/secrets.json";

/// The public playbook's task named `name` and its position.
fn step(name: &str) -> (usize, Value) {
    let play = document("ops/ansible/public.yml");
    let tasks = play[0]["tasks"].as_array().unwrap();
    let position = tasks.iter().position(|task| task["name"] == name).unwrap();
    (position, tasks[position].clone())
}

/// A document of the six names with distinct fixture digits, as the
/// recorder wrote it, changed by `change`.
fn filled(change: impl FnOnce(&mut serde_json::Map<String, Value>)) -> String {
    let mut fields = serde_json::Map::new();
    for (index, name) in NAMES.iter().enumerate() {
        fields.insert(
            (*name).to_owned(),
            json!(format!("{:x}", index + 1).repeat(64)),
        );
    }
    change(&mut fields);
    serde_json::to_string(&fields).unwrap()
}

fn file(text: &str, mode: &str, owner: &str) -> Value {
    json!({"kind": "file", "mode": mode, "owner": owner, "text": text})
}

/// The file states the program was given, in its order.
fn file_states() -> Vec<Value> {
    let plain = |text: &str| file(text, "600", "root");
    let mut states = vec![json!({"kind": "absent"}), plain(&filled(|_| {}))];
    let same = NAMES.map(|name| (name.to_owned(), json!("1".repeat(64))));
    states.push(plain(
        &serde_json::to_string(&serde_json::Map::from_iter(same)).unwrap(),
    ));
    let changes: [(&str, Option<Value>); 12] = [
        ("owner_password", None),
        ("extra", Some(json!("a".repeat(64)))),
        ("metrics_token", Some(json!("A".repeat(64)))),
        ("metrics_token", Some(json!("a".repeat(63)))),
        ("metrics_token", Some(json!("a".repeat(65)))),
        ("metrics_token", Some(json!(7))),
        ("metrics_token", Some(Value::Null)),
        ("metrics_token", Some(json!(["a".repeat(64)]))),
        ("metrics_token", Some(json!("g".repeat(64)))),
        (
            "metrics_token",
            Some(json!(format!("{}\n", "a".repeat(63)))),
        ),
        (
            "metrics_token",
            Some(json!(format!("{}\n", "a".repeat(64)))),
        ),
        (
            "metrics_token",
            Some(json!(format!("{}\u{e9}", "a".repeat(63)))),
        ),
    ];
    for (name, value) in changes {
        states.push(plain(&filled(|fields| match value {
            Some(value) => {
                fields.insert(name.to_owned(), value);
            }
            None => {
                fields.remove(name);
            }
        })));
    }
    states.push(plain(&serde_json::to_string(&NAMES).unwrap()));
    let mut twice: Vec<&str> = NAMES.iter().rev().copied().collect();
    twice.extend(NAMES);
    states.push(plain(&serde_json::to_string(&twice).unwrap()));
    states.push(plain(&format!("\"{}\"", NAMES.concat())));
    for text in ["null", "7", "{}", "", "not json"] {
        states.push(plain(text));
    }
    let whole = filled(|_| {});
    let duplicated = format!(
        "{},\"metrics_token\":\"{}\"}}",
        &whole[..whole.len() - 1],
        "b".repeat(64)
    );
    states.push(plain(&duplicated));
    states.push(plain(&format!("\u{feff}{whole}")));
    let cut: String = whole[..whole.len() - 2]
        .bytes()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let hex = format!("{cut}ff227d");
    states.push(json!({"kind": "file", "mode": "600", "owner": "root", "hex": hex}));
    states.push(plain(&format!("{whole:<16383}")));
    states.push(plain(&format!("{whole:<16384}")));
    for mode in ["640", "644", "400", "700", "4600", "2600", "1600"] {
        states.push(file(&whole, mode, "root"));
    }
    states.push(file(&whole, "600", "other"));
    states.push(json!({"kind": "link", "mode": "600", "owner": "root", "text": whole}));
    states.push(json!({"kind": "dangling"}));
    states.push(json!({"kind": "directory", "mode": "700", "owner": "root"}));
    states.push(json!({"kind": "fifo", "mode": "600", "owner": "root"}));
    states
}

/// The values are looked for without following links and written only
/// where nothing is; the file then found must be a small private root file,
/// and its value an object of exactly the six names, each 64 lowercase
/// hexadecimal digits, checked without printing it. Ansible decides every
/// recorded state as the program decided it, writes exactly where the
/// program wrote, and refuses where the program threw.
#[test]
fn every_recorded_secret_file_state_is_answered_as_the_program_did() {
    let look = json!({"path": PATH, "follow": false, "get_checksum": false, "get_mime": false});
    let (looked, first) = step("Look for the deployment secrets without following links");
    assert_eq!(first["ansible.builtin.stat"], look);
    assert_eq!(first["register"], "scpub_secret_record");
    let (written, generate) =
        step("Generate independent deployment secrets once without printing them");
    let (inspected, inspect) = step("Inspect the deployment secrets without following links");
    assert_eq!(inspect["ansible.builtin.stat"], look);
    assert_eq!(inspect["register"], "scpub_secret_state");
    let (shaped, shape) = step("Require the deployment secrets to be a small private root file");
    let (read, slurp) = step("Read protected secrets for private configuration rendering");
    let (kept, keep) = step("Keep secret values out of logs and persistent fact caches");
    let (decided, decide) =
        step("Decide without printing them whether the secrets have their names and shape");
    let (required, require) = step("Require deployment secrets of exactly the six names and shape");
    let order = [
        looked, written, inspected, shaped, read, kept, decided, required,
    ];
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]));
    for task in [&first, &inspect, &shape] {
        assert_eq!(task["when"], "not ansible_check_mode", "{}", task["name"]);
    }
    for task in [&generate, &slurp, &keep, &decide] {
        assert_eq!(task["no_log"], true, "{}", task["name"]);
    }
    assert_eq!(decide["ansible.builtin.set_fact"]["cacheable"], false);
    let when = generate["when"].as_array().unwrap();
    let when: Vec<&str> = when.iter().map(|rule| rule.as_str().unwrap()).collect();
    assert_eq!(
        when,
        [
            "not ansible_check_mode",
            "not scpub_secret_record.stat.exists"
        ]
    );
    let written = filled(|_| {});
    let made = |state: &Value| {
        let created = state["kind"] == "absent";
        let found = if created {
            json!({"kind": "file", "mode": "600", "owner": "root", "text": written})
        } else {
            state.clone()
        };
        (created, found)
    };
    let results = |state: &Value| {
        let (_, found) = made(state);
        let mut variables = json!({
            "scpub_secret_record": stat_result(state, false, 0),
            "scpub_secret_state": stat_result(&found, false, 0),
        });
        if found["kind"] == "file" || found["kind"] == "link" {
            variables["scpub_secret_file"] = slurp_result(&found);
        }
        Some(variables)
    };
    let states = file_states();
    let recorded = Recorded::new(RECORDED, &[TEST], TEST);
    let tasks = [shape, keep, decide, require];
    assert_eq!(replay(recorded, TEST, &tasks, &states, results, &[]), 40);
    // Where the program created the file, the copy task is the one that runs.
    let mut recorded = Recorded::new(RECORDED, &[TEST], TEST);
    let variables: Vec<Value> = states
        .iter()
        .map(|state| json!({"scpub_secret_record": stat_result(state, false, 0)}))
        .collect();
    let writes = evaluate(&when, &variables);
    for (index, (state, rules)) in states.iter().zip(writes).enumerate() {
        let answer = recorded.answer(state);
        let creates = rules.iter().all(|rule| rule == true);
        assert_eq!(creates, answer["stdout"] == "created\n", "case {index}");
        assert_eq!(creates, made(state).0, "case {index}");
    }
    recorded.finish();
}

/// The real copy task writes, once, a private object of the six names in
/// their order, each 64 lowercase hexadecimal digits and all different, and
/// prints none of them; run again it leaves the file as it was. Only its
/// destination and the root owner it gives the file are substituted.
#[test]
fn the_values_are_made_once_privately_and_never_printed() {
    use std::os::unix::fs::PermissionsExt;
    let (_, generate) = step("Generate independent deployment secrets once without printing them");
    let scratch = Scratch::new();
    let target = scratch.path().join("values.json");
    let mut task = generate.clone();
    let copy = task["ansible.builtin.copy"].as_object_mut().unwrap();
    assert_eq!(copy.remove("dest"), Some(json!(PATH)));
    assert_eq!(copy.remove("owner"), Some(json!("root")));
    assert_eq!(copy.remove("group"), Some(json!("root")));
    assert_eq!(copy["mode"], "0600");
    assert_eq!(copy["force"], false);
    copy.insert("dest".into(), json!(target));
    let run = |exists: bool| {
        let book = json!([{"hosts": "localhost", "connection": "local", "gather_facts": false,
            "vars": {"ansible_python_interpreter": "{{ ansible_playbook_python }}",
                "scpub_secret_record": {"stat": {"exists": exists}}},
            "tasks": [task.clone()]}]);
        let path = scratch.path().join("play.json");
        std::fs::write(&path, serde_json::to_vec(&book).unwrap()).unwrap();
        let arguments = ["-i".as_ref(), "localhost,".as_ref(), path.as_os_str()];
        succeeded(&ansible("ansible-playbook", &arguments, &scratch))
    };
    let said = run(false);
    let bytes = std::fs::read(&target).unwrap();
    let mode = std::fs::metadata(&target).unwrap().permissions().mode();
    assert_eq!(mode & 0o7777, 0o600);
    let value: serde_json::Map<String, Value> = serde_json::from_slice(&bytes).unwrap();
    let keys: Vec<&str> = value.keys().map(String::as_str).collect();
    let mut sorted = NAMES.to_vec();
    sorted.sort_unstable();
    assert_eq!(keys, sorted);
    let digits: Vec<&str> = value
        .values()
        .map(|value| value.as_str().unwrap())
        .collect();
    let hexadecimal = |text: &&str| {
        text.len() == 64
            && text
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    };
    assert!(digits.iter().all(hexadecimal));
    let mut distinct = digits.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(distinct.len(), NAMES.len());
    let order: Vec<usize> = NAMES
        .iter()
        .map(|name| {
            let text = std::str::from_utf8(&bytes).unwrap();
            text.find(&format!("\"{name}\"")).unwrap()
        })
        .collect();
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(!digits.iter().any(|digits| said.contains(digits)));
    assert!(said.contains("changed=1"));
    for exists in [false, true] {
        let again = run(exists);
        assert!(std::fs::read(&target).unwrap() == bytes);
        assert!(again.contains("changed=0"));
        assert!(!digits.iter().any(|digits| again.contains(digits)));
    }
}
