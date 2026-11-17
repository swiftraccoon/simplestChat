//! Replays of `recorded_triage_main.json`: the command lines the
//! reference's parser read, and whole runs of its `main` for every mode,
//! each in its own place with the recorded checkout, commit, policy,
//! reviews and fake `gh`, held to the same exit status, the same line on
//! standard error, the same evidence files, bytes and modes, and the same
//! calls of `gh` and `git`.

use super::{Host, Line, Options, command, parsed};
use crate::{
    codeql::{
        alike,
        tests::{items, recording, text},
        triage::{
            github::{
                Reach,
                tests::{Logged, answers, calls, environment, install, same_calls, same_limits},
            },
            tests::{Served, Virtual, placed, seconds},
        },
    },
    document::Document,
    policy::Date,
};
use std::{fs, os::unix::fs::PermissionsExt as _, path::Path};

const RECORDED: &[u8] = include_bytes!("recorded_triage_main.json");

/// What the recording writes of the reference's namespace of `options`.
fn namespace(options: &Options) -> Document {
    let spelled = |path: &Option<std::path::PathBuf>| match path {
        Some(path) => Document::from(path.to_string_lossy().as_ref()),
        None => Document::Null,
    };
    Document::record([
        ("mode", options.mode.as_str().into()),
        ("input", spelled(&options.input)),
        ("output", options.output.to_string_lossy().as_ref().into()),
        ("repository", options.repository.as_str().into()),
        ("revision", options.revision.as_str().into()),
        ("reference", options.reference.as_str().into()),
        ("authorizeDismissals", options.authorize_dismissals.into()),
        ("sourceCache", spelled(&options.source_cache)),
        ("includeVendor", options.include_vendor.into()),
    ])
}

#[test]
fn every_recorded_command_line_is_read_alike() {
    let recorded = recording(RECORDED);
    let mut tally = [0; 3];
    for case in items(&recorded, "lines") {
        let arguments: Vec<String> = items(case, "arguments")
            .iter()
            .map(|argument| argument.text().expect("an argument").to_owned())
            .collect();
        let result = case.get("result").expect("a result");
        let here = parsed(&arguments);
        if let Some(expected) = result.get("parsed") {
            let Line::Asked(options) = here else {
                panic!("{arguments:?} was parsed there: {here:?}");
            };
            assert_eq!(&namespace(&options), expected, "{arguments:?}");
            tally[0] += 1;
        } else if result.get("exit").and_then(Document::whole) == Some(0) {
            assert_eq!(here, Line::Help, "{arguments:?}");
            tally[1] += 1;
        } else {
            assert_eq!(result.get("exit").and_then(Document::whole), Some(2));
            assert_eq!(here, Line::Refused, "{arguments:?}");
            tally[2] += 1;
        }
    }
    assert_eq!(tally, [30, 9, 40]);
}

/// Every entry below `root` as the recorder listed it: its kind, its mode,
/// and a file's text, its bytes, or its size and digest.
fn listing(root: &Path, place: &Path) -> Document {
    if !root.exists() {
        return Document::Null;
    }
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("a directory") {
            let path = entry.expect("an entry").path();
            let relative = path.strip_prefix(root).expect("below");
            let metadata = path.symlink_metadata().expect("metadata");
            let mut fields: Vec<(String, Document)> = Vec::new();
            if metadata.file_type().is_symlink() {
                fields.push(("kind".to_owned(), "link".into()));
            } else if metadata.is_dir() {
                fields.push(("kind".to_owned(), "directory".into()));
                pending.push(path.clone());
            } else {
                fields.push(("kind".to_owned(), "file".into()));
                let data = fs::read(&path).expect("a file");
                if data.len() > 65536 {
                    fields.push(("size".to_owned(), data.len().into()));
                    fields.push(("sha256".to_owned(), crate::digest(&data).into()));
                } else {
                    let text = String::from_utf8(data).expect("text");
                    let text = text.replace(&*place.to_string_lossy(), "<place>");
                    fields.push(("text".to_owned(), text.into()));
                }
            }
            let mode = u64::from(metadata.permissions().mode() & 0o777);
            fields.push(("mode".to_owned(), mode.into()));
            let name = relative.to_string_lossy().into_owned();
            found.push((name, Document::Record(fields)));
        }
    }
    Document::Record(found).sorted()
}

#[test]
fn every_recorded_main_leaves_the_same_evidence() {
    let recorded = recording(RECORDED);
    let program = text(&recorded, "gh");
    let revision = text(&recorded, "revision");
    let today = Date::parse(text(&recorded, "today")).expect("a day");
    let cases = items(&recorded, "mains");
    let mut statuses = [0; 2];
    let mut evidence = [0; 3];
    let mut directories = 0;
    for case in cases {
        let name = text(case, "name");
        let scratch = placed(case);
        let place = &scratch.0;
        let root = place.join("root");
        let answers = answers(case);
        if let Some(answers) = &answers {
            install(place, program, answers);
        }
        let clock = Virtual::new(seconds(case, "start"));
        let tracked = case.get("tracked").expect("a commit");
        let pace = seconds(case, "gitSeconds");
        let git = Served::new(&root, revision, tracked, &Document::Null, &clock, pace);
        let variables = environment(case, place);
        let logged = Logged::new(&clock, answers.unwrap_or_default());
        let reach = Reach {
            clock: &clock,
            transport: &logged,
            environment: &variables,
        };
        let host = Host {
            reach,
            git: &git,
            today,
        };
        let spelled = place.to_string_lossy();
        let arguments: Vec<String> = items(case, "arguments")
            .iter()
            .map(|argument| {
                argument
                    .text()
                    .expect("an argument")
                    .replace("<place>", &spelled)
            })
            .collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let status = command(&root, &arguments, &host, &mut out, &mut err);
        let result = case.get("result").expect("a result");
        // Where the reference's interpreter left with what it raised, it
        // ended with 1.
        let raised = result.get("raised").map(|_| 1);
        let expected = result.get("status").and_then(Document::whole).or(raised);
        assert_eq!(Some(i128::from(status)), expected, "{name}: the status");
        statuses[usize::from(status)] += 1;
        let err = String::from_utf8(err)
            .expect("text")
            .replace(&*spelled, "<place>");
        assert_eq!(err, text(case, "stderr"), "{name}: standard error");
        assert_eq!(
            String::from_utf8(out).expect("text"),
            text(case, "stdout"),
            "{name}"
        );
        let output = text(case, "output").replace("<place>", &spelled);
        let left = listing(Path::new(&output), place);
        let expected = case.get("evidence").expect("evidence").clone().sorted();
        assert_eq!(left, expected, "{name}: the evidence");
        let directory = fs::symlink_metadata(&output)
            .ok()
            .filter(fs::Metadata::is_dir);
        let mode = directory.map(|metadata| u64::from(metadata.permissions().mode() & 0o777));
        let recorded = case.get("outputMode").expect("a mode").whole();
        assert_eq!(
            mode.map(i128::from),
            recorded,
            "{name}: the evidence directory's mode"
        );
        directories += usize::from(mode.is_some());
        let names = left.names();
        let kind = if names.contains("report.json") {
            0
        } else if names.contains("failure.json") {
            1
        } else {
            2
        };
        evidence[kind] += 1;
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
    }
    assert_eq!(cases.len(), 96);
    assert_eq!(statuses, [20, 76]);
    assert_eq!(evidence, [27, 51, 18]);
    assert_eq!(directories, 81);
}

#[test]
fn the_codeql_group_hands_triage_its_own_command_line() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let line =
        |words: &[&str]| -> Vec<String> { words.iter().map(|word| (*word).to_owned()).collect() };
    let run = crate::codeql::run;
    assert_eq!(
        run(root, &line(&["triage", "--help"])),
        std::process::ExitCode::SUCCESS
    );
    assert_eq!(
        run(root, &line(&["triage"])),
        std::process::ExitCode::from(2)
    );
    assert_eq!(
        run(root, &line(&["triage", "lint"])),
        std::process::ExitCode::from(2)
    );
}

#[test]
fn every_recorded_request_is_validated_alike() {
    let recorded = recording(RECORDED);
    let cases = items(&recorded, "validations");
    let mut tally = [0; 2];
    for case in cases {
        let flag = |name: &str| case.get(name) == Some(&Document::Bool(true));
        let cache = case.get("sourceCache").and_then(Document::text);
        let options = Options {
            mode: text(case, "mode").to_owned(),
            input: None,
            output: "/o".into(),
            repository: "swiftraccoon/simplestChat".to_owned(),
            revision: text(case, "revision").to_owned(),
            reference: "refs/heads/main".to_owned(),
            authorize_dismissals: flag("authorizeDismissals"),
            source_cache: cache.map(Into::into),
            include_vendor: flag("includeVendor"),
        };
        let marker = case.get("automated").and_then(Document::text);
        let automated = crate::scope::automated(|name| Some(name) == marker);
        let here = super::validate_request(&options, automated).map_err(|fault| fault.0);
        let result = case.get("result").expect("a result");
        match result.get("refused").and_then(Document::text) {
            Some(code) => {
                assert_eq!(here, Err(code), "{options:?} {marker:?}");
                tally[1] += 1;
            }
            None => {
                assert_eq!(here, Ok(()), "{options:?} {marker:?}");
                tally[0] += 1;
            }
        }
    }
    assert_eq!(tally, [12, 228]);
}
