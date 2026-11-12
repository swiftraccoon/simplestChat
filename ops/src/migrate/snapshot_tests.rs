//! The Python snapshot's statements and conclusions for every recorded
//! client, replayed against a scripted one.

use super::super::client::{Client, Seen, expanded, number};
use super::super::snapshot_sql::CONSTRAINT_DEFINITION;
use super::*;
use crate::host::command::{Commands, Local, Request};
use crate::host::tests::{Fixed, paths};
use crate::host::{System, state};
use crate::json::python_dump;
use serde_json::Value;
use std::cell::RefCell;
use std::collections::BTreeSet;

const RECORDED: &str = include_str!("recorded_snapshot.json");
const ECHO: &str = "\n\\echo <MARKER>\n";

/// A machine whose only client is the recorded one.
struct Machine<'a> {
    scenario: &'a Value,
    clock: &'a Fixed,
    seen: &'a RefCell<Seen>,
}

impl Commands for Machine<'_> {
    fn run(&self, request: &Request<'_>) -> Result<Vec<u8>> {
        panic!("a snapshot runs no other command: {:?}", request.argv);
    }

    fn converse<'a>(
        &'a self,
        argv: &[String],
        environment: &[(&str, &str)],
    ) -> Result<Box<dyn Dialogue + 'a>> {
        let pairs = environment.iter();
        let environment = pairs.map(|(name, value)| ((*name).to_owned(), (*value).to_owned()));
        let started = (argv.to_vec(), environment.collect());
        self.seen.borrow_mut().started.push(started);
        Ok(Box::new(Client::new(self.scenario, self.clock, self.seen)))
    }
}

fn bounds(recorded: &Value, scenario: &Value) -> Bounds {
    let chosen = |name: &str| match scenario["bounds"].get(name) {
        Some(value) => number(value),
        None => number(&recorded["bounds"][name]),
    };
    Bounds {
        line: chosen("line"),
        section: chosen("section"),
        metadata: chosen("metadata"),
        total: chosen("total"),
    }
}

fn json(value: &Value) -> Json {
    state::decode(value.to_string().as_bytes()).unwrap()
}

/// What the scenario's function concluded on this machine.
fn concluded(host: &Host<'_>, scenario: &Value, bounds: Bounds) -> Result<Option<Json>> {
    let word = |name: &str| scenario[name].as_str().unwrap();
    match word("function") {
        "equivalent" => {
            let (source, destination) = (json(&scenario["source"]), json(&scenario["destination"]));
            equivalent(&source, &destination).map(|()| None)
        }
        "collect" => snapshot(host, word("container"), bounds).map(Some),
        "renamed_users" => {
            assert_eq!(bounds, BOUNDS);
            renamed_users(host, word("container"), word("old"), word("new")).map(Some)
        }
        "digest" => {
            let mut session = Session::open(host, word("container"), bounds)?;
            let outcome = session.digest(&expanded(&scenario["statement"]), bounds.section);
            session.close();
            outcome.map(Some)
        }
        other => panic!("unexpected recorded function {other}"),
    }
}

#[test]
fn every_recorded_client_gets_the_recorded_statements_and_conclusion() {
    let recorded: Value = serde_json::from_str(RECORDED).unwrap();
    let scenarios = recorded["scenarios"].as_array().unwrap();
    assert_eq!(scenarios.len(), 120);
    let fixed = |name: &str| recorded[name].as_str().unwrap().to_owned() + ECHO;
    let (mut passed, mut refused, mut failed) = (0, 0, 0);
    let mut markers = BTreeSet::new();
    for scenario in scenarios {
        let name = scenario["name"].as_str().unwrap();
        let clock = Fixed::at("2026-11-12T04:00:00Z");
        if scenario["clock"] == "whole" {
            // Every reading of the clock finds the whole session's time gone.
            clock.tick.set(WHOLE + SECOND);
        }
        let seen = RefCell::new(Seen::default());
        let machine = Machine {
            scenario,
            clock: &clock,
            seen: &seen,
        };
        let scratch = std::env::temp_dir();
        let host = Host {
            commands: &machine,
            clock: &clock,
            paths: paths(&scratch),
            owner: 0,
        };
        let outcome = concluded(&host, scenario, bounds(&recorded, scenario));
        let seen = seen.into_inner();
        match (&scenario["failure"], outcome) {
            (Value::Null, Ok(value)) => {
                let dump = value.map(|value| python_dump(&value, false));
                match &scenario["dump"] {
                    Value::Null => assert_eq!(dump, None, "{name}"),
                    Value::String(text) => assert_eq!(dump.as_ref(), Some(text), "{name}"),
                    large => {
                        let dump = dump.unwrap();
                        assert_eq!(dump.len(), number(&large["bytes"]), "{name}");
                        let digest = hexadecimal_text(&Sha256::digest(dump.as_bytes()));
                        assert_eq!(digest, large["sha256"].as_str().unwrap(), "{name}");
                    }
                }
                passed += 1;
            }
            (failure, Err(Failure::Refused(code))) => {
                assert_eq!(failure["refused"], code, "{name}");
                refused += 1;
            }
            (failure, Err(other)) => {
                // A value of the wrong shape and a missing field stay apart.
                let kind = failure["kind"].as_str().unwrap_or_else(|| panic!("{name}"));
                let expected = match kind {
                    "KeyError" => Failure::Key,
                    "ValueError" | "JSONDecodeError" | "DuplicateJsonError" => Failure::Value,
                    kind => panic!("unexpected recorded kind {kind}"),
                };
                assert_eq!(other, expected, "{name}");
                failed += 1;
            }
            (failure, Ok(_)) => panic!("{name}: expected {failure}"),
        }
        // Exactly the recorded statements were asked, in the recorded order.
        let asked = scenario.get("asked").and_then(Value::as_array);
        let asked = asked.into_iter().flatten().map(|entry| match entry {
            Value::String(known) if known == "<SETUP>" => fixed("setup"),
            Value::String(known) if known == "<CATALOG>" => fixed("catalog"),
            Value::String(known) if known == "<METADATA>" => fixed("metadata"),
            Value::String(text) => text.clone(),
            run => expanded(run) + ECHO,
        });
        let asked: Vec<String> = asked.collect();
        assert_eq!(seen.asked, asked, "{name}");
        let farewell = scenario.get("farewell").and_then(Value::as_str);
        assert_eq!(seen.farewell.as_deref(), farewell, "{name}");
        // One client at most, started exactly as recorded.
        let started = scenario
            .get("started")
            .is_some_and(|started| started == true);
        assert_eq!(seen.started.len(), usize::from(started), "{name}");
        for (argv, environment) in &seen.started {
            assert_eq!(serde_json::json!(argv), recorded["argv"], "{name}");
            let environment: serde_json::Map<String, Value> = environment
                .iter()
                .map(|(name, value)| (name.clone(), Value::String(value.clone())))
                .collect();
            assert_eq!(
                Value::Object(environment),
                recorded["environment"],
                "{name}"
            );
        }
        assert!(seen.markers.len() <= 1, "{name}");
        markers.extend(seen.markers);
    }
    assert_eq!((passed, refused, failed), (19, 81, 20));
    // Every session ends its statements with a marker of its own.
    assert!(markers.len() > 50);
}

/// The statements, bounds and deadlines are the helper's own.
#[test]
fn the_fixed_statements_and_bounds_are_the_recorded_ones() {
    let recorded: Value = serde_json::from_str(RECORDED).unwrap();
    assert_eq!(recorded["setup"], SETUP);
    assert_eq!(recorded["catalog"], CATALOG);
    assert_eq!(recorded["metadata"], METADATA);
    assert_eq!(recorded["constraintDefinition"], CONSTRAINT_DEFINITION);
    assert_eq!(recorded["socket"], SOCKET);
    let none = serde_json::json!({});
    assert_eq!(bounds(&recorded, &none), BOUNDS);
    assert_eq!(number(&recorded["relations"]), MAX_RELATIONS);
    assert_eq!(number(&recorded["identifierBytes"]), MAX_IDENTIFIER_BYTES);
    assert_eq!(recorded["querySeconds"], QUERY.as_secs());
    assert_eq!(recorded["wholeSeconds"], WHOLE.as_secs());
}

/// The recorded refusals are exactly the ones a snapshot can give.
#[test]
fn the_recording_covers_every_refusal_a_snapshot_names() {
    let recorded: Value = serde_json::from_str(RECORDED).unwrap();
    let mut given = BTreeSet::new();
    for scenario in recorded["scenarios"].as_array().unwrap() {
        if let Some(code) = scenario["failure"]["refused"].as_str() {
            given.insert(code.to_owned());
        }
    }
    let mut named = BTreeSet::new();
    // Escaped quotes make every piece between two quotes a candidate.
    for word in include_str!("snapshot.rs").split('"') {
        let code = word
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_');
        if code && word.contains('_') {
            named.insert(word.to_owned());
        }
    }
    assert_eq!(named, given);
}

/// A real child stands in for the client: its records are counted and
/// hashed as they arrive, a silent one costs no more than the deadline, and
/// neither is left behind.
#[test]
fn a_real_client_is_spoken_to_bounded_and_reaped() {
    struct Shell(&'static str);
    impl Commands for Shell {
        fn run(&self, request: &Request<'_>) -> Result<Vec<u8>> {
            panic!("a snapshot runs no other command: {:?}", request.argv);
        }

        fn converse<'a>(
            &'a self,
            argv: &[String],
            environment: &[(&str, &str)],
        ) -> Result<Box<dyn Dialogue + 'a>> {
            assert_eq!(argv[..3], DOCKER);
            let argv = ["/bin/sh", "-c", self.0].map(str::to_owned);
            Local.converse(&argv, environment)
        }
    }
    let answering = Shell(
        "while IFS= read -r line; do case \"$line\" in \
         '\\quit') exit 0;; \
         '\\echo '*) printf '{\"private\":\"fixture-only\"}\\n{\"number\":2}\\n'; \
         printf '%s\\n' \"${line#??????}\";; esac; done",
    );
    let clock = System::new();
    let scratch = std::env::temp_dir();
    let host = Host {
        commands: &answering,
        clock: &clock,
        paths: paths(&scratch),
        owner: 0,
    };
    let container = "1".repeat(64);
    let mut session = Session::open(&host, &container, BOUNDS).unwrap();
    let digest = session
        .digest("fixture statement;", BOUNDS.section)
        .unwrap();
    let again = session
        .digest("fixture statement;", BOUNDS.section)
        .unwrap();
    session.close();
    let payload = b"{\"private\":\"fixture-only\"}\n{\"number\":2}\n";
    let expected = format!(
        "{{\"rows\": 2, \"copyBytes\": {}, \"sha256\": \"{}\"}}",
        payload.len(),
        hexadecimal_text(&Sha256::digest(payload))
    );
    assert_eq!(python_dump(&digest, false), expected);
    assert_eq!(python_dump(&again, false), expected);
    assert!(!expected.contains("fixture-only"));
    // A client that never answers is given up on its statement's deadline.
    let silent = Shell("while IFS= read -r line; do :; done");
    let clock = Fixed::at("2026-11-12T04:00:00Z");
    clock.tick.set(QUERY / 4);
    let host = Host {
        commands: &silent,
        clock: &clock,
        paths: paths(&scratch),
        owner: 0,
    };
    let started = std::time::Instant::now();
    let refused = collect(&host, &container);
    assert_eq!(
        refused.err(),
        Some(Failure::Refused("postgres_query_deadline"))
    );
    assert!(started.elapsed() < Duration::from_secs(4));
}

/// A client that stops reading is given up on the statement's deadline, and
/// a client that can no longer be written to is a failure of the machine.
#[test]
fn a_client_that_takes_no_statement_is_named_by_how_it_failed() {
    struct Deaf(Failure);
    impl Dialogue for Deaf {
        fn send(&mut self, _: &[u8], _: Duration) -> Result<()> {
            Err(self.0.clone())
        }

        fn hear(&mut self, _: Duration) -> Result<Heard> {
            panic!("nothing was sent");
        }

        fn alive(&mut self) -> bool {
            true
        }

        fn end(self: Box<Self>, _: &[u8]) {}
    }
    struct Host1(Failure);
    impl Commands for Host1 {
        fn run(&self, request: &Request<'_>) -> Result<Vec<u8>> {
            panic!("a snapshot runs no other command: {:?}", request.argv);
        }

        fn converse<'a>(
            &'a self,
            _: &[String],
            _: &[(&str, &str)],
        ) -> Result<Box<dyn Dialogue + 'a>> {
            Ok(Box::new(Deaf(self.0.clone())))
        }
    }
    let container = "1".repeat(64);
    for (failure, reported) in [
        (
            Failure::Timeout,
            Failure::Refused("postgres_input_deadline"),
        ),
        (
            Failure::Command,
            Failure::Refused("postgres_session_exited"),
        ),
        (Failure::Os, Failure::Os),
    ] {
        let machine = Host1(failure);
        let clock = Fixed::at("2026-11-12T04:00:00Z");
        let scratch = std::env::temp_dir();
        let host = Host {
            commands: &machine,
            clock: &clock,
            paths: paths(&scratch),
            owner: 0,
        };
        assert_eq!(collect(&host, &container).err(), Some(reported));
    }
}

/// A session's whole time bounds its later statements, however promptly
/// each one is answered: a database of six statements is given up before
/// the statement during which the session's five minutes have passed.
#[test]
fn the_whole_time_of_a_session_bounds_its_later_statements() {
    let recorded: Value = serde_json::from_str(RECORDED).unwrap();
    let scenario = &recorded["scenarios"][1];
    assert_eq!(scenario["name"], "one table and no sequence");
    let outcome = |tick: Duration| {
        let clock = Fixed::at("2026-11-12T04:00:00Z");
        clock.tick.set(tick);
        let seen = RefCell::new(Seen::default());
        let machine = Machine {
            scenario,
            clock: &clock,
            seen: &seen,
        };
        let scratch = std::env::temp_dir();
        let host = Host {
            commands: &machine,
            clock: &clock,
            paths: paths(&scratch),
            owner: 0,
        };
        let result = collect(&host, scenario["container"].as_str().unwrap());
        (result.map(|_| ()), seen.into_inner().asked.len())
    };
    // The clock is read once when the session opens and three times for
    // each statement. At nine seconds a reading, all six statements fit.
    assert_eq!(outcome(Duration::from_secs(9)), (Ok(()), 6));
    // At eighteen each statement still meets its own forty seconds, and
    // the sixth would begin after the whole five minutes: it is not sent.
    let late = Err(Failure::Refused("postgres_input_deadline"));
    assert_eq!(outcome(Duration::from_secs(18)), (late, 5));
}
