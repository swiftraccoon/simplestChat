use super::*;
use crate::host::command::Request as Asked;
use crate::host::command::tests::Scripted;
use crate::host::failure::Failure;
use crate::host::replay::{build, expected, held_lock};
use crate::host::state::tests::Scratch;
use crate::host::tests::{Fixed, paths};
use crate::turn::tests::{Run, changed, given, groups, index, private, unrecorded};
use serde_json::Value;
use std::cell::{Cell, RefCell};

/// Eighty-nine recorded scenarios of the Python handoff, replayed request by
/// request: every command the Rust issues must be the recorded one, from the
/// private validation directory the recorded one ran in, and receives the
/// recorded answer. Each invocation is admitted or refused for the recorded
/// reason and leaves the recorded envelope and certificate storage.
#[test]
fn every_recorded_handoff_is_replayed_request_by_request() {
    let recorded: Value = serde_json::from_str(include_str!("recorded_tls.json")).unwrap();
    let scenarios = recorded["scenarios"].as_array().unwrap();
    assert_eq!(scenarios.len(), 89);
    let epoch = recorded["epoch"].as_i64().unwrap();
    let uid = rustix::process::geteuid().as_raw();
    let (proxy_group, relay_group) = groups();
    let mut invocations = 0;
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
            let mut words: Vec<OsString> = vec!["tls".into()];
            let rest = step["arguments"].as_array().unwrap().iter();
            words.extend(rest.map(|word| OsString::from(word.as_str().unwrap())));
            let status = step["status"].as_i64().unwrap();
            run.alter(&step["before"], epoch);
            changed(
                &mut tree,
                &step["before"]["changed"],
                &step["before"]["removed"],
            );
            let Some((action, identity)) = parse(&words) else {
                assert_eq!(status, 2, "{name}");
                continue;
            };
            assert_ne!(status, 2, "{name}");
            let context = format!("{name}: {words:?}");
            let calls = step["calls"].as_array().unwrap();
            let position = Cell::new(0_usize);
            let commands = Scripted::new(|asked: &Asked<'_>| {
                let handshake = |_: &[&str]| panic!("{context}: a handoff speaks to no relay");
                if let Some(answer) = unrecorded(asked, handshake) {
                    return answer;
                }
                let call = calls.get(position.get());
                let call = call.unwrap_or_else(|| panic!("{context}: unrecorded {:?}", asked.argv));
                position.set(position.get() + 1);
                let line = recorded["commandLines"][index(&call["argv"])]
                    .as_array()
                    .unwrap();
                let line: Vec<&str> = line.iter().map(|word| word.as_str().unwrap()).collect();
                let said: Vec<String> = asked.argv.iter().map(|word| run.recorded(word)).collect();
                assert_eq!(said, line, "{context}");
                let seconds = call["timeout"].as_f64().unwrap();
                assert!((asked.seconds - seconds).abs() < f64::EPSILON, "{context}");
                let kept = asked.archive.unwrap();
                let evidence = kept.parent().unwrap().strip_prefix(base).unwrap();
                let evidence = run.recorded(evidence.to_str().unwrap());
                assert_eq!(evidence, call["attempt"].as_str().unwrap(), "{context}");
                let file = kept.file_name().unwrap().to_str().unwrap();
                assert_eq!(file, format!("{:03}.stdout", position.get()), "{context}");
                let mut output = private(kept);
                private(asked.errors.unwrap());
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
            let clock = Fixed::at("2026-11-11T06:00:00+00:00");
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
            let privileged = step["euid"] == 0;
            let outcome = execute(&site, action, identity, privileged, |takeover| {
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
            let runtime = "ReleaseError: Resolve unfinished benchmark cleanup before releasing";
            let reason = step["refused"]
                .as_str()
                .map(|reason| match reason == runtime {
                    true => format!("ReleaseError: {UNFINISHED}"),
                    false => reason.to_owned(),
                });
            assert_eq!(refused, reason, "{context}");
            assert_eq!(step["stdout"], "", "{context}");
            assert_eq!(position.get(), calls.len(), "{context}");
            changed(&mut tree, &step["changed"], &step["removed"]);
            assert_eq!(run.listed(), expected(&tree), "{context}");
            given(&run, &step["chowns"], proxy_group, &context);
        }
    }
    assert_eq!(invocations, 127);
}

/// Every length of remainder encodes with its padding and decodes back; text
/// that is not exactly that is refused.
#[test]
fn the_envelope_spelling_is_strict_padded_base64() {
    let vectors = [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("\u{fffd}>?", "77+9Pj8="),
    ];
    for (plain, spelled) in vectors {
        assert_eq!(base64::encode(plain.as_bytes()), spelled);
        assert_eq!(base64::decode(spelled).unwrap(), plain.as_bytes());
    }
    assert_eq!(base64::encode(&[0xfb, 0xff]), "+/8=");
    for malformed in [
        "Zg", "Zg=", "Z===", "Zm9v=", "Zm 9v", "Zm9v\n", "Zm-_", "=Zm9", "Zg==Zg==", "e30",
    ] {
        assert!(base64::decode(malformed).is_err(), "{malformed}");
    }
}
