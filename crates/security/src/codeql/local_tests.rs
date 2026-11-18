//! What the replays of the local analysis share: the host a recorded case
//! ran on, with its environment, budget and witness; the reference's
//! commands as the recorder logged them; the evidence a case left, listed
//! as the recorder listed it; and the tally of how each recorded case came
//! out here.

use super::{
    ANALYZE, CACHE, CACHED, CATEGORIES, CREATE, Host, POLICY, RUST_EXTRACTION_STATISTICS,
    analyze_check, categories, create_check, policy_check, reason,
};
use crate::{
    budget::Budget,
    codeql::{
        tests::{encoded, items, planted, text},
        tools::{AUTOMATED_LANGUAGES, LANGUAGES},
    },
    context::{Failure, Outcome, Run},
    document::Document,
    scratch::Scratch,
};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    ffi::{OsStr, OsString},
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
};

/// `text` with the place `place` written as the recording writes it.
pub(crate) fn scrub(text: &str, place: &Path) -> String {
    text.replace(&*place.to_string_lossy(), "<place>")
}

/// `text` with the recording's place written as the place `place`.
pub(crate) fn placed_text(text: &str, place: &Path) -> String {
    text.replace("<place>", &place.to_string_lossy())
}

/// A whole number of a document.
pub(crate) fn number(value: i128) -> Document {
    let number = match i64::try_from(value) {
        Ok(signed) => serde_json::Number::from(signed),
        Err(_) => serde_json::Number::from(u64::try_from(value).expect("a machine word")),
    };
    Document::Number(number)
}

/// A command a check of the run's own ran, as the recorder logged the
/// reference's: its name, arguments, working directory, deadline, accepted
/// statuses, input and the variables it added.
pub(crate) fn event(name: &str, argv: &[&OsStr], how: &Run<'_>, place: &Path) -> Document {
    let spelled = |text: &OsStr| Document::from(scrub(&text.to_string_lossy(), place));
    let argv = argv.iter().map(|argument| spelled(argument)).collect();
    let directory = how
        .directory
        .map(|directory| spelled(directory.as_os_str()));
    let accepted = how
        .accepted
        .iter()
        .map(|status| number(i128::from(*status)));
    let variables = how
        .variables
        .iter()
        .map(|(name, value)| ((*name).to_owned(), spelled(value)));
    let variables: Vec<(String, Document)> = variables.collect();
    Document::record([
        ("run", name.into()),
        ("argv", Document::List(argv)),
        ("cwd", directory.unwrap_or(Document::Null)),
        ("timeout", how.within.as_secs().into()),
        ("accepted", Document::List(accepted.collect())),
        (
            "input",
            String::from_utf8_lossy(how.input).into_owned().into(),
        ),
        (
            "env",
            if variables.is_empty() {
                Document::Null
            } else {
                Document::Record(variables)
            },
        ),
    ])
}

/// The vendor script the reference ran, as the recordings name it. It was
/// deleted after them; this program's own `vendor` replaced it.
const REFERENCE_VENDOR: &str = "/build/security_vendor.py";

/// The command the reference ran as the commands the port runs: this
/// program's own `codeql triage` where the reference's interpreter ran its
/// triage script, which the recorder wrote as `<triage>`, and its own
/// `vendor` where it ran its vendor script.
pub(crate) fn ported(event: &Document) -> Document {
    let mut event = event.clone();
    let Some(argv) = event.get("argv").and_then(Document::items) else {
        return event;
    };
    let head = argv.get(1).and_then(Document::text).unwrap_or_default();
    let replaced: Option<&[&str]> = if head == "<triage>" {
        Some(&["codeql", "triage"])
    } else if head.ends_with(REFERENCE_VENDOR) {
        Some(&["vendor"])
    } else {
        None
    };
    if let Some(replaced) = replaced {
        let mut argv = argv.to_vec();
        argv.splice(1..2, replaced.iter().map(|word| Document::from(*word)));
        event.put("argv", Document::List(argv));
    }
    event
}

/// The host a recorded case ran on: its environment, the budgets it was
/// answered, this host's names as the recorder named them, and what the
/// run asked of each.
pub(crate) struct Replay {
    /// The case's own place.
    pub place: PathBuf,
    variables: Vec<(String, OsString)>,
    budgets: RefCell<VecDeque<Document>>,
    /// The compilers the run asked the budget for, in order.
    pub requested: RefCell<Vec<Document>>,
    /// How often the run kept its files private.
    pub masks: Cell<usize>,
    /// Every command the run's own checks ran, in order.
    pub events: RefCell<Vec<Document>>,
    linux: bool,
}

impl Replay {
    /// The host of a case at `place` whose caller's environment is
    /// `environment`, whose budgets are `budgets`, on Linux or not.
    pub(crate) fn new(
        place: &Path,
        environment: &Document,
        budgets: &[Document],
        linux: bool,
    ) -> Self {
        let fields = environment.fields().expect("an environment");
        let variables = fields
            .iter()
            .map(|(name, value)| {
                let value = placed_text(value.text().expect("a value"), place);
                (name.clone(), OsString::from(value))
            })
            .collect();
        Self {
            place: place.to_path_buf(),
            variables,
            budgets: RefCell::new(budgets.iter().cloned().collect()),
            requested: RefCell::new(Vec::new()),
            masks: Cell::new(0),
            events: RefCell::new(Vec::new()),
            linux,
        }
    }

    /// The caller's variable `name`.
    pub(crate) fn variable(&self, name: &str) -> Option<OsString> {
        let found = self.variables.iter().find(|(held, _)| held == name);
        found.map(|(_, value)| value.clone())
    }

    /// The next budget the case was answered, for `requested` compilers.
    fn budget(&self, requested: Option<&str>) -> crate::Result<Budget> {
        self.requested.borrow_mut().push(requested.into());
        let answer = self.budgets.borrow_mut().pop_front().expect("a budget");
        if let Some(code) = answer.get("refused").and_then(Document::text) {
            return Err(crate::Fault(Box::leak(code.to_owned().into_boxed_str())));
        }
        let whole = |name: &str| {
            answer
                .get(name)
                .and_then(Document::whole)
                .expect("a number")
        };
        Ok(Budget {
            workers: u64::try_from(whole("workers")).expect("workers"),
            ram_mib: u64::try_from(whole("ramMiB")).expect("memory"),
            query_ram_mib: i64::try_from(whole("queryRamMiB")).expect("memory"),
        })
    }

    /// Run `body` on the host of the case.
    pub(crate) fn with<T>(&self, body: impl FnOnce(&Host<'_>) -> T) -> T {
        let variable = |name: &str| self.variable(name);
        let search = self.variable("PATH");
        let budget = |requested: Option<&str>| self.budget(requested);
        let private = || self.masks.set(self.masks.get() + 1);
        let witness = |name: &'static str, argv: &[&OsStr], how: &Run<'_>| {
            let told = event(name, argv, how, &self.place);
            self.events.borrow_mut().push(told);
        };
        let program = self.place.join("bin/helper");
        let host = Host {
            variable: &variable,
            search: search.as_deref(),
            system: "Linux",
            machine: "x86_64",
            bundle: "darwin",
            linux: self.linux,
            program: &program,
            budget: &budget,
            private: &private,
            witness: &witness,
        };
        body(&host)
    }

    /// The commands the run's own checks ran.
    pub(crate) fn told(&self) -> Document {
        Document::List(self.events.borrow().clone())
    }
}

/// The case's tree planted in a new place of its own, with the recorded
/// checkout's pin and workflow below `root` where the recording gives them.
pub(crate) fn placed(tree: &Document, pin: Option<&str>, workflow: Option<&str>) -> Scratch {
    let scratch = Scratch::new("codeql-local");
    let root = scratch.0.join("root");
    if let Some(pin) = pin {
        fs::create_dir_all(root.join("security")).expect("a checkout");
        fs::write(root.join("security/codeql-toolchain.json"), pin).expect("a pin");
    }
    if let Some(workflow) = workflow {
        fs::create_dir_all(root.join(".github/workflows")).expect("a checkout");
        fs::write(root.join(".github/workflows/codeql.yml"), workflow).expect("a workflow");
    }
    planted(&scratch.0, tree);
    scratch
}

/// Every entry below `root` as the recorder listed it: its kind, its mode,
/// and a file's text, its bytes, or its size and digest.
pub(crate) fn listing(root: &Path, place: &Path) -> Document {
    if root.symlink_metadata().is_err() {
        return Document::Null;
    }
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("a directory") {
            let path = entry.expect("an entry").path();
            let relative = path.strip_prefix(root).expect("below").to_string_lossy();
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
                if data.len() > 4096 {
                    fields.push(("size".to_owned(), data.len().into()));
                    fields.push(("sha256".to_owned(), crate::digest(&data).into()));
                } else {
                    match String::from_utf8(data) {
                        Ok(text) => fields.push(("text".to_owned(), scrub(&text, place).into())),
                        Err(error) => {
                            let bytes = encoded(error.as_bytes());
                            fields.push(("base64".to_owned(), bytes.into()));
                        }
                    }
                }
            }
            let mode = u64::from(metadata.permissions().mode() & 0o777);
            fields.push(("mode".to_owned(), mode.into()));
            found.push((relative.into_owned(), Document::Record(fields)));
        }
    }
    Document::Record(found).sorted()
}

/// Whether the mode `here` is the recorded mode `there`: the same, or the
/// private mode of a file or directory that the reference made with its
/// interpreter's default mode, which the recorder's mask of 022 left open
/// to others. Under the command's own mask of 077 the two are the same.
pub(crate) fn same_mode(there: i128, here: i128) -> bool {
    here == there || (there & 0o077 != 0 && here == there & 0o700)
}

/// Hold the listing `here` to the recorded `there`, entry by entry, the
/// mode of each file and directory by [`same_mode`]; the names of `skipped`
/// are not compared. A link is held to being a link, but its own mode is
/// not compared: that is the host's, not the reference's. A Mac gives a new
/// link what its creator's mask leaves of 0777 (0755 in the recordings),
/// Linux gives every link 0777, and neither looks at it when the link is
/// followed. Panics, naming the case and the entry, at the first difference.
pub(crate) fn same_listing(name: &str, here: &Document, there: &Document, skipped: &[&str]) {
    let kept = |listing: &Document| -> Vec<(String, Document)> {
        let fields = listing.fields().unwrap_or_default();
        let fields = fields
            .iter()
            .filter(|(entry, _)| !skipped.contains(&entry.as_str()));
        fields.cloned().collect()
    };
    let (here, there) = (kept(&here.clone().sorted()), kept(&there.clone().sorted()));
    let names = |fields: &[(String, Document)]| -> Vec<String> {
        fields.iter().map(|(entry, _)| entry.clone()).collect()
    };
    assert_eq!(names(&here), names(&there), "{name}: the entries");
    for ((entry, made), (_, recorded)) in here.iter().zip(&there) {
        let without = |value: &Document| {
            let fields = value.fields().expect("an entry").iter();
            Document::Record(
                fields
                    .filter(|(field, _)| field != "mode")
                    .cloned()
                    .collect(),
            )
        };
        assert_eq!(without(made), without(recorded), "{name}: {entry}");
        if made.get("kind").and_then(Document::text) == Some("link") {
            continue;
        }
        let mode = |value: &Document| value.get("mode").and_then(Document::whole).expect("a mode");
        assert!(
            same_mode(mode(recorded), mode(made)),
            "{name}: {entry} of mode {:o} where it was {:o}",
            mode(made),
            mode(recorded)
        );
    }
}

/// The checks `context` recorded, each as its name and status.
pub(crate) fn statuses(context: &crate::context::Context) -> Document {
    let checks = context.checks().iter().map(|check| {
        let field = |name: &str| {
            let found = check.iter().find(|(held, _)| *held == name);
            found.map(|(_, value)| value.clone()).expect("a field")
        };
        let status = field("exitStatus").as_i64().expect("a status");
        Document::record([
            ("name", field("name").as_str().expect("a name").into()),
            ("exitStatus", number(i128::from(status))),
        ])
    });
    Document::List(checks.collect())
}

/// How the recorded cases came out here: held alike, refused with the
/// reference's code, refused with the class the reference raised, refused
/// on purpose where the reference held, and refused in other words where
/// the reference raised.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Tally {
    pub held: usize,
    pub refused: usize,
    pub raised: usize,
    pub stricter: usize,
    pub reworded: usize,
}

impl Tally {
    /// Hold the case `case` to what came of it here, `here`: a held value
    /// is compared exactly, field order and all. `stricter` names the cases
    /// the reference held that are refused here, and `reworded` those it
    /// raised one class for and that are refused with another, each with
    /// the refusal here.
    pub(crate) fn judge(
        &mut self,
        case: &Document,
        here: std::result::Result<Document, String>,
        stricter: &[(&str, &str)],
        reworded: &[(&str, &str)],
    ) {
        let name = text(case, "name");
        let result = case.get("result").expect("a result");
        if let Some((_, refusal)) = stricter.iter().find(|(held, _)| *held == name) {
            assert!(result.get("held").is_some(), "{name} was held there");
            assert_eq!(here, Err((*refusal).to_owned()), "{name}");
            self.stricter += 1;
        } else if let Some((_, refusal)) = reworded.iter().find(|(held, _)| *held == name) {
            assert!(result.get("raised").is_some(), "{name} was raised there");
            assert_eq!(here, Err((*refusal).to_owned()), "{name}");
            self.reworded += 1;
        } else if let Some(held) = result.get("held") {
            let came = here.unwrap_or_else(|refusal| panic!("{name}: refused {refusal}"));
            assert_eq!(&came, held, "{name}");
            self.held += 1;
        } else if let Some(code) = result.get("refused").and_then(Document::text) {
            assert_eq!(here, Err(code.to_owned()), "{name}");
            self.refused += 1;
        } else {
            let class = text(result, "raised");
            assert_eq!(here, Err(class.to_owned()), "{name}: {class}");
            self.raised += 1;
        }
    }
}

/// What came of a step here, as the tally takes it: the value, or the
/// reason the evidence would give.
pub(crate) fn came<T>(
    outcome: Outcome<T>,
    held: impl FnOnce(T) -> Document,
) -> std::result::Result<Document, String> {
    outcome
        .map(held)
        .map_err(|failure: Failure| reason(&failure))
}

/// The texts of the list `name` of `value`.
pub(crate) fn words<'a>(value: &'a Document, name: &str) -> Vec<&'a str> {
    let listed = items(value, name).iter();
    listed.map(|word| word.text().expect("a word")).collect()
}

#[test]
fn every_check_is_named_as_the_reference_named_it() {
    for (at, language) in AUTOMATED_LANGUAGES.iter().enumerate() {
        assert_eq!(create_check(language), Ok(CREATE[at]));
        assert_eq!(CREATE[at], format!("codeql-create-{language}"));
    }
    assert_eq!(
        create_check("c-cpp"),
        Err(Failure::Fixed("invalid_scanner_check_name"))
    );
    for (at, language) in LANGUAGES.iter().enumerate() {
        for (place, category) in CATEGORIES.iter().enumerate() {
            let analyzed = format!("codeql-analyze-{language}-{category}");
            let policed = format!("codeql-policy-{language}-{category}");
            assert_eq!(ANALYZE[at][place], analyzed);
            assert_eq!(POLICY[at][place], policed);
            assert_eq!(analyze_check(language, category), Ok(ANALYZE[at][place]));
            assert_eq!(policy_check(language, category), Ok(POLICY[at][place]));
        }
    }
    let unnamed = Err(Failure::Fixed("invalid_scanner_check_name"));
    assert_eq!(analyze_check("go", "security"), unnamed);
    assert_eq!(analyze_check("rust", "quality"), unnamed);
    assert_eq!(policy_check("go", "security"), unnamed);
    assert_eq!(policy_check("rust", "all"), unnamed);
    for (at, language) in CACHED.iter().enumerate() {
        let operations = ["key", "restore", "source", "save"];
        for (place, operation) in operations.iter().enumerate() {
            assert_eq!(
                CACHE[at][place],
                format!("codeql-cache-{operation}-{language}")
            );
        }
    }
}

#[test]
fn a_suite_of_all_is_both_and_any_other_is_security() {
    assert_eq!(categories("all"), ["security", "quality-advisory"]);
    for suite in ["security", "", "All", "quality-advisory"] {
        assert_eq!(categories(suite), ["security"], "{suite}");
    }
}

#[test]
fn a_failure_is_named_as_the_reference_named_it() {
    for (failure, said) in [
        (Failure::Fixed("codeql_revision"), "codeql_revision"),
        (Failure::Fixed("codeql_coverage_field"), "KeyError"),
        (Failure::Fixed("codeql_coverage_json"), "JSONDecodeError"),
        (Failure::Fixed("tool_entry_missing"), "FileNotFoundError"),
        (Failure::Fixed("tool_entry_unavailable"), "OSError"),
        (Failure::Fixed("command_timed_out"), "ProcessError"),
        (
            Failure::Fixed("command_stdout_limit_exceeded"),
            "ProcessError",
        ),
        (Failure::Fixed("ValueError"), "ValueError"),
        (
            Failure::Command("codeql-create-rust"),
            "scanner_command_failed_codeql-create-rust",
        ),
        (
            Failure::Prerequisite("git"),
            "missing_security_prerequisite_git",
        ),
    ] {
        assert_eq!(reason(&failure), said, "{failure:?}");
    }
}

#[test]
fn the_rust_counters_are_the_references() {
    let recorded = crate::codeql::tests::recording(include_bytes!("recorded_local.json"));
    assert_eq!(words(&recorded, "labels"), RUST_EXTRACTION_STATISTICS);
}
