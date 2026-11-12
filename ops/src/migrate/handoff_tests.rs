//! Every recorded run of the earlier handoff program, replayed command by
//! command against the same sealed source.

use super::*;
use crate::host::command::Request as Issued;
use crate::host::command::tests::Scripted;
use crate::host::replay::{build, expected, held_lock};
use crate::host::state::tests::Scratch;
use crate::host::tests::{Fixed, paths};
use crate::turn::tests::{Run, changed, given, groups, index, private, unrecorded};
use serde_json::Value;
use std::cell::{Cell, RefCell};

const RECORDED: &str = include_str!("recorded_handoff.json");

/// Eighty-eight recorded scenarios of the program dns-handoff.yml carried,
/// replayed request by request: every command the Rust issues must be the
/// recorded one with its deadline, from the evidence directory the recorded
/// one ran in, and receives the recorded answer. Each run is admitted or
/// refused for the recorded reason, prints the recorded line and leaves the
/// recorded bridge files, each given to the proxy's account.
#[test]
fn every_recorded_handoff_is_replayed_command_by_command() {
    let recorded: Value = serde_json::from_str(RECORDED).unwrap();
    let scenarios = recorded["scenarios"].as_array().unwrap();
    assert_eq!(scenarios.len(), 88);
    let epoch = recorded["epoch"].as_i64().unwrap();
    let uid = rustix::process::geteuid().as_raw();
    let (proxy_group, relay_group) = groups();
    let (mut invocations, mut passed, mut usage) = (0, 0, 0);
    for scenario in scenarios {
        let name = scenario["name"].as_str().unwrap();
        let scratch = Scratch::new();
        let base = scratch.path();
        let mut tree = recorded["trees"][index(&scenario["start"])]
            .as_object()
            .unwrap()
            .clone();
        build(base, &tree, epoch);
        let run = Run {
            base,
            met: RefCell::new(Vec::new()),
            random: &[".tls-validation-"],
        };
        for step in scenario["steps"].as_array().unwrap() {
            invocations += 1;
            let mut words: Vec<OsString> = vec!["handoff".into()];
            let rest = step["arguments"].as_array().unwrap().iter();
            words.extend(rest.map(|word| OsString::from(word.as_str().unwrap())));
            let status = step["status"].as_i64().unwrap();
            run.alter(&step["before"], epoch);
            changed(
                &mut tree,
                &step["before"]["changed"],
                &step["before"]["removed"],
            );
            let identity = base.join("etc/machine-id");
            match step["machine"].as_str() {
                Some(text) => std::fs::write(&identity, text).unwrap(),
                None => drop(std::fs::remove_file(&identity)),
            }
            let context = format!("{name}: {words:?}");
            let Some(asked) = parse(&words) else {
                // The program took a command line of another length for a
                // value of the wrong shape; here it is a syntax error.
                assert_eq!((status, &step["refused"]), (1, &Value::from("ValueError")));
                usage += 1;
                continue;
            };
            let calls = step["calls"].as_array().unwrap();
            let position = Cell::new(0_usize);
            let commands = Scripted::new(|issued: &Issued<'_>| {
                let handshake = |_: &[&str]| panic!("{context}: a handoff speaks to no relay");
                if let Some(answer) = unrecorded(issued, handshake) {
                    return answer;
                }
                let call = calls.get(position.get());
                let call =
                    call.unwrap_or_else(|| panic!("{context}: unrecorded {:?}", issued.argv));
                position.set(position.get() + 1);
                let line = recorded["commandLines"][index(&call["argv"])]
                    .as_array()
                    .unwrap();
                let line: Vec<&str> = line.iter().map(|word| word.as_str().unwrap()).collect();
                let said: Vec<String> = issued.argv.iter().map(|word| run.recorded(word)).collect();
                assert_eq!(said, line, "{context}");
                let seconds = call["timeout"].as_f64().unwrap();
                assert!((issued.seconds - seconds).abs() < f64::EPSILON, "{context}");
                assert!(
                    issued.input.is_none() && issued.source.is_none(),
                    "{context}"
                );
                let kept = issued.archive.unwrap();
                let evidence = kept.parent().unwrap().strip_prefix(base).unwrap();
                let evidence = run.recorded(evidence.to_str().unwrap());
                assert_eq!(evidence, call["attempt"].as_str().unwrap(), "{context}");
                let mut output = private(kept);
                private(issued.errors.unwrap());
                if !call["answer"].is_null() {
                    let answer = recorded["answers"][index(&call["answer"])]
                        .as_str()
                        .unwrap();
                    output.write_all(run.concrete(answer).as_bytes()).unwrap();
                }
                match call["raises"].as_str() {
                    Some("timeout") => Err(Failure::Timeout),
                    Some("command") => Err(Failure::Command),
                    _ => Ok(Vec::new()),
                }
            });
            let clock = Fixed::at(recorded["now"].as_str().unwrap());
            let host = Host {
                commands: &commands,
                clock: &clock,
                paths: Paths {
                    root: base.join("srv/simplestchat-public"),
                    config: base.join("etc/simplestchat-public"),
                    work: base.join("run/simplestchat-bench"),
                    ..paths(base)
                },
                owner: uid,
            };
            let site = Site {
                host: &host,
                proxy: uid,
                proxy_group,
                relay_group,
                rearm: &|| {},
            };
            let holder = (step["busy"] == true).then(|| held_lock(base));
            let bound = RefCell::new(Vec::new());
            let probe = |address: Ipv4Addr| {
                bound.borrow_mut().push(address.to_string());
                match step["bindFails"] == true {
                    true => Err(std::io::Error::from(std::io::ErrorKind::AddrNotAvailable)),
                    false => Ok(()),
                }
            };
            let privileged = step["euid"] == 0;
            let outcome = execute(&site, asked, privileged, &probe, |takeover| {
                Workload::fixture_for(base, uid, takeover)
            });
            drop(holder);
            let refused = outcome.as_ref().err().map(|fault| match fault {
                Fault::Refused(text) => format!("ReleaseError: {}", run.recorded(text)),
                Fault::Kind(kind) => (*kind).to_owned(),
            });
            let context = format!("{context}: {refused:?}");
            assert_eq!(i64::from(outcome.is_err()), status, "{context}");
            // The lock refuses either unfinished journal in one way; the
            // helper told the two apart only inside its own process.
            let release = "ReleaseError: unfinished_other_release";
            let reason = step["refused"]
                .as_str()
                .map(|reason| match reason == release {
                    true => "ReleaseError: unfinished_host_work".to_owned(),
                    false => reason.to_owned(),
                });
            assert_eq!(refused, reason, "{context}");
            let line = outcome.unwrap_or_default();
            assert_eq!(
                run.recorded(&line),
                step["stdout"].as_str().unwrap(),
                "{context}"
            );
            assert_eq!(position.get(), calls.len(), "{context}");
            // The address is probed exactly when the recorded program did.
            let probed: Vec<Value> = step["bound"].as_array().unwrap().clone();
            let probed: Vec<&str> = probed
                .iter()
                .map(|pair| pair[0].as_str().unwrap())
                .collect();
            assert_eq!(*bound.borrow(), probed, "{context}");
            changed(&mut tree, &step["changed"], &step["removed"]);
            assert_eq!(run.listed(), expected(&tree), "{context}");
            given(&run, &step["chowns"], proxy_group, &context);
            passed += usize::from(status == 0);
        }
    }
    assert_eq!((invocations, passed, usage), (90, 6, 3));
}

/// An address is globally reachable exactly where the interpreter the
/// earlier program ran under said so: sixty-three addresses at the edges of
/// every special block.
#[test]
fn an_address_is_public_where_the_recorded_interpreter_said_so() {
    let recorded: Value = serde_json::from_str(RECORDED).unwrap();
    let addresses = recorded["addresses"].as_array().unwrap();
    assert_eq!(addresses.len(), 63);
    let mut reachable = 0;
    for vector in addresses {
        let text = vector["address"].as_str().unwrap();
        let address: Ipv4Addr = text.parse().unwrap();
        assert_eq!(global(address), vector["global"], "{text}");
        let admitted = public(text).is_ok();
        assert_eq!(admitted, vector["global"] == true, "{text}");
        reachable += usize::from(admitted);
    }
    assert_eq!(reachable, 32);
    // Only the one canonical spelling of an address is an address.
    for spelled in [
        "08.8.8.8",
        " 8.8.8.8",
        "8.8.8.8 ",
        "8.8.8",
        "134744072",
        "8.8.8.8.8",
        "",
    ] {
        assert_eq!(
            public(spelled).err(),
            Some(Fault::Kind("AddressValueError"))
        );
    }
}

/// A proxy image is one of the two the project uses, by tag and digest.
#[test]
fn only_a_pinned_project_proxy_image_is_admitted() {
    let digest = "e".repeat(64);
    for image in [
        "docker.io/library/caddy",
        "ghcr.io/swiftraccoon/simplestchat-caddy",
    ] {
        assert!(pinned(&format!("{image}:2.11.1@sha256:{digest}")));
        assert!(pinned(&format!("{image}:2-alpine.1@sha256:{digest}")));
        for refused in [
            format!("{image}@sha256:{digest}"),
            format!("{image}:@sha256:{digest}"),
            format!("{image}:2.11.1"),
            format!("{image}:2.11.1@sha256:{}", &digest[1..]),
            format!("{image}:2.11.1@sha256:{}", digest.to_uppercase()),
            format!("{image}:Latest@sha256:{digest}"),
            format!("{image}:a:b@sha256:{digest}"),
            format!("{image}:2.11.1@sha256:{digest}\n"),
            format!(" {image}:2.11.1@sha256:{digest}"),
            format!("{image}:2.11.1@sha256:{digest}@sha256:{digest}"),
            format!("x{image}:2.11.1@sha256:{digest}"),
        ] {
            assert!(!pinned(&refused), "{refused:?}");
        }
    }
}

/// The recorded refusals are exactly the ones a handoff names itself.
#[test]
fn the_recording_covers_every_refusal_a_handoff_names() {
    let recorded: Value = serde_json::from_str(RECORDED).unwrap();
    let mut given = std::collections::BTreeSet::new();
    for scenario in recorded["scenarios"].as_array().unwrap() {
        for step in scenario["steps"].as_array().unwrap() {
            let refused = step["refused"].as_str().unwrap_or_default();
            if let Some(code) = refused.strip_prefix("ReleaseError: handoff_") {
                given.insert(format!("handoff_{code}"));
            }
        }
    }
    let mut named = std::collections::BTreeSet::new();
    for word in include_str!("handoff.rs").split('"') {
        let plain = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_';
        if word.starts_with("handoff_") && word.bytes().all(plain) {
            named.insert(word.to_owned());
        }
    }
    assert_eq!(named, given);
    assert_eq!(named.len(), 15);
}

/// A bridge file is made new, private and the proxy's: a name that is
/// already taken is never written through.
#[test]
fn a_bridge_file_never_replaces_what_is_there() {
    use std::os::unix::fs::MetadataExt;
    let scratch = Scratch::new();
    let clock = Fixed::at("2026-11-11T06:00:00Z");
    let commands = Scripted::new(|_: &Issued<'_>| panic!("no command may run"));
    let uid = rustix::process::geteuid().as_raw();
    let (proxy_group, relay_group) = groups();
    let host = Host {
        commands: &commands,
        clock: &clock,
        paths: paths(scratch.path()),
        owner: uid,
    };
    let site = Site {
        host: &host,
        proxy: uid,
        proxy_group,
        relay_group,
        rearm: &|| {},
    };
    let path = scratch.path().join("key.pem");
    held(&site, &path, b"first").unwrap();
    let made = std::fs::metadata(&path).unwrap();
    assert_eq!((made.mode() & 0o7777, made.gid()), (0o600, proxy_group));
    let again = held(&site, &path, b"second");
    assert_eq!(again.err(), Some(Fault::Kind("FileExistsError")));
    assert_eq!(std::fs::read(&path).unwrap(), b"first");
}
