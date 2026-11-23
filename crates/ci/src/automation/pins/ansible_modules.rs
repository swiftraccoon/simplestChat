//! What Ansible's stat, slurp and command modules register for a described
//! state, as the replays of retired inline programs give it to the tasks
//! that replaced them, and the real modules run on this machine against
//! such states to show those results are the modules' own. Only the fields
//! the replacing tasks read are described.

use super::ansible::{Scratch, play};
use super::release_playbook::base64;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// The user and group a described owner is: `root` the given identity,
/// `other` a different one.
fn identity(state: &Value, field: &str, root: u32) -> u32 {
    if state[field] == "other" {
        root + 1
    } else {
        root
    }
}

/// The bytes a described file holds: `hex` digits, or `text` in UTF-8.
pub(super) fn content(state: &Value) -> Vec<u8> {
    match state["hex"].as_str() {
        Some(hex) => {
            let pairs = hex.as_bytes().chunks(2);
            let digit = |pair: &[u8]| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16);
            pairs.map(|pair| digit(pair).unwrap()).collect()
        }
        None => state["text"]
            .as_str()
            .unwrap_or_default()
            .as_bytes()
            .to_vec(),
    }
}

/// The `stat` result of a described path, with `root` the identity that
/// owns it: `kind` is absent, file, directory, fifo, link (to a file, or
/// to a directory with `to`) or dangling; `mode` is octal text. A link is
/// described by what lstat says when `follow` is false and by its target
/// when it is true, where a dangling link does not exist.
pub(super) fn stat_result(state: &Value, follow: bool, root: u32) -> Value {
    let kind = state["kind"].as_str().unwrap();
    let mode = |state: &Value| {
        let bits = u32::from_str_radix(state["mode"].as_str().unwrap(), 8).unwrap();
        format!("{bits:04o}")
    };
    let owned = |mut stat: Value| {
        stat["uid"] = json!(identity(state, "owner", root));
        stat["gid"] = json!(identity(state, "group", root));
        stat["mode"] = json!(mode(state));
        stat
    };
    let stat = match (kind, follow) {
        ("absent", _) | ("dangling", true) => json!({"exists": false}),
        ("file", _) | ("link", true) if state["to"] != "directory" => owned(json!({
            "exists": true, "isreg": true, "isdir": false, "islnk": false, "isfifo": false,
            "size": content(state).len(),
        })),
        ("directory", _) | ("link", true) => owned(json!({
            "exists": true, "isreg": false, "isdir": true, "islnk": false, "isfifo": false,
        })),
        ("fifo", _) => owned(json!({
            "exists": true, "isreg": false, "isdir": false, "islnk": false, "isfifo": true,
        })),
        ("link" | "dangling", false) => json!({
            "exists": true, "isreg": false, "isdir": false, "islnk": true,
        }),
        _ => panic!("an unknown described state: {kind}"),
    };
    json!({"changed": false, "stat": stat})
}

/// The `slurp` result of a described file.
pub(super) fn slurp_result(state: &Value) -> Value {
    json!({"changed": false, "content": base64(&content(state)), "encoding": "base64"})
}

/// The lines Python's `str.splitlines` divides `text` into.
fn python_lines(text: &str) -> Vec<String> {
    let breaks = [
        '\n', '\r', '\u{b}', '\u{c}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}', '\u{2028}',
        '\u{2029}',
    ];
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if breaks.contains(&character) {
            if character == '\r' && characters.peek() == Some(&'\n') {
                characters.next();
            }
            lines.push(std::mem::take(&mut line));
        } else {
            line.push(character);
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// The `command` result of a described answer `{status, stdout | stdoutHex,
/// stderr | stderrHex, padTo, sleep}` for loop `item`, or nothing where the
/// module itself fails the task whatever the task's `failed_when` says: an
/// answer that is not UTF-8 or a command that outlives the task's timeout.
/// Without `exact` the trailing line breaks of each stream are stripped, as
/// the module's default `strip_empty_ends` does.
pub(super) fn command_result(answer: &Value, item: &Value, exact: bool) -> Option<Value> {
    if answer["sleep"] == true {
        return None;
    }
    let stream = |name: &str| {
        let hex = answer[format!("{name}Hex")].as_str();
        let mut bytes = match hex {
            Some(hex) => content(&json!({"hex": hex})),
            None => answer[name]
                .as_str()
                .unwrap_or_default()
                .as_bytes()
                .to_vec(),
        };
        if name == "stdout"
            && let Some(size) = answer["padTo"].as_u64()
        {
            bytes.resize(usize::try_from(size).unwrap(), b' ');
        }
        String::from_utf8(bytes).ok()
    };
    let (stdout, stderr) = (stream("stdout")?, stream("stderr")?);
    let stripped = |text: String| {
        if exact {
            text
        } else {
            text.trim_end_matches(['\r', '\n']).to_owned()
        }
    };
    let (stdout, stderr) = (stripped(stdout), stripped(stderr));
    Some(json!({
        "changed": false, "failed": false, "item": item, "rc": answer["status"],
        "stdout_lines": python_lines(&stdout), "stdout": stdout,
        "stderr_lines": python_lines(&stderr), "stderr": stderr,
    }))
}

/// Build a described state at `path`, owned by this user.
pub(super) fn build(state: &Value, path: &Path, directory: &Path) {
    let mode = |path: &Path, state: &Value| {
        let bits = u32::from_str_radix(state["mode"].as_str().unwrap(), 8).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits)).unwrap();
        let built = std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(built & 0o7777, bits, "the fixture's mode was not kept");
    };
    match state["kind"].as_str().unwrap() {
        "file" => {
            std::fs::write(path, content(state)).unwrap();
            mode(path, state);
        }
        "directory" => {
            std::fs::create_dir(path).unwrap();
            mode(path, state);
        }
        "fifo" => {
            let made = std::process::Command::new("/usr/bin/mkfifo")
                .arg(path)
                .status();
            assert!(made.unwrap().success());
            mode(path, state);
        }
        "link" => {
            let target = directory.join("target");
            if state["to"] == "directory" {
                std::fs::create_dir(&target).unwrap();
            } else {
                std::fs::write(&target, content(state)).unwrap();
            }
            mode(&target, state);
            std::os::unix::fs::symlink(&target, path).unwrap();
        }
        "dangling" => std::os::unix::fs::symlink(directory.join("missing"), path).unwrap(),
        kind => assert_eq!(kind, "absent"),
    }
}

/// The stat module, with and without following links, reports for every
/// kind of state exactly what the replays describe for it, owned by this
/// user; slurp returns a file's bytes, which need not be UTF-8.
#[test]
fn stat_and_slurp_report_what_the_replays_describe() {
    use std::os::unix::fs::MetadataExt;
    let states = [
        json!({"kind": "absent"}),
        json!({"kind": "file", "mode": "600", "text": "{\"schemaVersion\":1}"}),
        json!({"kind": "file", "mode": "4600", "hex": "7bff7d"}),
        json!({"kind": "file", "mode": "644", "text": ""}),
        json!({"kind": "directory", "mode": "700"}),
        json!({"kind": "directory", "mode": "1755"}),
        json!({"kind": "fifo", "mode": "600"}),
        json!({"kind": "link", "mode": "600", "text": "linked"}),
        json!({"kind": "link", "to": "directory", "mode": "700"}),
        json!({"kind": "dangling"}),
    ];
    let scratch = Scratch::new();
    let mut tasks = Vec::new();
    for (index, state) in states.iter().enumerate() {
        let directory = scratch.path().join(index.to_string());
        std::fs::create_dir(&directory).unwrap();
        build(state, &directory.join("path"), &directory);
        for follow in [false, true] {
            tasks.push(json!({
                "ansible.builtin.stat": {"path": directory.join("path"), "follow": follow,
                    "get_checksum": false, "get_mime": false},
                "register": format!("stat_{index}_{follow}"),
            }));
        }
        if state["kind"] == "file" {
            tasks.push(json!({
                "ansible.builtin.slurp": {"src": directory.join("path")},
                "register": format!("slurp_{index}"),
            }));
        }
    }
    // A path below a regular file is not absent: the module fails.
    tasks.push(json!({
        "ansible.builtin.stat": {"path": scratch.path().join("1/path/below"), "follow": false},
        "register": "below", "ignore_errors": true,
    }));
    let names: Vec<String> = (0..states.len())
        .flat_map(|index| [format!("stat_{index}_false"), format!("stat_{index}_true")])
        .collect();
    let slurps = (0..states.len()).map(|index| format!("slurp_{index} | default(none)"));
    let below = std::iter::once("below is failed".to_owned());
    let listed = names
        .iter()
        .cloned()
        .chain(slurps)
        .chain(below)
        .collect::<Vec<_>>();
    tasks.push(json!({"ansible.builtin.copy": {
        "content": format!("{{{{ [{}] | to_json }}}}", listed.join(", ")),
        "dest": "{{ scratch }}/results.json", "mode": "0600"}}));
    let ran = play(&Value::Array(tasks), &json!({}), &[]);
    let results: Vec<Value> =
        serde_json::from_slice(&std::fs::read(ran.path().join("results.json")).unwrap()).unwrap();
    let user = std::fs::metadata(scratch.path()).unwrap().uid();
    for (index, state) in states.iter().enumerate() {
        for (offset, follow) in [false, true].into_iter().enumerate() {
            let reported = &results[index * 2 + offset]["stat"];
            let described = stat_result(state, follow, user);
            for (field, value) in described["stat"].as_object().unwrap() {
                let group = field == "gid";
                let expected = if group {
                    let built = std::fs::symlink_metadata(scratch.path().join(index.to_string()));
                    json!(built.unwrap().gid())
                } else {
                    value.clone()
                };
                assert_eq!(reported[field], expected, "{state} {follow} {field}");
            }
        }
        let slurped = &results[names.len() + index];
        if state["kind"] == "file" {
            assert_eq!(
                slurped["content"],
                slurp_result(state)["content"],
                "{state}"
            );
        } else {
            assert!(slurped.is_null());
        }
    }
    assert_eq!(results.last(), Some(&json!(true)));
}

/// The command module strips trailing line breaks unless told not to and
/// divides the lines as Python does; an answer that is not UTF-8 and a
/// command that outlives its timeout fail the task even where the task
/// says it never fails; an executable that is not there is an exit status
/// of 2 with nothing printed.
#[test]
fn the_command_module_registers_what_the_replays_describe() {
    let printed = "A=1\r\nB=x=y\n\u{b}C\n\n";
    let shell = |script: &str| json!(["/bin/sh", "-c", script]);
    let ask = |argv: Value, extra: Value| {
        let mut task = json!({"ansible.builtin.command": {"argv": argv},
            "failed_when": false, "changed_when": false, "register": "answer",
            "ignore_errors": true});
        for (key, value) in extra.as_object().unwrap() {
            if key == "strip_empty_ends" {
                task["ansible.builtin.command"][key] = value.clone();
            } else {
                task[key] = value.clone();
            }
        }
        let tasks = json!([task, {"ansible.builtin.copy": {
            "content": "{{ answer | to_json }}", "dest": "{{ scratch }}/answer.json",
            "mode": "0600"}}]);
        let ran = play(&tasks, &json!({}), &[]);
        serde_json::from_slice::<Value>(&std::fs::read(ran.path().join("answer.json")).unwrap())
            .unwrap()
    };
    let described = |exact: bool| {
        let answer = json!({"status": 3, "stdout": printed, "stderr": "warn\n"});
        command_result(&answer, &Value::Null, exact).unwrap()
    };
    // Every byte that is not plain text is an octal escape of printf.
    let escaped: String = printed
        .bytes()
        .map(|byte| match byte {
            b' '..=b'~' if !b"'\\%".contains(&byte) => char::from(byte).to_string(),
            _ => format!("\\{byte:03o}"),
        })
        .collect();
    let script = format!("printf '{escaped}'; printf 'warn\\n' >&2; exit 3");
    for exact in [false, true] {
        let real = ask(shell(&script), json!({"strip_empty_ends": !exact}));
        for field in ["rc", "stdout", "stdout_lines", "stderr", "stderr_lines"] {
            assert_eq!(real[field], described(exact)[field], "{exact} {field}");
        }
    }
    for script in ["printf 'A=\\377\\n'", "printf 'A=1\\n'; printf '\\377' >&2"] {
        let undecodable = ask(shell(script), json!({}));
        assert_eq!(undecodable["failed"], true, "{script}");
        let answer = json!({"status": 0, "stdoutHex": "413dff0a"});
        assert!(command_result(&answer, &Value::Null, false).is_none());
    }
    let late = ask(shell("sleep 5"), json!({"timeout": 1}));
    assert_eq!(late["failed"], true);
    let missing = ask(json!(["/nonexistent/program"]), json!({}));
    assert_eq!(
        (&missing["failed"], &missing["rc"], &missing["stdout"]),
        (&json!(false), &json!(2), &json!(""))
    );
    assert_eq!(missing["stdout_lines"], json!([]));
}
