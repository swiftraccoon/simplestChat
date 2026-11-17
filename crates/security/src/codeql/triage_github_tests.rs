//! Replays of `recorded_triage_github.json`: the reference's GitHub client,
//! its health check, its plans and its applications, each case run here
//! against the recorded fake `gh` on a private search path, which logs
//! every argument, the variables that matter and what it read. The calls
//! must be the same in the same order with the same bytes, under the same
//! limits, with the same waits of the same virtual clock.

use super::{Bounded, Github, Reach, Transport};
use crate::{
    Result,
    codeql::{
        alike,
        sources::NativeSources,
        tests::{bytes, items, recording, text},
        triage::{
            Ledger,
            plan::{Checkout, analysis_health, api_plan, apply_plan},
            tests::{Count, Served, Virtual, exceptions, number, placed, scrubbed, seconds},
        },
    },
    document::Document,
    vendor::Bounds,
};
use std::{
    cell::RefCell,
    ffi::OsString,
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
};

const RECORDED: &[u8] = include_bytes!("recorded_triage_github.json");

/// The answers of `case`, each one given again written out in full.
pub(crate) fn answers(case: &Document) -> Option<Vec<Document>> {
    let given = case.get("answers").and_then(Document::items)?;
    let mut answers: Vec<Document> = Vec::new();
    for answer in given {
        let again = answer.get("as").and_then(Document::whole);
        let again = again.map(|first| usize::try_from(first - 1).expect("an answer"));
        answers.push(again.map_or_else(|| answer.clone(), |first| answers[first].clone()));
    }
    Some(answers)
}

/// The fake `gh` `program` below `place`, answering its calls with
/// `answers` in turn.
pub(crate) fn install(place: &Path, program: &str, answers: &[Document]) {
    let spelled = place.to_string_lossy();
    fs::create_dir_all(place.join("bin")).expect("a search directory");
    let gh = place.join("bin/gh");
    fs::write(&gh, program.replace("<place>", &spelled)).expect("the fake gh");
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).expect("its mode");
    let state = place.join("gh");
    fs::create_dir_all(state.join("answers")).expect("its answers");
    fs::write(state.join("count"), "0\n").expect("its count");
    fs::write(state.join("log"), "").expect("its log");
    for (index, answer) in answers.iter().enumerate() {
        let stem = state.join(format!("answers/{}", index + 1));
        let file = |extension: &str| PathBuf::from(format!("{}.{extension}", stem.display()));
        for stream in ["stdout", "stderr"] {
            if let Some(spec) = answer.get(stream) {
                fs::write(file(stream), bytes(spec, place)).expect("an answer");
            }
        }
        if let Some(pause) = answer.get("sleep").and_then(Document::text) {
            fs::write(file("sleep"), pause).expect("a pause");
        }
        if let Some(effect) = answer.get("effect") {
            let content = bytes(effect.get("content").expect("content"), place);
            fs::write(file("effect"), content).expect("an effect");
            let target = place.join(text(effect, "path"));
            fs::write(file("target"), target.to_string_lossy().as_bytes()).expect("its target");
        }
        let status = answer.get("status").and_then(Document::whole).unwrap_or(0);
        fs::write(file("status"), status.to_string()).expect("a status");
    }
}

/// What the fake `gh` below `place` was asked: its log and what it read on
/// each call.
pub(crate) fn calls(place: &Path) -> Document {
    let state = place.join("gh");
    let Ok(count) = fs::read_to_string(state.join("count")) else {
        return Document::Null;
    };
    let count: usize = count.trim().parse().expect("a count");
    let log = fs::read_to_string(state.join("log")).expect("a log");
    let inputs = (1..=count).map(|index| {
        let read = fs::read(state.join(format!("input.{index}"))).expect("an input");
        Document::from(String::from_utf8(read).expect("text"))
    });
    let log = log.replace(&*place.to_string_lossy(), "<place>");
    Document::record([
        ("log", log.into()),
        ("inputs", Document::List(inputs.collect())),
    ])
}

/// Hold the calls `here` to the recorded `calls`, written out or by their
/// digests.
pub(crate) fn same_calls(name: &str, here: &Document, recorded: &Document) {
    if *recorded == Document::Null {
        assert_eq!(*here, Document::Null, "{name}: no gh");
        return;
    }
    let log = here.get("log").and_then(Document::text).expect("a log");
    match recorded.get("log").expect("a log") {
        Document::Text(expected) => assert_eq!(log, expected, "{name}: the log"),
        hashed => {
            let lines = log.matches('\n').count();
            assert_eq!(
                crate::digest(log.as_bytes()),
                text(hashed, "sha256"),
                "{name}"
            );
            assert_eq!(
                Some(lines as i128),
                hashed.get("lines").and_then(Document::whole)
            );
        }
    }
    let inputs = here.get("inputs").expect("inputs");
    let expected = recorded.get("inputs").expect("inputs");
    if expected.items().is_some() {
        assert_eq!(inputs, expected, "{name}: what gh read");
    } else {
        assert_eq!(
            super::super::digest(inputs),
            text(expected, "digest"),
            "{name}"
        );
    }
}

/// The environment of `case`, below its place.
pub(crate) fn environment(case: &Document, place: &Path) -> Vec<(OsString, OsString)> {
    let given = case.get("environment").and_then(Document::fields);
    let given = given.expect("an environment");
    let spelled = place.to_string_lossy();
    given
        .iter()
        .map(|(name, value)| {
            let value = value.text().expect("a value").replace("<place>", &spelled);
            (OsString::from(name), OsString::from(value))
        })
        .collect()
}

/// The fake `gh` reached as the reference's was: its limits logged, run
/// with twenty real seconds unless an answer says its own limit is meant,
/// and the virtual clock moved on by the time each answer takes.
pub(crate) struct Logged<'a> {
    clock: &'a Virtual,
    answers: Vec<Document>,
    limits: RefCell<Vec<Document>>,
}

impl<'a> Logged<'a> {
    /// The fake answering `answers`, on `clock`.
    pub(crate) fn new(clock: &'a Virtual, answers: Vec<Document>) -> Self {
        Self {
            clock,
            answers,
            limits: RefCell::new(Vec::new()),
        }
    }

    /// The limits of every call, in order.
    pub(crate) fn limits(&self) -> Vec<Document> {
        self.limits.borrow().clone()
    }
}

impl Transport for Logged<'_> {
    fn exchange(
        &self,
        program: &Path,
        arguments: &[String],
        variables: &[(OsString, OsString)],
        input: &[u8],
        within: f64,
        room: (u64, u64),
    ) -> Result<(i32, Vec<u8>)> {
        let index = self.limits.borrow().len();
        self.limits.borrow_mut().push(Document::record([
            ("timeout", number(within)),
            ("stdout", room.0.into()),
            ("stderr", room.1.into()),
        ]));
        let answer = self.answers.get(index).cloned().unwrap_or(Document::Null);
        let real = answer.get("realTimeout") == Some(&Document::Bool(true));
        let seconds = match answer.get("seconds") {
            Some(Document::Number(taken)) => taken.as_f64().expect("seconds"),
            _ => 0.0,
        };
        let limit = if real { within } else { 20.0 };
        let exchanged = Bounded.exchange(program, arguments, variables, input, limit, room);
        self.clock.advance(seconds);
        exchanged
    }
}

/// Hold the recorded limits to those here, the time of each as a number.
pub(crate) fn same_limits(name: &str, here: &[Document], recorded: &Document) {
    let recorded = recorded.items().expect("limits");
    assert_eq!(here.len(), recorded.len(), "{name}: the calls");
    for (here, recorded) in here.iter().zip(recorded) {
        assert!(alike(here, recorded), "{name}: {here:?} {recorded:?}");
    }
}

/// `value` changed as each of `edits` says: a field set or removed, at the
/// path of names and indices it gives.
fn edited(value: &Document, edits: &[Document]) -> Document {
    fn change(value: &mut Document, path: &[Document], set: Option<&Document>) {
        let (step, rest) = path.split_first().expect("a path");
        match (value, step) {
            (Document::Record(fields), Document::Text(name)) => {
                let at = fields.iter().position(|(held, _)| held == name);
                if !rest.is_empty() {
                    change(&mut fields[at.expect("a field")].1, rest, set);
                } else if let Some(set) = set {
                    match at {
                        Some(at) => fields[at].1 = set.clone(),
                        None => fields.push((name.clone(), set.clone())),
                    }
                } else {
                    fields.remove(at.expect("a field"));
                }
            }
            (Document::List(items), index) => {
                let index = usize::try_from(index.whole().expect("an index")).expect("an index");
                if rest.is_empty() {
                    items[index] = set.expect("a value").clone();
                } else {
                    change(&mut items[index], rest, set);
                }
            }
            _ => panic!("no such path"),
        }
    }
    let mut value = value.clone();
    for edit in edits {
        let path = edit.get("path").and_then(Document::items).expect("a path");
        let deleted = edit.get("delete") == Some(&Document::Bool(true));
        change(
            &mut value,
            path,
            (!deleted).then(|| edit.get("value").expect("a value")),
        );
    }
    value
}

/// One case's operations run on the client, each held to its recorded
/// result, the time after it and the next write's time.
fn operated(
    name: &str,
    case: &Document,
    clock: &Virtual,
    client: &mut Github<'_>,
    checkout: &mut Checkout<'_, '_>,
    journal: &mut Vec<Document>,
    count: &mut Count,
) {
    let ledger = Ledger::of(checkout.root);
    let revision = text(case, "revision");
    let mut previous = Document::Null;
    for (index, (op, recorded)) in items(case, "ops")
        .iter()
        .zip(items(case, "results"))
        .enumerate()
    {
        let label = format!("{name} #{index}");
        let given = |field: &str| op.get(field).and_then(Document::text);
        let here = match text(op, "op") {
            "request" => {
                let body = op.get("body").filter(|body| **body != Document::Null);
                let answered = client.request(text(op, "suffix"), body);
                let summary = op.get("summary") == Some(&Document::Bool(true));
                answered.map(|value| match value.items() {
                    Some(listed) if summary => Document::record([("length", listed.len().into())]),
                    _ => value,
                })
            }
            "prepareWrite" => client.prepare_write().map(|()| Document::Null),
            "nextWrite" => {
                client.next_write = match op.get("value") {
                    Some(Document::Text(deadline)) if deadline == "deadline" => client.deadline,
                    Some(value) => seconds(&Document::record([("v", value.clone())]), "v"),
                    None => panic!("a time"),
                };
                Ok(Document::Null)
            }
            "head" => client.head(text(op, "reference")).map(Document::from),
            "pages" => {
                let pages = client.pages(
                    text(op, "resource"),
                    text(op, "reference"),
                    given("revision"),
                );
                let summary = op.get("summary") == Some(&Document::Bool(true));
                pages.map(|pages| {
                    if summary {
                        Document::record([("length", pages.len().into())])
                    } else {
                        Document::List(pages)
                    }
                })
            }
            "health" => analysis_health(
                client,
                given("revision").unwrap_or(revision),
                given("reference").unwrap_or("refs/heads/main"),
            ),
            "plan" => {
                let reviews = exceptions(op.get("reviews").expect("reviews"));
                let planned = api_plan(
                    client,
                    checkout,
                    given("revision").unwrap_or(revision),
                    given("reference").unwrap_or("refs/heads/main"),
                    &reviews,
                    &ledger,
                );
                if let Ok(plan) = &planned {
                    previous = plan.clone();
                }
                planned
            }
            "apply" => {
                let edits = op
                    .get("edits")
                    .and_then(Document::items)
                    .unwrap_or_default();
                let plan = edited(&previous, edits);
                let reviews = exceptions(op.get("reviews").expect("reviews"));
                let mut written = |value: &Document| {
                    journal.push(value.clone());
                    Ok(())
                };
                apply_plan(client, checkout, &plan, &reviews, &mut written, &ledger)
            }
            other => panic!("no operation {other}"),
        };
        let here = here.map(|value| scrubbed(&value, checkout.root));
        count.judge(
            &label,
            recorded.get("result").expect("a result"),
            &here,
            &[],
        );
        let now = recorded.get("now").expect("a time");
        assert!(alike(&number(clock.time()), now), "{label}: the time");
        let next = recorded.get("nextWrite").expect("a time");
        assert!(
            alike(&number(client.next_write), next),
            "{label}: the next write"
        );
    }
}

#[test]
fn every_recorded_client_asks_github_alike() {
    let recorded = recording(RECORDED);
    let program = text(&recorded, "gh");
    let revision = text(&recorded, "revision");
    let (mut count, mut made_count) = (Count::default(), Count::default());
    let cases = items(&recorded, "clients");
    for case in cases {
        let name = text(case, "name");
        let scratch = placed(case);
        let place = &scratch.0;
        let answers = answers(case);
        if let Some(answers) = &answers {
            install(place, program, answers);
        }
        let clock = Virtual::ticking(seconds(case, "start"), seconds(case, "tick"));
        let tracked = case.get("tracked").expect("a commit");
        let hooks = case.get("hooks").expect("hooks");
        let pace = seconds(case, "gitSeconds");
        let git = Served::new(place, revision, tracked, hooks, &clock, pace);
        let variables = environment(case, place);
        let logged = Logged::new(&clock, answers.unwrap_or_default());
        let reach = Reach {
            clock: &clock,
            transport: &logged,
            environment: &variables,
        };
        let apply = case.get("apply") == Some(&Document::Bool(true));
        let made = Github::new(text(case, "repository"), apply, reach);
        let shown = made.as_ref().map(|client| {
            Document::record([
                ("repository", client.repository.as_str().into()),
                ("deadline", number(client.deadline)),
                ("nextWrite", number(client.next_write)),
                (
                    "executable",
                    client.executable.to_string_lossy().as_ref().into(),
                ),
            ])
        });
        let shown = shown
            .map(|made| scrubbed(&made, place))
            .map_err(|fault| *fault);
        made_count.judge(name, case.get("made").expect("made"), &shown, &[]);
        let mut journal = Vec::new();
        if let Ok(mut client) = made {
            let native = case.get("native") == Some(&Document::Bool(true));
            let cache = place.join("cache");
            let mut sources = native.then(|| {
                NativeSources::new(place, revision, &cache, &git, Bounds::REVIEWED)
                    .unwrap_or_else(|stop| panic!("{name}: {stop}"))
            });
            let mut checkout = Checkout {
                root: place,
                sources: sources.as_mut(),
                git: &git,
            };
            let case = &with_revision(case, revision);
            operated(
                name,
                case,
                &clock,
                &mut client,
                &mut checkout,
                &mut journal,
                &mut count,
            );
        }
        same_calls(name, &calls(place), case.get("calls").expect("calls"));
        same_limits(name, &logged.limits(), case.get("limits").expect("limits"));
        assert!(
            alike(&clock.sleeps(), case.get("sleeps").expect("sleeps")),
            "{name}: waits"
        );
        assert_eq!(
            &git.asked(),
            case.get("asked").expect("asked"),
            "{name}: git"
        );
        let journal = Document::List(journal);
        match case.get("journal").expect("a journal") {
            Document::List(_) => assert_eq!(&journal, case.get("journal").expect("a journal")),
            hashed => assert_eq!(
                super::super::digest(&journal),
                text(hashed, "digest"),
                "{name}"
            ),
        }
    }
    assert_eq!(cases.len(), 277);
    assert_eq!(made_count.tally(), [263, 14, 0, 0]);
    assert_eq!(count.tally(), [262, 125, 47, 0]);
}

/// `case` with the recorded revision beside it, for its operations.
fn with_revision(case: &Document, revision: &str) -> Document {
    let mut case = case.clone();
    case.put("revision", revision.into());
    case
}
