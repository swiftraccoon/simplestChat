//! The command lines of `codeql`, held to what the reference's parsers made
//! of the recorded ones, and `codeql jobs`, held to the recorded runs of
//! the reference.

use super::{Around, CARGO, Line, TOOLS, USAGE, command, parsed};
use crate::{
    budget::{Budget, detect},
    codeql::tests::{items, recording, text, whole},
    document::Document,
    paths,
};
use std::{cell::RefCell, ffi::OsString, path::Path};

const TOOLS_RECORDED: &[u8] = include_bytes!("recorded_tools.json");
const CARGO_RECORDED: &[u8] = include_bytes!("recorded_cargo.json");
const JOBS_RECORDED: &[u8] = include_bytes!("recorded_jobs.json");

/// What a parsed line comes to, as the recording writes the reference's
/// namespace: the value of each option given, a path spelled as its paths
/// spell one.
fn namespace(asked: &[(&'static str, String)], options: &[(&str, &str, bool)]) -> Document {
    let fields = options.iter().filter_map(|(option, field, spelled)| {
        let given = asked
            .iter()
            .find(|(held, _)| held == option)
            .map(|(_, value)| value)?;
        let value = if *spelled {
            paths::path(given.as_ref()).to_string_lossy().into_owned()
        } else {
            given.clone()
        };
        Some(((*field).to_owned(), Document::from(value)))
    });
    Document::Record(fields.collect()).sorted()
}

/// Hold every recorded line of `recorded` to what `read` makes of it, and
/// answer how many were asked, helped and refused.
fn lines(
    recorded: &[u8],
    read: &dyn Fn(&[String]) -> Line,
    options: &[(&str, &str, bool)],
) -> [usize; 3] {
    let recorded = recording(recorded);
    let mut tally = [0; 3];
    for case in items(&recorded, "lines") {
        let argv: Vec<String> = items(case, "argv")
            .iter()
            .map(|word| word.text().expect("a word").to_owned())
            .collect();
        let status = case.get("status").expect("a status");
        match read(&argv) {
            Line::Asked(asked) => {
                assert_eq!(status.text(), Some("parsed"), "{argv:?}");
                let expected = case.get("parsed").cloned().expect("a namespace").sorted();
                assert_eq!(namespace(&asked, options), expected, "{argv:?}");
                tally[0] += 1;
            }
            Line::Help => {
                assert_eq!(status.whole(), Some(0), "{argv:?}");
                tally[1] += 1;
            }
            Line::Refused => {
                assert_eq!(status.whole(), Some(2), "{argv:?}");
                tally[2] += 1;
            }
        }
    }
    tally
}

#[test]
fn every_recorded_tools_line_is_read_as_the_references_parser_read_it() {
    let language = |option: &str, value: &str| {
        option != "--language" || crate::codeql::tools::LANGUAGES.contains(&value)
    };
    let read = |argv: &[String]| parsed(&TOOLS, "--output", argv, &language);
    let options = [
        ("--codeql", "codeql", true),
        ("--language", "language", false),
        ("--output", "output", true),
    ];
    assert_eq!(lines(TOOLS_RECORDED, &read, &options), [18, 7, 22]);
}

#[test]
fn every_recorded_cargo_key_line_is_read_as_the_references_parser_read_it() {
    let read = |argv: &[String]| parsed(&CARGO, "--source-root", argv, &|_, _| true);
    let options = [
        ("--source-root", "source_root", true),
        ("--publication-check", "publication_check", true),
    ];
    assert_eq!(lines(CARGO_RECORDED, &read, &options), [12, 4, 12]);
}

/// Run `arguments` with the caller's variable `CODEQL_BUILD_JOBS` set to
/// `jobs` where it is given, and answer the status and both streams.
fn jobs(
    arguments: &[&str],
    jobs: Option<OsString>,
    budget: &dyn Fn(Option<&str>) -> crate::Result<Budget>,
) -> (u8, String, String) {
    let variable = move |name: &str| {
        (name == "CODEQL_BUILD_JOBS")
            .then(|| jobs.clone())
            .flatten()
    };
    let private = || panic!("no analyzer setup");
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut around = Around {
        variable: &variable,
        search: None,
        system: "Darwin",
        machine: "arm64",
        bundle: "darwin",
        budget,
        private: &private,
        out: &mut out,
        err: &mut err,
    };
    let arguments: Vec<String> = arguments.iter().map(|word| (*word).to_owned()).collect();
    let status = command(Path::new("/nowhere"), &arguments, &mut around);
    let text = |bytes: Vec<u8>| String::from_utf8(bytes).expect("text");
    (status, text(out), text(err))
}

#[test]
fn every_recorded_jobs_run_answers_alike() {
    let recorded = recording(JOBS_RECORDED);
    let mut answered = 0;
    for case in items(&recorded, "runs") {
        let name = text(case, "name");
        let mut arguments = vec!["jobs"];
        arguments.extend(
            items(case, "argv")
                .iter()
                .map(|word| word.text().expect("a word")),
        );
        let (status, out, err) = jobs(&arguments, Some(text(case, "value").into()), &detect);
        assert_eq!(i128::from(status), whole(case, "status"), "{name}");
        assert_eq!(out, text(case, "stdout"), "{name}");
        // The reference's interpreter told the code in its traceback.
        let told = text(case, "raised").strip_prefix("security_tools.ToolError: ");
        let told = told.map(|code| format!("CodeQL resource budget failed: {code}\n"));
        assert_eq!(err, told.unwrap_or_default(), "{name}");
        answered += 1;
    }
    assert_eq!(answered, 11);
}

#[test]
fn jobs_asks_the_budget_with_the_callers_variable_and_prints_its_workers() {
    let asked = RefCell::new(Vec::new());
    let budget = |requested: Option<&str>| {
        asked.borrow_mut().push(requested.map(str::to_owned));
        match requested {
            Some("9") => Err(crate::Fault("codeql_build_jobs_budget")),
            _ => Ok(Budget {
                workers: 3,
                ram_mib: 1,
                query_ram_mib: 1,
            }),
        }
    };
    assert_eq!(
        jobs(&["jobs"], None, &budget),
        (0, "3\n".to_owned(), String::new())
    );
    assert_eq!(jobs(&["jobs", "x"], Some("2".into()), &budget).0, 0);
    let refused = (
        1,
        String::new(),
        "CodeQL resource budget failed: codeql_build_jobs_budget\n".to_owned(),
    );
    assert_eq!(jobs(&["jobs"], Some("9".into()), &budget), refused);
    use std::os::unix::ffi::OsStringExt as _;
    assert_eq!(
        jobs(&["jobs"], Some(OsString::from_vec(vec![0xff])), &budget).0,
        0
    );
    let expected = vec![
        None,
        Some("2".to_owned()),
        Some("9".to_owned()),
        Some(String::new()),
    ];
    assert_eq!(asked.into_inner(), expected);
}

#[test]
fn a_line_of_no_command_of_the_groups_is_refused_with_the_usage() {
    let never = |_: Option<&str>| -> crate::Result<Budget> { panic!("no budget") };
    let usage = format!("{USAGE}\n");
    for arguments in [
        &[][..],
        &["bogus"],
        &["tools"],
        &["cargo-key"],
        &["Jobs"],
        &["tools", "--output"],
    ] {
        assert_eq!(
            jobs(arguments, None, &never),
            (2, String::new(), usage.clone()),
            "{arguments:?}"
        );
    }
    for arguments in [&["tools", "-h"][..], &["cargo-key", "--help"]] {
        assert_eq!(
            jobs(arguments, None, &never),
            (0, usage.clone(), String::new()),
            "{arguments:?}"
        );
    }
    let refused = "Native CodeQL extraction evidence failed validation.\n".to_owned();
    assert_eq!(
        jobs(&["coverage"], None, &never),
        (1, String::new(), refused)
    );
}
