//! Replays of `recorded_tools.json`: what `build/security_codeql_tools.py`
//! made of fixture checkouts, analyzers and query packs, and of the
//! installation of a bundle by fake `curl` and `tar` programs whose texts
//! the recording holds.

use super::{
    Field, Host, LANGUAGES, archive_digest, install, language_pin, pin, scalar, suite, verify,
};
use crate::{
    codeql::tests::{Tally, items, listed, planted, recording, scrubbed, text, whole},
    context::{Context, Limits},
    document::Document,
    scratch::Scratch,
};
use std::{
    ffi::OsString,
    fs,
    os::unix::fs::DirBuilderExt as _,
    path::{Path, PathBuf},
};

const RECORDED: &[u8] = include_bytes!("recorded_tools.json");

/// The code here of what the reference's interpreter raised.
fn raised(class: &str, words: &str) -> String {
    match (class, words) {
        ("KeyError", _) | ("ValueError", "Expected a JSON object" | "Expected a JSON string") => {
            "codeql_tools_field"
        }
        ("ValueError" | "JSONDecodeError" | "DuplicateJsonError" | "UnicodeDecodeError", _) => {
            "codeql_tools_json"
        }
        ("FileNotFoundError", _) => "tool_entry_missing",
        ("OSError" | "FileExistsError" | "NotADirectoryError", _) => "tool_entry_unavailable",
        ("IsADirectoryError", _) => "nonregular_tool_file",
        (other, _) => panic!("unexpected {other}"),
    }
    .to_owned()
}

/// What a step came to here, said as the tally takes it.
fn came<T>(
    place: &Path,
    outcome: crate::context::Outcome<T>,
    held: impl FnOnce(T) -> Document,
) -> Result<Document, String> {
    outcome
        .map(held)
        .map_err(|failure| scrubbed(&failure.to_string(), place))
}

/// A path as the recording writes it.
fn spelled(place: &Path, path: &Path) -> Document {
    scrubbed(&path.to_string_lossy(), place).into()
}

/// The field of metadata that the recording names.
fn field(name: &str) -> Field {
    [
        Field::NAME,
        Field::VERSION,
        Field::CLI_VERSION,
        Field::PRIMARY_LANGUAGE,
        Field::FINALISED,
        Field::OVERLAY_BASE_DATABASE,
        Field::OVERLAY_DATABASE,
        Field::BUILD_MODE,
    ]
    .into_iter()
    .find(|field| field.name == name)
    .expect("a field")
}

/// A fresh place holding the tree of `case`.
fn placed(case: &Document) -> Scratch {
    let scratch = Scratch::new("codeql-tools");
    planted(&scratch.0, case.get("tree").expect("a tree"));
    scratch
}

#[test]
fn every_recorded_pin_is_read_alike() {
    let recorded = recording(RECORDED);
    let mut tally = Tally::default();
    for case in items(&recorded, "pins") {
        let scratch = placed(case);
        tally.judge(
            case,
            came(&scratch.0, pin(&scratch.0), |held| held),
            &raised,
            &[],
        );
    }
    let expected = Tally {
        held: 18,
        refused: 17,
        raised: 15,
        stricter: 0,
    };
    assert_eq!(tally, expected);
    let mut tally = Tally::default();
    for case in items(&recorded, "languagePins") {
        let scratch = placed(case);
        let selected = language_pin(&scratch.0, text(case, "language"));
        tally.judge(case, came(&scratch.0, selected, |held| held), &raised, &[]);
    }
    let expected = Tally {
        held: 5,
        refused: 3,
        raised: 2,
        stricter: 0,
    };
    assert_eq!(tally, expected);
}

#[test]
fn the_checkouts_own_pin_agrees_with_its_workflow() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let pinned = pin(&root).expect("the reviewed pin");
    let languages = pinned.get("languages").expect("languages").names();
    assert_eq!(languages, LANGUAGES.into_iter().collect());
}

#[test]
fn every_recorded_metadata_field_is_found_alike() {
    let recorded = recording(RECORDED);
    let mut tally = Tally::default();
    for case in items(&recorded, "scalars") {
        let found = scalar(
            text(case, "text"),
            field(text(case, "field")),
            text(case, "expected"),
        );
        let found = found
            .map(|()| Document::Null)
            .map_err(|failure| failure.to_string());
        tally.judge(case, found, &raised, &[]);
    }
    let expected = Tally {
        held: 32,
        refused: 19,
        raised: 0,
        stricter: 0,
    };
    assert_eq!(tally, expected);
}

#[test]
fn every_recorded_suite_is_resolved_alike() {
    let recorded = recording(RECORDED);
    let mut tally = Tally::default();
    for case in items(&recorded, "suites") {
        let scratch = placed(case);
        let codeql = scratch.0.join("bundle/codeql");
        let (language, category) = (text(case, "language"), text(case, "category"));
        let resolved = suite(&scratch.0, &codeql, language, category);
        let resolved = came(&scratch.0, resolved, |path| spelled(&scratch.0, &path));
        tally.judge(case, resolved, &raised, &[]);
    }
    let expected = Tally {
        held: 13,
        refused: 13,
        raised: 6,
        stricter: 0,
    };
    assert_eq!(tally, expected);
}

#[test]
fn every_recorded_archive_is_hashed_alike() {
    let recorded = recording(RECORDED);
    let mut tally = Tally::default();
    for case in items(&recorded, "digests") {
        let scratch = placed(case);
        let size = u64::try_from(whole(case, "size")).expect("a size");
        let hashed = archive_digest(&scratch.0.join("archive"), size);
        tally.judge(case, came(&scratch.0, hashed, Document::from), &raised, &[]);
    }
    let expected = Tally {
        held: 4,
        refused: 5,
        raised: 0,
        stricter: 0,
    };
    assert_eq!(tally, expected);
}

/// The caller's environment of a recorded invocation: only `PATH` and
/// `HOME`.
fn ambient(search: OsString, home: PathBuf) -> impl Fn(&str) -> Option<OsString> {
    move |name: &str| match name {
        "PATH" => Some(search.clone()),
        "HOME" => Some(home.clone().into_os_string()),
        _ => None,
    }
}

/// The checks a context recorded, as the recording lists them.
fn checks(context: &Context) -> Document {
    let checks = context.checks().iter().map(|check| {
        let field = |name: &str| {
            check
                .iter()
                .find(|(held, _)| *held == name)
                .map(|(_, value)| value)
        };
        let name = field("name")
            .and_then(|name| name.as_str())
            .expect("a name");
        let status = field("exitStatus").expect("a status");
        Document::from(format!("{name} {status}"))
    });
    Document::List(checks.collect())
}

/// The text of the file `path`, or nothing.
fn read(path: &Path, place: &Path) -> Document {
    let held = fs::read_to_string(path).unwrap_or_default();
    scrubbed(&held, place).into()
}

#[test]
fn every_recorded_verification_is_judged_alike() {
    let recorded = recording(RECORDED);
    let mut tally = Tally::default();
    for case in items(&recorded, "verifies") {
        let scratch = placed(case);
        let place = &scratch.0;
        let output = place.join("output");
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&output)
            .expect("an output");
        if let Some(tree) = case.get("outputTree") {
            planted(&output, tree);
        }
        let ambient = ambient("/usr/bin:/bin".into(), place.join("caller"));
        let mut context =
            Context::new(place, &output, Limits::REVIEWED, &ambient).expect("a context");
        let codeql =
            PathBuf::from(text(case, "codeql").replace("<place>", &place.to_string_lossy()));
        let languages: Vec<&str> = match case.get("languages").and_then(Document::items) {
            Some(languages) => languages
                .iter()
                .map(|word| word.text().expect("a word"))
                .collect(),
            None => LANGUAGES.to_vec(),
        };
        let verified = verify(&mut context, &codeql, &languages);
        let name = text(case, "name");
        tally.judge(
            case,
            came(place, verified, |()| Document::Null),
            &raised,
            &[],
        );
        let moded = ["tmp", "config", "cache", "home"];
        assert_eq!(
            listed(&output, place, &moded),
            case.get("output").cloned().expect("an output").sorted(),
            "{name}"
        );
        assert_eq!(
            read(&place.join("codeql.log"), place),
            *case.get("log").expect("a log"),
            "{name}"
        );
        assert_eq!(
            checks(&context),
            *case.get("checks").expect("checks"),
            "{name}"
        );
    }
    let expected = Tally {
        held: 6,
        refused: 11,
        raised: 8,
        stricter: 0,
    };
    assert_eq!(tally, expected);
}

#[test]
fn every_recorded_installation_is_made_alike() {
    let recorded = recording(RECORDED);
    let bundle = text(&recorded, "bundle");
    let mut tally = Tally::default();
    let raised_here = |class: &str, words: &str| match class {
        "UnicodeDecodeError" => "codeql_archive_listing".to_owned(),
        _ => raised(class, words),
    };
    for case in items(&recorded, "installs") {
        let scratch = placed(case);
        let place = &scratch.0;
        let output = place.join("output");
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&output)
            .expect("an output");
        let search = place.join("bin").into_os_string();
        let ambient = ambient(search.clone(), place.join("caller"));
        let mut context =
            Context::new(place, &output, Limits::REVIEWED, &ambient).expect("a context");
        let host = Host {
            bundle,
            search: Some(&search),
        };
        let installed = install(&mut context, &host);
        let name = text(case, "name");
        tally.judge(
            case,
            came(place, installed, |path| spelled(place, &path)),
            &raised_here,
            &[],
        );
        let sha = "2ad3c93fec9b2f3d0ed1af002a742f2ebbd3b4061770df503b832405489774ea";
        let receipt = format!("codeql-tools/{sha}/receipt.json");
        let moded = [
            "codeql-tools",
            &format!("codeql-tools/{sha}"),
            receipt.as_str(),
        ];
        let cache = listed(&place.join(".cache"), place, &moded);
        assert_eq!(
            cache,
            case.get("cache").cloned().expect("a cache").sorted(),
            "{name}"
        );
        let output_listed = listed(&output, place, &[]);
        assert_eq!(
            output_listed,
            case.get("output").cloned().expect("an output").sorted(),
            "{name}"
        );
        let logs = case.get("logs").expect("logs");
        for log in ["curl.log", "tar.log"] {
            assert_eq!(
                read(&place.join("fake").join(log), place),
                *logs.get(log).expect("a log"),
                "{name} {log}"
            );
        }
        assert_eq!(
            checks(&context),
            *case.get("checks").expect("checks"),
            "{name}"
        );
    }
    let expected = Tally {
        held: 15,
        refused: 34,
        raised: 13,
        stricter: 0,
    };
    assert_eq!(tally, expected);
}

#[test]
fn every_recorded_tools_command_answers_alike() {
    let recorded = recording(RECORDED);
    let bundle = text(&recorded, "bundle");
    let mut answered = 0;
    for case in items(&recorded, "mains") {
        let scratch = placed(case);
        let place = &scratch.0;
        let mut arguments = vec!["tools".to_owned()];
        let words = items(case, "argv")
            .iter()
            .map(|word| word.text().expect("a word"));
        arguments.extend(words.map(|word| word.replace("<place>", &place.to_string_lossy())));
        // A relative output is named from the working directory, which is
        // the crate's here: it leads into the case's place, so that nothing
        // is left in the checkout should it ever be made.
        if let Some(at) = arguments.iter().position(|word| word == "output") {
            let directory = std::env::current_dir().expect("a working directory");
            let up = "../".repeat(directory.components().count() - 1);
            let below = place.join("output");
            let below = below.strip_prefix("/").expect("an absolute place");
            arguments[at] = format!("{up}{}", below.display());
        }
        let search = place.join("bin").into_os_string();
        let variable = ambient(search.clone(), place.join("caller"));
        let budget =
            |_: Option<&str>| -> crate::Result<crate::budget::Budget> { panic!("no budget") };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let kept = std::cell::Cell::new(0);
        let private = || kept.set(kept.get() + 1);
        let mut around = crate::codeql::cli::Around {
            variable: &variable,
            search: Some(&search),
            system: "Darwin",
            machine: "arm64",
            bundle,
            budget: &budget,
            private: &private,
            out: &mut out,
            err: &mut err,
        };
        let status = crate::codeql::cli::command(place, &arguments, &mut around);
        let name = text(case, "name");
        assert_eq!(i128::from(status), whole(case, "status"), "{name}");
        assert_eq!(
            scrubbed(&String::from_utf8(out).expect("text"), place),
            text(case, "stdout"),
            "{name}"
        );
        assert!(err.is_empty(), "{name}");
        let output = place.join("output");
        let shown = if output.is_dir() {
            listed(&output, place, &["", "home", "tmp"])
        } else {
            Document::Null
        };
        assert_eq!(
            shown,
            case.get("output").cloned().expect("an output").sorted(),
            "{name}"
        );
        let cache = listed(&place.join(".cache"), place, &[]);
        assert_eq!(
            cache,
            case.get("cache").cloned().expect("a cache").sorted(),
            "{name}"
        );
        let mode = output.symlink_metadata().ok().map(|held| {
            use std::os::unix::fs::PermissionsExt as _;
            Document::from(u64::from(held.permissions().mode() & 0o777))
        });
        assert_eq!(
            Document::from(mode),
            *case.get("outputMode").expect("a mode"),
            "{name}"
        );
        assert_eq!(kept.get(), 1, "{name}: kept from other accounts once");
        answered += 1;
    }
    assert_eq!(answered, 10);
}

#[test]
fn a_bundle_is_named_as_the_reference_named_its_platform() {
    // The reference took `darwin` for every Mac, and otherwise its
    // interpreter's platform and the machine.
    assert_eq!(super::bundle_platform("darwin", "arm64"), "darwin");
    assert_eq!(super::bundle_platform("darwin", "x86_64"), "darwin");
    assert_eq!(super::bundle_platform("linux", "x86_64"), "linux-x86_64");
    assert_eq!(super::bundle_platform("linux", "aarch64"), "linux-aarch64");
    let machine = rustix::system::uname()
        .machine()
        .to_string_lossy()
        .into_owned();
    let expected = if cfg!(target_os = "macos") {
        "darwin".to_owned()
    } else {
        format!("{}-{machine}", std::env::consts::OS)
    };
    assert_eq!(super::host_bundle(), expected);
}

#[test]
fn the_analyzer_setup_replaces_the_callers_own_temporary_directory() {
    let scratch = Scratch::new("codeql-environment");
    let output = scratch.0.join("output");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&output)
        .expect("an output");
    let ambient = |name: &str| match name {
        "TMPDIR" => Some(OsString::from("/caller/tmp")),
        "HOME" => Some(OsString::from("/caller")),
        _ => None,
    };
    let mut context =
        Context::new(&scratch.0, &output, Limits::REVIEWED, &ambient).expect("a context");
    assert_eq!(context.variable("TMPDIR"), Some("/caller/tmp".as_ref()));
    super::configure_environment(&mut context).expect("the setup");
    assert_eq!(
        context.variable("TMPDIR"),
        Some(output.join("tmp").as_os_str())
    );
    let config = output.join("config");
    assert_eq!(
        context.variable("XDG_CONFIG_HOME"),
        Some(config.as_os_str())
    );
}

#[test]
fn every_recorded_staging_name_is_tried_alike() {
    // The reference's library was given the recorded names in turn, and
    // every name it took was kept.
    let recorded = recording(RECORDED);
    let mut tally = Tally::default();
    for case in items(&recorded, "stagings") {
        let scratch = placed(case);
        let place = &scratch.0;
        let parent =
            PathBuf::from(text(case, "parent").replace("<place>", &place.to_string_lossy()));
        let mut given = items(case, "names")
            .iter()
            .map(|name| name.text().expect("a name"));
        let taken = std::cell::RefCell::new(Vec::new());
        let mut names = || {
            let name = given.next().expect("a name left").to_owned();
            taken.borrow_mut().push(Document::from(name.as_str()));
            Ok(name)
        };
        let made = super::staging(&parent, &mut names);
        crate::codeql::tests::unlocked(place);
        let taking = |class: &str, words: &str| match class {
            "PermissionError" => "tool_entry_unavailable".to_owned(),
            _ => raised(class, words),
        };
        // The name is shown whole: it is the recorded one, not a random one.
        let shown = |path: PathBuf| {
            let shown = path.to_string_lossy();
            Document::from(shown.replace(&*place.to_string_lossy(), "<place>"))
        };
        tally.judge(case, came(place, made, shown), &taking, &[]);
        let expected = case.get("taken").cloned().expect("the names taken");
        assert_eq!(
            Document::List(taken.into_inner()),
            expected,
            "{}",
            text(case, "name")
        );
    }
    let expected = Tally {
        held: 4,
        refused: 0,
        raised: 4,
        stricter: 0,
    };
    assert_eq!(tally, expected);
}
