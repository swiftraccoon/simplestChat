//! Replays of the `recorded_local.json` sections of one language's
//! analysis: the native build's three phases, the native coverage, and the
//! policy of one report, each against the recorded fake `codeql` or fake
//! helper in the case's own place, held to the same answer, the same
//! commands with their arguments, directories, deadlines and variables, the
//! same checks, the same evidence and what the fake was asked.

use super::{create_native, enforce_policy, native_coverage};
use crate::{
    budget::Budget,
    codeql::{
        local::{
            Options,
            tests::{Replay, Tally, came, listing, placed, ported, same_listing, scrub, statuses},
        },
        tests::{items, recording, text, whole},
    },
    context::{Context, Limits},
    document::Document,
};
use std::{fs, path::Path};

const RECORDED: &[u8] = include_bytes!("recorded_local.json");

/// The environment every one of these cases ran in.
fn environment() -> Document {
    Document::record([
        ("PATH", "<place>/bin".into()),
        ("HOME", "<place>/caller".into()),
    ])
}

/// The log of the fake `name` below `place`, as the recording writes it.
fn log(place: &Path, name: &str) -> Document {
    let path = place.join(format!("fake/{name}.log"));
    scrub(&fs::read_to_string(path).unwrap_or_default(), place).into()
}

/// The recorded commands as the port runs them.
fn ported_events(case: &Document) -> Document {
    Document::List(items(case, "events").iter().map(ported).collect())
}

#[test]
fn every_recorded_native_build_is_traced_alike() {
    let recorded = recording(RECORDED);
    let mut tally = Tally::default();
    for case in items(&recorded, "natives") {
        let name = text(case, "name");
        let scratch = placed(case.get("tree").expect("a tree"), None, None);
        let place = &scratch.0;
        let replay = Replay::new(place, &environment(), &[], false);
        let ambient = |variable: &str| replay.variable(variable);
        let mut context = Context::new(
            &place.join("root"),
            &place.join("out"),
            Limits::REVIEWED,
            &ambient,
        )
        .expect("a context");
        let numbers = items(case, "budget");
        let number = |at: usize| numbers[at].whole().expect("a number");
        let budget = Budget {
            workers: u64::try_from(number(0)).expect("workers"),
            ram_mib: u64::try_from(number(1)).expect("memory"),
            query_ram_mib: i64::try_from(number(2)).expect("memory"),
        };
        let created = replay.with(|host| {
            create_native(
                &mut context,
                host,
                &place.join("source"),
                &place.join("bundle/codeql"),
                &place.join("openssl"),
                &budget,
            )
        });
        tally.judge(case, came(created, |()| Document::Null), &[], &[]);
        assert_eq!(
            replay.told(),
            *case.get("events").expect("events"),
            "{name}"
        );
        assert_eq!(
            statuses(&context),
            *case.get("checks").expect("checks"),
            "{name}"
        );
        let logs = case.get("logs").expect("logs");
        assert_eq!(
            log(place, "codeql"),
            *logs.get("codeql").expect("a log"),
            "{name}"
        );
    }
    let expected = Tally {
        held: 2,
        refused: 3,
        raised: 0,
        stricter: 0,
        reworded: 0,
    };
    assert_eq!(tally, expected);
}

#[test]
fn every_recorded_native_coverage_is_counted_alike() {
    let recorded = recording(RECORDED);
    let mut tally = Tally::default();
    for case in items(&recorded, "coverages") {
        let name = text(case, "name");
        let tree = case.get("tree").expect("a tree");
        let scratch = placed(
            tree,
            Some(text(&recorded, "pin")),
            Some(text(&recorded, "workflow")),
        );
        let place = &scratch.0;
        let replay = Replay::new(place, &environment(), &[], false);
        let ambient = |variable: &str| replay.variable(variable);
        let mut context = Context::new(
            &place.join("root"),
            &place.join("out"),
            Limits::REVIEWED,
            &ambient,
        )
        .expect("a context");
        let counted = replay.with(|host| {
            native_coverage(
                &mut context,
                host,
                &place.join("bundle/codeql"),
                &place.join("out/databases/c-cpp"),
            )
        });
        tally.judge(case, came(counted, Document::from), &[], &[]);
        assert_eq!(
            replay.told(),
            *case.get("events").expect("events"),
            "{name}"
        );
        assert_eq!(
            statuses(&context),
            *case.get("checks").expect("checks"),
            "{name}"
        );
        let logs = case.get("logs").expect("logs");
        assert_eq!(
            log(place, "codeql"),
            *logs.get("codeql").expect("a log"),
            "{name}"
        );
    }
    let expected = Tally {
        held: 1,
        refused: 4,
        raised: 3,
        stricter: 0,
        reworded: 0,
    };
    assert_eq!(tally, expected);
}

#[test]
fn every_recorded_policy_is_enforced_alike() {
    let recorded = recording(RECORDED);
    let revision = text(&recorded, "revision");
    let mut tally = Tally::default();
    for case in items(&recorded, "policies") {
        let name = text(case, "name");
        let scratch = placed(case.get("tree").expect("a tree"), None, None);
        let place = &scratch.0;
        let replay = Replay::new(place, &environment(), &[], false);
        let ambient = |variable: &str| replay.variable(variable);
        let mut context = Context::new(
            &place.join("root"),
            &place.join("out"),
            Limits::REVIEWED,
            &ambient,
        )
        .expect("a context");
        let options = Options {
            codeql: None,
            revision: revision.to_owned(),
            output: place.join("out"),
            language: None,
            suite: "security".to_owned(),
            openssl_prefix: place.join("openssl"),
            database_cache: None,
            rust_cargo_cache: None,
            include_vendor: case.get("includeVendor") == Some(&Document::Bool(true)),
            cache_tool: None,
        };
        let mut identity = match case.get("identity") {
            Some(Document::Null) | None => Document::record([
                ("language", text(case, "language").into()),
                ("category", text(case, "category").into()),
                ("policyPassed", false.into()),
            ]),
            Some(identity) => identity.clone(),
        };
        let enforced =
            replay.with(|host| enforce_policy(&mut context, host, &options, &mut identity));
        tally.judge(case, came(enforced, |()| identity.clone()), &[], &[]);
        assert_eq!(replay.told(), ported_events(case), "{name}");
        assert_eq!(
            statuses(&context),
            *case.get("checks").expect("checks"),
            "{name}"
        );
        let logs = case.get("logs").expect("logs");
        assert_eq!(
            log(place, "helper"),
            *logs.get("helper").expect("a log"),
            "{name}"
        );
        let left = listing(&place.join("out"), place);
        same_listing(name, &left, case.get("output").expect("an output"), &[]);
    }
    let expected = Tally {
        held: 6,
        refused: 6,
        raised: 6,
        stricter: 0,
        reworded: 0,
    };
    assert_eq!(tally, expected);
}

#[test]
fn a_native_build_stops_at_its_first_failed_phase_and_keeps_its_bounds() {
    let recorded = recording(RECORDED);
    let natives = items(&recorded, "natives");
    let phases = [
        "codeql-init-c-cpp",
        "codeql-trace-c-cpp",
        "codeql-finalize-c-cpp",
    ];
    for (failed, case) in natives.iter().skip(2).enumerate() {
        let checks = items(case, "checks");
        let names: Vec<&str> = checks.iter().map(|check| text(check, "name")).collect();
        assert_eq!(names, phases[..=failed], "{}", text(case, "name"));
        let last = checks.last().expect("a check");
        assert_ne!(whole(last, "exitStatus"), 0);
    }
    // The traced build: the fixed script after `--`, the budget's compilers
    // and memory, the static OpenSSL and the compilers' count, in the copy.
    let events = items(&natives[0], "events");
    let trace = items(&events[1], "argv");
    let tail: Vec<&str> = trace[trace.len() - 3..]
        .iter()
        .map(|word| word.text().expect("a word"))
        .collect();
    assert_eq!(tail, ["--", "bash", "build/codeql-native-build.sh"]);
    for event in events {
        assert_eq!(text(event, "cwd"), "<place>/source");
    }
    let variables = events[1].get("env").expect("variables");
    assert_eq!(text(variables, "CODEQL_BUILD_JOBS"), "2");
    assert_eq!(text(variables, "OPENSSL_DIR"), "<place>/openssl");
}
