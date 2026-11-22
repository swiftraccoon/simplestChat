//! The recorded answers of the Python controller this job replaced
//! (`recorded_infrastructure.json`): recap counts, the cloud-init and startup
//! projections, the serial milestones, the guest receipts, the inventory, the
//! seed, the image pin and the QEMU command line. Each case's input is bound
//! to its recorded SHA-256 before the Rust answer is compared.

use super::{Error, ansible::Recap, evidence, files, infrastructure, options::Purpose, vm};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use simplestchat_ops::release::infrastructure::{Action, Receipt, admit};
use std::path::Path;

const TESTS: [&str; 9] = [
    "cloudInit",
    "guestReceipt",
    "inventory",
    "pin",
    "qemu",
    "recap",
    "seed",
    "serial",
    "startupStderr",
];

/// Every case of `test` as (input, answer), each input's digest checked.
fn cases(test: &str) -> Vec<(Value, Value)> {
    let recording: Value =
        serde_json::from_str(include_str!("recorded_infrastructure.json")).unwrap();
    let mut names: Vec<&str> = recording["tests"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    names.sort_unstable();
    assert_eq!(names, TESTS);
    let cases = recording["tests"][test]["cases"].as_array().unwrap();
    cases
        .iter()
        .map(|case| {
            let given = case["given"].clone();
            let digest = files::hex(&Sha256::digest(serde_json::to_vec(&given).unwrap()));
            assert_eq!(case["input"].as_str(), Some(digest.as_str()), "{test}");
            (given, case["answer"].clone())
        })
        .collect()
}

fn bytes(given: &Value) -> Vec<u8> {
    if let Some(hex) = given.get("hex").and_then(Value::as_str) {
        return (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
            .collect();
    }
    let mut text = given["text"].as_str().unwrap().as_bytes().to_vec();
    let spaces = given.get("spaces").and_then(Value::as_u64).unwrap_or(0);
    text.extend(std::iter::repeat_n(b' ', usize::try_from(spaces).unwrap()));
    text
}

/// Recap counts and the completeness and idempotence verdicts are the
/// reference's for every recorded output; output that is not text is refused.
#[test]
fn every_recorded_recap_is_counted_and_judged_as_the_reference_did() {
    let recorded = cases("recap");
    for (given, answer) in &recorded {
        let parsed = Recap::parse(&bytes(given));
        let unchanged = given["unchanged"].as_bool().unwrap();
        let as_answer = |result: Result<Value, &str>| match result {
            Ok(value) => json!({"ok": value}),
            Err("vm_ansible_recap_text") => json!({"exception": "UnicodeDecodeError"}),
            Err(code) => json!({"refused": code}),
        };
        let counts = as_answer(parsed.map(Recap::to_json));
        let verdict =
            as_answer(parsed.and_then(|recap| recap.verdict(unchanged).map(|()| recap.to_json())));
        assert_eq!(
            json!({"counts": counts, "verdict": verdict}),
            *answer,
            "{given}"
        );
    }
    assert_eq!(recorded.len(), 55);
}

/// The cloud-init projection, its fixed refusals and the healthy verdict
/// are the reference's; a report that is not JSON is the reference's
/// invalid-report case.
#[test]
fn every_recorded_cloud_init_report_is_projected_as_the_reference_did() {
    let recorded = cases("cloudInit");
    for (given, answer) in &recorded {
        let status = i32::try_from(given["status"].as_i64().unwrap()).unwrap();
        let raw = match given.get("document") {
            Some(document) => serde_json::to_vec(document).unwrap(),
            None => bytes(given),
        };
        let actual = match evidence::cloud_status(&raw, status) {
            Ok(projection) => json!({"ok": projection}),
            Err(code) => match answer.get("exception") {
                Some(_) if code == "vm_cloud_init_report_invalid" => answer.clone(),
                _ => json!({"refused": code}),
            },
        };
        assert_eq!(actual, *answer, "{given}");
    }
    assert_eq!(recorded.len(), 40);
}

/// The startup projection keeps exactly the reference's classes, reasons,
/// components and withheld count for every recorded stream.
#[test]
fn every_recorded_startup_stream_is_projected_as_the_reference_did() {
    let recorded = cases("startupStderr");
    for (given, answer) in &recorded {
        assert_eq!(
            json!({"ok": evidence::startup_errors(&bytes(given))}),
            *answer,
            "{given}"
        );
    }
    assert_eq!(recorded.len(), 47);
}

/// Boot milestones, including one split across reads, and the count and
/// digest of every recorded serial stream are the reference's.
#[test]
fn every_recorded_serial_stream_shows_the_references_milestones() {
    let recorded = cases("serial");
    for (given, answer) in &recorded {
        let mut serial = evidence::Serial::default();
        for chunk in given["chunks"].as_array().unwrap() {
            serial.observe(&bytes(&json!({"hex": chunk})));
        }
        let observed = serial.to_json();
        let actual = json!({"milestones": observed["milestones"],
            "bytes": observed["bytesObserved"], "sha256": observed["sha256"]});
        assert_eq!(actual, *answer, "{given}");
    }
    assert_eq!(recorded.len(), 19);
}

/// Every recorded receipt is admitted, kept as fixed failure labels or
/// refused by the reference's code; JSON that is not one object is the
/// reference's decoding failure.
#[test]
fn every_recorded_guest_receipt_is_admitted_as_the_reference_admitted_it() {
    let recorded = cases("guestReceipt");
    for (given, answer) in &recorded {
        let action = Action::named(given["action"].as_str().unwrap()).unwrap();
        let status = i32::try_from(given["status"].as_i64().unwrap()).unwrap();
        let raw = match given.get("receipt") {
            Some(receipt) => serde_json::to_vec(receipt).unwrap(),
            None => bytes(given),
        };
        let (result, failure) = match admit(status, &raw, &"a".repeat(32), action) {
            Ok(Receipt::Passed(projection)) => (json!({"ok": projection}), Value::Null),
            Ok(Receipt::Failed(diagnostic)) => (
                json!({"refused": "vm_guest_assertion_failed"}),
                diagnostic.to_json(),
            ),
            Err(malformed) if malformed.code() == "invalid_vm_guest_receipt_json" => {
                assert!(answer["result"].get("exception").is_some(), "{given}");
                (answer["result"].clone(), Value::Null)
            }
            Err(malformed) => (json!({"refused": malformed.code()}), Value::Null),
        };
        assert_eq!(
            json!({"result": result, "guestFailure": failure}),
            *answer,
            "{given}"
        );
    }
    assert_eq!(recorded.len(), 122);
}

/// The inventory is the reference's for each recorded identity, with the
/// revision the delivery playbook binds added; invalid identities are refused.
#[test]
fn every_recorded_inventory_is_the_references_plus_the_bound_revision() {
    let recorded = cases("inventory");
    for (given, answer) in &recorded {
        let port = u16::try_from(given["port"].as_u64().unwrap());
        let revision = given["revision"].as_str().unwrap();
        let built = port.map_err(|_| Error::Input).and_then(|port| {
            infrastructure::inventory_value(
                Path::new(given["work"].as_str().unwrap()),
                port,
                given["run"].as_str().unwrap(),
                Path::new(given["artifacts"].as_str().unwrap()),
                revision,
            )
        });
        match (built, answer.get("ok")) {
            (Ok(mut value), Some(expected)) => {
                let host = &mut value["benchmark_hosts"]["hosts"]["vmfixture"];
                let bound = host.as_object_mut().unwrap().remove("vm_fixture_revision");
                assert_eq!(bound, Some(json!(revision)));
                assert_eq!(value, *expected, "{given}");
            }
            (Err(error), None) => {
                assert_eq!(error, Error::Input);
                let code = answer["refused"].as_str().unwrap();
                assert!(["invalid_vm_identity", "invalid_vm_port"].contains(&code));
            }
            (built, _) => panic!("{given}: {built:?} against {answer}"),
        }
    }
    assert_eq!(recorded.len(), 8);
}

/// The seed holds the reference's cloud-init configuration, metadata and
/// known host line; keys of another kind or comment are refused.
#[test]
fn every_recorded_seed_is_the_references_configuration() {
    let recorded = cases("seed");
    for (given, answer) in &recorded {
        let text = |name: &str| format!("{}\n", given[name].as_str().unwrap()).into_bytes();
        let port = u16::try_from(given["port"].as_u64().unwrap()).unwrap();
        let seeded = vm::seed(
            given["run"].as_str().unwrap(),
            port,
            &text("client"),
            &text("host"),
            given["private"].as_str().unwrap().as_bytes(),
        );
        match seeded {
            Ok([user, meta, known]) => {
                // The case holds a host private key: name it by its run only.
                assert_eq!(answer["result"], "ok", "{}", given["run"]);
                let files = &answer["files"];
                let header = "#cloud-config\n";
                let user = String::from_utf8(user).unwrap();
                let recorded = files["user-data"].as_str().unwrap();
                let parse = |text: &str| -> Value {
                    serde_json::from_str(text.strip_prefix(header).unwrap()).unwrap()
                };
                assert_eq!(parse(&user), parse(recorded));
                assert!(user.ends_with("}\n") && recorded.ends_with("}\n"));
                let meta: Value = serde_json::from_slice(&meta).unwrap();
                let recorded: Value =
                    serde_json::from_str(files["meta-data"].as_str().unwrap()).unwrap();
                assert_eq!(meta, recorded);
                assert_eq!(known, files["known_hosts"].as_str().unwrap().as_bytes());
            }
            Err(error) => {
                assert_eq!(error, Error::Refused);
                assert_eq!(
                    answer["result"],
                    json!({"refused": "invalid_vm_public_key"})
                );
            }
        }
    }
    assert_eq!(recorded.len(), 4);
}

/// The image pin is admitted or refused as the reference did. The pin's
/// refusals carry no code of their own; the reference's two codes are both
/// refusals here.
#[test]
fn every_recorded_image_pin_is_admitted_as_the_reference_did() {
    let recorded = cases("pin");
    for (given, answer) in &recorded {
        let raw = serde_json::to_vec(&given["pin"]).unwrap();
        match (vm::pin(&raw), answer.get("ok")) {
            (Ok((url, sha)), Some(expected)) => assert_eq!(json!([url, sha]), *expected),
            (Err(error), None) => {
                assert_eq!(error, Error::Refused);
                let code = answer["refused"].as_str().unwrap();
                assert!(["invalid_vm_image_pin", "invalid_tool_record_fields"].contains(&code));
            }
            (pinned, _) => panic!("{given}: {pinned:?} against {answer}"),
        }
    }
    assert_eq!(recorded.len(), 13);
}

/// QEMU runs with the reference's watchdog, limits, devices, sandbox, disks
/// and single loopback forward; the only difference is the network's
/// explicit IPv4-only selection, which every flow's guest shares.
#[test]
fn the_qemu_command_is_the_references_with_the_shared_ipv4_selection() {
    let recorded = cases("qemu");
    for (given, answer) in &recorded {
        let port = u16::try_from(given["port"].as_u64().unwrap()).unwrap();
        let arguments = vm::qemu_arguments_for(
            Purpose::Infrastructure,
            Path::new(given["work"].as_str().unwrap()),
            port,
            given["run"].as_str().unwrap(),
            "/usr/bin/prlimit".into(),
            "/usr/bin/qemu-system-x86_64".into(),
            false,
            1800,
        )
        .unwrap();
        let reference = answer["ok"].as_array().unwrap();
        assert_eq!(reference[0], "/usr/bin/timeout");
        assert_eq!(arguments.len(), reference.len() - 1);
        for (actual, expected) in arguments.iter().zip(&reference[1..]) {
            let actual = actual.to_str().unwrap();
            let expected = expected.as_str().unwrap();
            if expected.starts_with('{') {
                let parse = |text: &str| serde_json::from_str::<Value>(text).unwrap();
                assert_eq!(parse(actual), parse(expected));
            } else if expected.starts_with("user,id=net0,") {
                let shared = expected.replace("net0,", "net0,ipv4=on,ipv6=off,");
                assert_eq!(actual, shared);
            } else {
                assert_eq!(actual, expected);
            }
        }
    }
    assert_eq!(recorded.len(), 2);
}
