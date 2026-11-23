//! The Compose comparison against the recorded answers of the program the
//! runtime upgrade preflight ran before it, command by command.

use super::compose::{CONFIGURATION, FAILED, IDENTITY, PRESERVED, VERSION, compare};
use crate::host::command::Request;
use crate::host::command::tests::Scripted;
use crate::host::failure::{Failure, Result};
use crate::test_support::Recording;
use serde_json::{Value, json};

/// The answers of the preflight's inline program at the revision that
/// replaced it (`recorded_compose.json`).
const RECORDED: &str = include_str!("recorded_compose.json");
const TEST: &str = "every_recorded_compose_comparison_is_answered_as_the_program_did";
const SERVICES: [&str; 3] = ["postgres", "simplestchat", "caddy"];
/// Where the candidate is said to be.
const CANDIDATE: &str = "/candidate/compose";
/// The cases this comparison answers differently from the program, with the
/// refusal it gives. The program read the last of duplicate keys, where this
/// comparison refuses ambiguous JSON; the program read a `NaN` and found it
/// unequal to itself, where this comparison refuses the number as unreadable.
const STRICTER: [(&str, &str); 2] = [("duplicate key", FAILED), ("not a number", FAILED)];

fn text(value: &str) -> Value {
    json!({"status": 0, "stdout": value})
}

/// The installed project's resolved configuration as the recorder spelled it.
fn configuration() -> Value {
    json!({
        "name": "simplestchat-public",
        "services": {
            "postgres": {"image": format!("postgres@sha256:{}", "a".repeat(64)),
                "mem_limit": "1073741824",
                "environment": {"PRIVATE_SETTING": format!("{} value", "fixture")}},
            "simplestchat": {"image": format!("sha256:{}", "b".repeat(64)), "cpus": 1.5,
                "pids_limit": 256},
            "caddy": {"image": format!("caddy@sha256:{}", "c".repeat(64)), "read_only": true,
                "ports": [443]},
        },
    })
}

fn dump(value: &Value) -> String {
    serde_json::to_string(value).unwrap()
}

fn config(value: &Value) -> Value {
    text(&format!("{}\n", dump(value)))
}

fn side(role: &str) -> Value {
    let mut answers = json!({"config": config(&configuration())});
    for service in SERVICES {
        answers[format!("hash-{service}")] = text(&format!("{service} {}\n", "e".repeat(64)));
    }
    if role == "candidate" {
        answers["version"] = text("5.6.0\n");
    }
    answers
}

fn case(name: &str, change: impl FnOnce(&mut Value)) -> Value {
    let mut described = json!({"name": name, "version": "5.6.0",
        "installed": side("installed"), "candidate": side("candidate")});
    change(&mut described);
    described
}

/// `configuration()` with one field of one service replaced.
fn changed(service: &str, field: &str, value: Value) -> Value {
    let mut changed = configuration();
    changed["services"][service][field] = value;
    config(&changed)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The comparisons the program was given, in its order.
fn cases() -> Vec<Value> {
    let mut cases = vec![case("same", |_| {})];
    for (name, value) in [
        ("version with blanks", text(" 5.6.0 \t\n")),
        ("version without newline", text("5.6.0")),
        ("prefixed version", text("v5.6.0\n")),
        ("other version", text("5.5.1\n")),
        ("empty version", text("")),
        ("version with a separator", text("5.6.0\u{1f}")),
        ("version with a no-break space", text("5.6.0\u{a0}")),
        (
            "undecodable version",
            json!({"status": 0, "stdoutHex": "352e362e30ff"}),
        ),
        ("failed version", json!({"status": 1, "stdout": "5.6.0\n"})),
        (
            "late version",
            json!({"status": 0, "stdout": "5.6.0\n", "sleep": true}),
        ),
    ] {
        cases.push(case(name, |described| {
            described["candidate"]["version"] = value
        }));
    }
    cases.push(case("selected version differs", |described| {
        described["version"] = json!("5.6.1");
    }));
    let whole = dump(&configuration());
    let services = dump(&configuration()["services"]);
    let utf16: Vec<u8> = whole.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let pad = |size: usize| json!({"status": 0, "pad": [whole, size]});
    for (name, role, value) in [
        (
            "reordered keys",
            "candidate",
            text(&format!(
                "{{\"services\":{services},\"name\":\"simplestchat-public\"}}\n"
            )),
        ),
        (
            "spaced JSON",
            "candidate",
            text(&serde_json::to_string_pretty(&configuration()).unwrap()),
        ),
        (
            "integer as float",
            "candidate",
            changed("simplestchat", "pids_limit", json!(256.0)),
        ),
        (
            "boolean as integer",
            "candidate",
            changed("caddy", "read_only", json!(1)),
        ),
        (
            "changed image",
            "candidate",
            changed(
                "simplestchat",
                "image",
                json!(format!("sha256:{}", "d".repeat(64))),
            ),
        ),
        (
            "changed private value",
            "candidate",
            changed(
                "postgres",
                "environment",
                json!({"PRIVATE_SETTING": format!("other {} value", "fixture")}),
            ),
        ),
        ("added field", "candidate", {
            let mut added = configuration();
            added["networks"] = json!({});
            config(&added)
        }),
        (
            "array order",
            "candidate",
            changed("caddy", "ports", json!([443, 80])),
        ),
        (
            "large integers",
            "candidate",
            text(&format!(
                "{},\"x\":18446744073709551617}}\n",
                &whole[..whole.len() - 1]
            )),
        ),
        ("not JSON", "candidate", text("services:\n")),
        ("empty configuration", "candidate", text("")),
        (
            "duplicate key",
            "candidate",
            text(&format!("{{\"name\":\"other\",{}", &whole[1..])),
        ),
        (
            "UTF-16 configuration",
            "candidate",
            json!({"status": 0, "stdoutHex": format!("fffe{}", hex(&utf16))}),
        ),
        ("not a number", "candidate", text("{\"x\": NaN}")),
        (
            "failed candidate configuration",
            "candidate",
            json!({"status": 1, "stdout": ""}),
        ),
        (
            "failed installed configuration",
            "installed",
            json!({"status": 1, "stdout": ""}),
        ),
        (
            "late installed configuration",
            "installed",
            json!({"status": 0, "stdout": whole, "sleep": true}),
        ),
        (
            "installed configuration at the limit",
            "installed",
            pad(8 * 1024 * 1024),
        ),
        (
            "installed configuration over the limit",
            "installed",
            pad(8 * 1024 * 1024 + 1),
        ),
    ] {
        cases.push(case(name, |described| described[role]["config"] = value));
    }
    for service in SERVICES {
        let name = format!("changed {service} identity");
        let identity = text(&format!("{service} {}\n", "f".repeat(64)));
        cases.push(case(&name, |described| {
            described["candidate"][format!("hash-{service}")] = identity;
        }));
    }
    for (name, role, service, value) in [
        (
            "identity without newline",
            "candidate",
            "caddy",
            text(&format!("caddy {}", "e".repeat(64))),
        ),
        (
            "failed installed identity",
            "installed",
            "simplestchat",
            json!({"status": 1, "stdout": ""}),
        ),
        (
            "failed candidate identity",
            "candidate",
            "postgres",
            json!({"status": 2, "stdout": ""}),
        ),
        (
            "undecodable identical identity",
            "installed",
            "caddy",
            json!({"status": 0, "stdoutHex": "ff"}),
        ),
    ] {
        cases.push(case(name, |described| {
            described[role][format!("hash-{service}")] = value;
        }));
    }
    cases.push(case("undecodable identity on both", |described| {
        for role in ["installed", "candidate"] {
            described[role]["hash-caddy"] = json!({"status": 0, "stdoutHex": "ff"});
        }
    }));
    cases
}

/// The bytes one described answer prints.
fn printed(answer: &Value) -> Vec<u8> {
    if let Some(hex) = answer["stdoutHex"].as_str() {
        let pairs = hex.as_bytes().chunks(2);
        let digit = |pair: &[u8]| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16);
        return pairs.map(|pair| digit(pair).unwrap()).collect();
    }
    if let Some(pad) = answer["pad"].as_array() {
        let mut bytes = pad[0].as_str().unwrap().as_bytes().to_vec();
        bytes.resize(usize::try_from(pad[1].as_u64().unwrap()).unwrap(), b' ');
        return bytes;
    }
    answer["stdout"].as_str().unwrap().as_bytes().to_vec()
}

/// Answer one request as the recorder's stand-in answered it: by the role
/// of the executable, then `version`, `--hash SERVICE` or the configuration,
/// as a local command that exits, outlives its deadline or says too much.
fn answered(described: &Value, request: &Request<'_>) -> Result<Vec<u8>> {
    let role = if request.argv[0] == "/usr/bin/docker" {
        "installed"
    } else {
        assert_eq!(request.argv[0], CANDIDATE);
        "candidate"
    };
    let arguments = &request.argv[1..];
    let key = if arguments.iter().any(|word| word == "--hash") {
        format!("hash-{}", arguments.last().unwrap())
    } else if arguments.iter().any(|word| word == "version") {
        "version".to_owned()
    } else {
        "config".to_owned()
    };
    let answer = &described[role][key];
    if answer["sleep"] == true {
        return Err(Failure::Timeout);
    }
    if answer["status"] != 0 {
        return Err(Failure::Command);
    }
    let bytes = printed(answer);
    if bytes.len() > request.limit {
        return Err(Failure::Refused(request.oversized));
    }
    Ok(bytes)
}

/// Each command as the recorder logged it: who, with which arguments and
/// under which of the three variables the program sets.
fn logged(argv: &[String], environment: &[(String, String)]) -> Value {
    let role = if argv[0] == "/usr/bin/docker" {
        "installed"
    } else {
        "candidate"
    };
    let variable = |name: &str| {
        let found = environment.iter().find(|(key, _)| key == name);
        found.map_or("", |(_, value)| value.as_str()).to_owned()
    };
    json!({"role": role, "arguments": argv[1..], "PATH": variable("PATH"),
        "LC_ALL": variable("LC_ALL"), "DOCKER_HOST": variable("DOCKER_HOST")})
}

/// Every comparison the program was given is answered as it answered it,
/// with the same commands in the same order under the same environment,
/// each bounded to 20 seconds and 8 MiB; what the program threw on is
/// refused. The two stricter refusals are the ones named, each after a
/// prefix of the program's commands.
#[test]
fn every_recorded_compose_comparison_is_answered_as_the_program_did() {
    let mut recorded = Recording::new(RECORDED, &[TEST]).cases(TEST);
    let mut counted = std::collections::BTreeMap::<&str, usize>::new();
    for described in cases() {
        let answer = recorded.answer(&serde_json::to_vec(&described).unwrap());
        let commands = Scripted::new(|request: &Request<'_>| answered(&described, request));
        let version = described["version"].as_str().unwrap();
        let outcome = compare(&commands, CANDIDATE, version);
        let asked = commands.asked.borrow();
        for request in asked.iter() {
            assert!(request.seconds == 20.0 && request.limit == 8 * 1024 * 1024);
        }
        let calls: Vec<Value> = asked
            .iter()
            .map(|request| logged(&request.argv, &request.environment))
            .collect();
        let program_calls = answer["calls"].as_array().unwrap();
        let name = described["name"].as_str().unwrap();
        if let Some((_, refusal)) = STRICTER.iter().find(|(known, _)| *known == name) {
            assert_eq!(outcome, Err(*refusal), "{name}");
            assert!(program_calls.starts_with(&calls), "{name}");
            counted
                .entry("stricter")
                .and_modify(|n| *n += 1)
                .or_insert(1);
            continue;
        }
        assert_eq!(calls, *program_calls, "{name}");
        let said = |stream: &str| answer[stream].as_str().unwrap_or_default().to_owned();
        let expected = match answer["exit"].as_i64() {
            Some(0) => {
                assert_eq!(said("stdout"), format!("{PRESERVED}\n"), "{name}");
                Ok(())
            }
            _ if answer.get("traceback").is_some() => {
                assert!(outcome.is_err(), "{name}");
                counted.entry("thrown").and_modify(|n| *n += 1).or_insert(1);
                continue;
            }
            _ => {
                let refusal = [FAILED, VERSION, CONFIGURATION, IDENTITY]
                    .into_iter()
                    .find(|refusal| said("stderr") == format!("{refusal}\n"));
                Err(refusal.expect("a fixed refusal of the program"))
            }
        };
        assert_eq!(outcome, expected, "{name}");
        let kind = if outcome.is_ok() { "same" } else { "refused" };
        counted.entry(kind).and_modify(|n| *n += 1).or_insert(1);
    }
    assert_eq!(recorded.finish(), 39);
    let counted: Vec<(&str, usize)> = counted.into_iter().collect();
    assert_eq!(
        counted,
        [
            ("refused", 20),
            ("same", 12),
            ("stricter", 2),
            ("thrown", 5)
        ]
    );
}
