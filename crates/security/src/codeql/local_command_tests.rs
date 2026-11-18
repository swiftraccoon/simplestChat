//! Replays of the selections and source identities of `recorded_local.json`
//! and of `recorded_local_main.json`: the command lines the reference's
//! parser read, whole runs of its `main` against the recorded checkout,
//! fake `git`, fake `codeql`, fake helper, scripted cache and budget, held
//! to the same exit status, line on standard output, commands, checks,
//! evidence and `summary.json`; and the workflow's own upload readiness
//! snippet, held to what `codeql upload-ready` answers.

use super::{
    Line, Upload, command, parsed, selected_languages, source_identity, upload, upload_parsed,
};
use crate::{
    codeql::{
        local::{
            Options,
            tests::{
                Replay, Tally, came, listing, placed, placed_text, ported, same_listing, scrub,
                statuses, words,
            },
        },
        tests::{items, planted, recording, text},
    },
    context::{Context, Limits},
    document::Document,
    scope::automated,
    scratch::{Scratch, host_git},
};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    process::Command,
};

const UNITS: &[u8] = include_bytes!("recorded_local.json");
const RECORDED: &[u8] = include_bytes!("recorded_local_main.json");
/// The checks that the shared steps run on their own: the analyzer's
/// version, its download, listing and extraction, and the two snapshots.
const SHARED: [&str; 6] = [
    "codeql-version",
    "codeql-download",
    "codeql-archive-list",
    "codeql-extract",
    "source-inventory",
    "final-source-inventory",
];

#[test]
fn every_recorded_selection_is_made_alike() {
    let recorded = recording(UNITS);
    let mut tally = Tally::default();
    for case in items(&recorded, "selections") {
        let environment = case
            .get("environment")
            .and_then(Document::fields)
            .unwrap_or_default();
        let present = |name: &str| environment.iter().any(|(held, _)| held == name);
        let selected = selected_languages(
            case.get("language").and_then(Document::text),
            case.get("includeVendor") == Some(&Document::Bool(true)),
            automated(present),
        );
        let held = |languages: Vec<String>| {
            Document::List(languages.into_iter().map(Document::from).collect())
        };
        tally.judge(case, came(selected.map_err(Into::into), held), &[], &[]);
    }
    let expected = Tally {
        held: 19,
        refused: 26,
        raised: 0,
        stricter: 0,
        reworded: 0,
    };
    assert_eq!(tally, expected);
}

#[test]
fn every_recorded_source_identity_is_judged_alike() {
    let recorded = recording(UNITS);
    let mut tally = Tally::default();
    for case in items(&recorded, "identities") {
        let name = text(case, "name");
        let scratch = placed(case.get("tree").expect("a tree"), None, None);
        let place = &scratch.0;
        let environment = case.get("environment").expect("an environment");
        let replay = Replay::new(place, environment, &[], false);
        let ambient = |variable: &str| replay.variable(variable);
        let mut context = Context::new(
            &place.join("root"),
            &place.join("out"),
            Limits::REVIEWED,
            &ambient,
        )
        .expect("a context");
        let bound = replay.with(|host| source_identity(&mut context, host, text(case, "revision")));
        tally.judge(case, came(bound, |()| Document::Null), &[], &[]);
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
        let log = fs::read_to_string(place.join("fake/git.log")).unwrap_or_default();
        let logs = case.get("logs").expect("logs");
        assert_eq!(
            Document::from(scrub(&log, place)),
            *logs.get("git").expect("a log"),
            "{name}"
        );
        let left = listing(&place.join("out"), place);
        same_listing(name, &left, case.get("output").expect("an output"), &[]);
    }
    let expected = Tally {
        held: 7,
        refused: 16,
        raised: 1,
        stricter: 0,
        reworded: 0,
    };
    assert_eq!(tally, expected);
}

/// What the recording writes of the reference's namespace of `options`.
fn namespace(options: &Options) -> Document {
    let spelled = |path: &Option<PathBuf>| match path {
        Some(path) => Document::from(path.to_string_lossy().as_ref()),
        None => Document::Null,
    };
    Document::record([
        ("codeql", spelled(&options.codeql)),
        ("database_cache", spelled(&options.database_cache)),
        ("include_vendor", options.include_vendor.into()),
        ("language", options.language.clone().into()),
        (
            "openssl_prefix",
            options.openssl_prefix.to_string_lossy().as_ref().into(),
        ),
        ("output", options.output.to_string_lossy().as_ref().into()),
        ("revision", options.revision.as_str().into()),
        ("rust_cargo_cache", spelled(&options.rust_cargo_cache)),
        ("suite", options.suite.as_str().into()),
    ])
}

/// The words of the list `name` of `value`, each owned.
fn arguments(value: &Document, name: &str) -> Vec<String> {
    words(value, name).into_iter().map(str::to_owned).collect()
}

#[test]
fn every_recorded_command_line_is_read_alike() {
    let recorded = recording(RECORDED);
    let revision = text(&recorded, "revision").to_owned();
    // The cache tool is the port's own option: `--c` now begins two options,
    // and `--ca` and `--cache-tool` name it where the reference had none.
    let tool = |cache: bool| {
        let line = ["--revision", &revision, "--output", "/o"]
            .map(str::to_owned)
            .to_vec();
        let mut found = match parsed(Path::new("/checkout"), &line) {
            Line::Asked(options) => options,
            other => panic!("{other:?}"),
        };
        if cache {
            found.cache_tool = Some("/t".into());
        }
        found
    };
    let common = ["--revision", revision.as_str(), "--output", "/o"];
    let different: [(Vec<&str>, Line<Options>); 4] = [
        ([&common[..], &["--c", "/c"]].concat(), Line::Refused),
        ([&common[..], &["--c=/c"]].concat(), Line::Refused),
        (
            [&common[..], &["--ca", "/t"]].concat(),
            Line::Asked(tool(true)),
        ),
        (
            [&common[..], &["--cache-tool", "/t"]].concat(),
            Line::Asked(tool(true)),
        ),
    ];
    let mut tally = [0; 4];
    for case in items(&recorded, "lines") {
        let line = arguments(case, "arguments");
        let here = parsed(Path::new("/checkout"), &line);
        let result = case.get("result").expect("a result");
        let changed = different.iter().find(|(other, _)| *other == line);
        if let Some((_, expected)) = changed {
            assert_eq!(&here, expected, "{line:?}");
            tally[3] += 1;
        } else if let Some(namespaced) = result.get("parsed") {
            let Line::Asked(options) = here else {
                panic!("{line:?} was parsed there: {here:?}");
            };
            assert_eq!(namespace(&options), namespaced.clone().sorted(), "{line:?}");
            assert_eq!(options.cache_tool, None, "{line:?}");
            tally[0] += 1;
        } else if result.get("exit").and_then(Document::whole) == Some(0) {
            assert_eq!(here, Line::Help, "{line:?}");
            tally[1] += 1;
        } else {
            assert_eq!(
                result.get("exit").and_then(Document::whole),
                Some(2),
                "{line:?}"
            );
            assert_eq!(here, Line::Refused, "{line:?}");
            tally[2] += 1;
        }
    }
    assert_eq!(tally, [29, 9, 26, 4]);
}

/// The base tree of every whole run with the changes of one.
fn merged(base: &Document, changes: &Document) -> Document {
    let mut fields = base.fields().expect("a tree").to_vec();
    for (name, spec) in changes.fields().unwrap_or_default() {
        match fields.iter_mut().find(|(held, _)| held == name) {
            Some(field) => field.1 = spec.clone(),
            None => fields.push((name.clone(), spec.clone())),
        }
    }
    Document::Record(fields)
}

/// Whether the recorded cache event `event` is one the reference's own
/// cache module refused before it read anything, which the port refuses
/// before it asks its cache tool.
fn before_the_tool(event: &Document) -> bool {
    let answer = event.get("answer").expect("an answer");
    answer.get("real") == Some(&Document::Bool(true))
}

/// The events of the recorded cache module that the port asks its cache
/// tool for.
fn asked_of_the_tool(case: &Document) -> Vec<&Document> {
    let events = items(case, "events").iter();
    events
        .filter(|event| event.get("cache").is_some() && !before_the_tool(event))
        .collect()
}

/// The cache tool's operation for what the reference asked its cache
/// module.
fn operation(event: &Document) -> &'static str {
    match text(event, "cache") {
        "key" => "key",
        "restore" => "restore",
        "source" => "validate-source",
        "save" => "save",
        other => panic!("no operation {other}"),
    }
}

/// The language of a recorded cache event.
fn cached_language(event: &Document) -> String {
    let arguments = event.get("arguments").expect("arguments");
    match arguments.get("language").and_then(Document::text) {
        Some(language) => language.to_owned(),
        None => {
            let database = text(arguments, "database");
            database.rsplit('/').next().expect("a name").to_owned()
        }
    }
}

/// The check of the cache tool for a recorded cache event.
fn cache_check(event: &Document) -> String {
    let name = if text(event, "cache") == "source" {
        "source"
    } else {
        operation(event)
    };
    format!("codeql-cache-{name}-{}", cached_language(event))
}

/// What the cache tool prints for a recorded answer: a key on its own line,
/// a receipt as one line of JSON in the order of its fields' names, or
/// nothing.
fn printed(event: &Document) -> String {
    let answer = event.get("answer").expect("an answer");
    match answer.get("held") {
        Some(Document::Text(key)) => format!("{key}\n"),
        Some(receipt @ Document::Record(_)) => format!("{}\n", receipt.clone().sorted().compact()),
        _ => String::new(),
    }
}

/// Whether the cache tool refuses a recorded event.
fn refused(event: &Document) -> bool {
    event
        .get("answer")
        .expect("an answer")
        .get("refused")
        .is_some()
}

/// Install the fake cache tool of `programs` below `place`, answering each
/// event of `events` as the reference's cache module was answered, and a
/// restoration with a receipt by `effect`.
fn script_cache(place: &Path, programs: &Document, events: &[&Document], effect: &str) {
    let tool = place.join("bin/cache");
    fs::write(&tool, placed_text(text(programs, "cache"), place)).expect("a fake cache tool");
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).expect("an executable");
    fs::create_dir_all(place.join("fake/cache")).expect("answers");
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for event in events {
        let operation = operation(event);
        let count = counts.entry(operation).or_default();
        *count += 1;
        let stem = place.join(format!("fake/cache/{operation}.{count}"));
        let with = |suffix: &str, content: &str| {
            fs::write(format!("{}.{suffix}", stem.display()), content).expect("an answer");
        };
        if refused(event) {
            with("status", "1\n");
            continue;
        }
        let said = printed(event);
        if !said.is_empty() {
            with("stdout", &said);
        }
        if operation == "restore" && !said.is_empty() {
            with("sh", effect);
        }
    }
}

/// The caller's variables that a key binds, which its check is given.
const KEYED: [&str; 6] = [
    "ACT",
    "LOCAL_CI_RUNNER_IMAGE",
    "ImageOS",
    "ImageVersion",
    "GITHUB_REF",
    "GITHUB_EVENT_NAME",
];

/// The command the port runs for a recorded cache event of a run in the
/// caller's environment `environment`.
fn tool_event(event: &Document, environment: &Document) -> Document {
    let arguments = event.get("arguments").expect("arguments");
    let given = |name: &str| Document::from(text(arguments, name));
    let word = |word: &str| Document::from(word);
    let output = "<place>/out";
    let mut argv = vec![
        word("<place>/bin/cache"),
        word("codeql-cache"),
        word(operation(event)),
    ];
    let (rest, within): (Vec<Document>, u64) = match text(event, "cache") {
        "key" => {
            let mut rest = vec![
                word("--source-root"),
                given("source"),
                word("--language"),
                given("language"),
                word("--suite"),
                given("suite"),
                word("--openssl-prefix"),
                given("openssl"),
                word("--root"),
                given("root"),
            ];
            if arguments.get("includeVendor") == Some(&Document::Bool(true)) {
                rest.push(word("--include-vendor"));
            }
            (rest, 180)
        }
        "restore" => (
            vec![
                word("--directory"),
                given("directory"),
                word("--key"),
                given("key"),
                word("--database"),
                given("database"),
                word("--codeql"),
                given("codeql"),
                word("--output"),
                word(output),
            ],
            600,
        ),
        "source" => (
            vec![
                word("--database"),
                given("database"),
                word("--source-root"),
                given("source"),
                word("--manifest"),
                word("<place>/out/source-manifest.json"),
                word("--language"),
                given("language"),
            ],
            600,
        ),
        _ => (
            vec![
                word("--directory"),
                given("directory"),
                word("--key"),
                given("key"),
                word("--revision"),
                given("revision"),
                word("--language"),
                given("language"),
                word("--codeql"),
                given("codeql"),
                word("--output"),
                word(output),
            ],
            600,
        ),
    };
    argv.extend(rest);
    let keyed = KEYED.iter().filter_map(|name| {
        let value = environment.get(name)?;
        Some(((*name).to_owned(), value.clone()))
    });
    let keyed: Vec<(String, Document)> = keyed.collect();
    let variables = if text(event, "cache") == "key" && !keyed.is_empty() {
        Document::Record(keyed)
    } else {
        Document::Null
    };
    Document::record([
        ("run", cache_check(event).into()),
        ("argv", Document::List(argv)),
        ("cwd", Document::Null),
        ("timeout", within.into()),
        ("accepted", Document::List(vec![Document::from(0_u64)])),
        ("input", "".into()),
        ("env", variables),
    ])
}

/// The commands the port's own checks run for a recorded run: the
/// reference's, but for what the shared steps run on their own, with this
/// program in place of the reference's interpreter, and the cache tool in
/// place of the cache module.
fn expected_events(case: &Document) -> Document {
    let mut found = Vec::new();
    for event in items(case, "events") {
        if let Some(name) = event.get("run").and_then(Document::text) {
            if !SHARED.contains(&name) {
                found.push(ported(event));
            }
        } else if !before_the_tool(event) {
            found.push(tool_event(
                event,
                case.get("environment").expect("an environment"),
            ));
        }
    }
    Document::List(found)
}

/// The number of the evidence of a check, and the rest of its name.
fn numbered(name: &str) -> Option<(usize, &str)> {
    let (digits, rest) = name.split_at_checked(2)?;
    let rest = rest.strip_prefix('-')?;
    digits.parse().ok().map(|number| (number, rest))
}

/// The recorded evidence of a run as the port leaves it: each check after
/// a question of the cache tool numbered one more for it, and each
/// question's own two files of what the tool printed.
fn expected_evidence(case: &Document, asked: &[&Document]) -> Document {
    let evidence = case.get("evidence").expect("evidence");
    let Some(fields) = evidence.fields() else {
        return Document::Null;
    };
    let sequence = |event: &Document| {
        let at = event
            .get("sequence")
            .and_then(Document::whole)
            .expect("a sequence");
        usize::try_from(at).expect("a sequence")
    };
    let mut found: Vec<(String, Document)> = fields
        .iter()
        .map(|(name, entry)| match numbered(name) {
            Some((number, rest)) => {
                let before = asked
                    .iter()
                    .filter(|event| sequence(event) < number)
                    .count();
                (format!("{:02}-{rest}", number + before), entry.clone())
            }
            None => (name.clone(), entry.clone()),
        })
        .collect();
    for (at, event) in asked.iter().enumerate() {
        let number = sequence(event) + 1 + at;
        let check = cache_check(event);
        let file = |said: String| {
            Document::record([
                ("kind", "file".into()),
                ("text", said.into()),
                ("mode", 0o600_u64.into()),
            ])
        };
        found.push((format!("{number:02}-{check}.stdout"), file(printed(event))));
        found.push((format!("{number:02}-{check}.stderr"), file(String::new())));
    }
    Document::Record(found).sorted()
}

/// The recorded summary as the port writes it: the cache tool's checks in
/// their places, and the refusal of the tool where the reference's module
/// refused.
fn expected_summary(case: &Document, asked: &[&Document]) -> Option<String> {
    let summary = case.get("summary").and_then(Document::text)?;
    if asked.is_empty() {
        return Some(summary.to_owned());
    }
    let mut document = Document::read(summary.as_bytes()).expect("a summary");
    let mut checks = items(&document, "checks").to_vec();
    for (inserted, event) in asked.iter().enumerate() {
        let at = event
            .get("checks")
            .and_then(Document::whole)
            .expect("a place");
        let at = usize::try_from(at).expect("a place") + inserted;
        let status = u64::from(refused(event));
        let check = Document::record([
            ("name", cache_check(event).into()),
            ("exitStatus", status.into()),
            (
                "elapsedSeconds",
                Document::Number(serde_json::Number::from_f64(0.0).expect("zero")),
            ),
        ]);
        checks.insert(at, check);
    }
    document.put("checks", Document::List(checks));
    if let Some(last) = asked.last().filter(|event| refused(event)) {
        let failure = format!("scanner_command_failed_{}", cache_check(last));
        document.put("failure", failure.into());
    }
    Some(document.pretty())
}

/// `listing` with the digest of every key of the Rust extractor's
/// dependency build left out: a key binds the absolute path of the copy of
/// the sources, which is the case's own place. The key is the step-one
/// function's, replayed with fixed paths in its own tests.
fn unkeyed(listing: &Document) -> Document {
    let marker = "codeql-rust-cargo-v1-";
    let unkey = |text: &str| {
        let mut kept = String::new();
        let mut rest = text;
        while let Some(at) = rest.find(marker) {
            let key = &rest[at..];
            let end = key.find('"').unwrap_or(key.len());
            let cut = key[..end].rfind('-').map_or(end, |dash| dash + 1);
            kept.push_str(&rest[..at]);
            kept.push_str(&key[..cut]);
            kept.push_str("<digest>");
            rest = &key[end..];
        }
        kept.push_str(rest);
        kept
    };
    let entries = listing
        .fields()
        .unwrap_or_default()
        .iter()
        .map(|(name, entry)| {
            let mut entry = entry.clone();
            if let Some(text) = entry.get("text").and_then(Document::text) {
                let unkeyed = unkey(text);
                entry.put("text", unkeyed.into());
            }
            (name.clone(), entry)
        });
    Document::Record(entries.collect())
}

/// `text` with every check's time written as the recorder's clock gave it.
fn untimed(text: &str) -> String {
    let marker = "\"elapsedSeconds\": ";
    let mut kept = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(marker) {
        let after = &rest[at + marker.len()..];
        let length = after
            .find(|letter: char| !(letter.is_ascii_digit() || ".e-+".contains(letter)))
            .unwrap_or(after.len());
        let elapsed: f64 = after[..length].parse().expect("a time");
        assert!(elapsed >= 0.0, "a time of {elapsed}");
        kept.push_str(&rest[..at + marker.len()]);
        kept.push_str("0.0");
        rest = &after[length..];
    }
    kept.push_str(rest);
    kept
}

/// A recorded whole run as it ran here: its place, its host, its exit
/// status and what it told on standard error.
struct Ran {
    scratch: Scratch,
    replay: Replay,
    status: u8,
    err: String,
}

/// Run the whole run `case` of `recorded` here: its tree, and its line with
/// the cache tool where it asks for a database cache, whose answers `edit`
/// may change; what the run says on standard output goes to `out`.
fn rerun(recorded: &Document, case: &Document, edit: &dyn Fn(&Path), out: &mut dyn Write) -> Ran {
    let base = recorded.get("base").expect("a base");
    let programs = recorded.get("programs").expect("programs");
    let effect = text(recorded, "restoreEffect");
    let scratch = Scratch::new("codeql-local-main");
    let place = scratch.0.clone();
    planted(&place, &merged(base, case.get("changes").expect("changes")));
    let mut line: Vec<String> = words(case, "arguments")
        .into_iter()
        .map(|word| placed_text(word, &place))
        .collect();
    if line.iter().any(|word| word == "--database-cache") {
        script_cache(&place, programs, &asked_of_the_tool(case), effect);
        edit(&place);
        line.extend([
            "--cache-tool".to_owned(),
            format!("{}/bin/cache", place.display()),
        ]);
    }
    let linux = text(case, "platform") == "linux";
    let budgets = items(case, "budgets");
    let environment = case.get("environment").expect("an environment");
    let replay = Replay::new(&place, environment, budgets, linux);
    let mut err = Vec::new();
    let status = replay.with(|host| command(&place.join("root"), &line, host, out, &mut err));
    let err = String::from_utf8(err).expect("text");
    Ran {
        scratch,
        replay,
        status,
        err,
    }
}

#[test]
fn every_recorded_main_leaves_the_same_evidence() {
    let recorded = recording(RECORDED);
    let cases = items(&recorded, "mains");
    let mut statuses = [0; 2];
    let mut stopped = 0;
    let mut summaries = 0;
    let mut cached = 0;
    let mut directories = 0;
    for case in cases {
        let name = text(case, "name");
        let mut out = Vec::new();
        let Ran {
            scratch,
            replay,
            status,
            err,
        } = rerun(&recorded, case, &|_| {}, &mut out);
        let place = &scratch.0;
        let asked = asked_of_the_tool(case);
        cached += usize::from(words(case, "arguments").contains(&"--database-cache"));
        let result = case.get("result").expect("a result");
        let raised = result.get("raised").and_then(Document::text);
        match raised {
            Some(class) => {
                assert_eq!(status, 1, "{name}: the status");
                let said = if class == "ToolError" {
                    text(result, "words")
                } else {
                    class
                };
                let line = format!("Local CodeQL stopped: {said}\n");
                assert_eq!(err, line, "{name}");
                stopped += 1;
            }
            None => {
                let expected = result.get("status").and_then(Document::whole);
                assert_eq!(Some(i128::from(status)), expected, "{name}: the status");
                assert_eq!(err, "", "{name}");
                statuses[usize::from(status)] += 1;
            }
        }
        let out = String::from_utf8(out).expect("text");
        assert_eq!(
            scrub(&out, place),
            text(case, "stdout"),
            "{name}: standard output"
        );
        let requested = Document::List(replay.requested.borrow().clone());
        assert_eq!(
            &requested,
            case.get("requested").expect("requests"),
            "{name}: the budget"
        );
        assert_eq!(
            replay.masks.get(),
            items(case, "masks").len(),
            "{name}: the mask"
        );
        assert_eq!(replay.told(), expected_events(case), "{name}: the commands");
        let output = place.join("out");
        let left = listing(&output, place);
        let evidence = expected_evidence(case, &asked);
        if evidence == Document::Null {
            assert_eq!(left, Document::Null, "{name}: no evidence");
        } else {
            same_listing(name, &left, &evidence, &["summary.json"]);
            let mode = |listing: &Document| {
                listing
                    .get("summary.json")
                    .and_then(|entry| entry.get("mode"))
                    .cloned()
            };
            assert_eq!(mode(&left), mode(&evidence), "{name}: the summary's mode");
        }
        let directory = fs::symlink_metadata(&output)
            .ok()
            .filter(fs::Metadata::is_dir);
        let mode = directory.map(|metadata| i128::from(metadata.permissions().mode() & 0o777));
        let recorded = case.get("outputMode").and_then(Document::whole);
        assert_eq!(mode, recorded, "{name}: the evidence directory's mode");
        directories += usize::from(mode.is_some());
        let summary = fs::read_to_string(output.join("summary.json")).ok();
        assert_eq!(
            summary.as_deref().map(untimed),
            expected_summary(case, &asked),
            "{name}: the summary"
        );
        summaries += usize::from(summary.is_some());
        let logs = case.get("logs").expect("logs");
        for program in ["codeql", "git", "helper", "curl"] {
            let log =
                fs::read_to_string(place.join(format!("fake/{program}.log"))).unwrap_or_default();
            let recorded = logs.get(program).expect("a log");
            assert_eq!(
                Document::from(scrub(&log, place)),
                *recorded,
                "{name}: {program}"
            );
        }
        for (tree, recorded) in case
            .get("trees")
            .and_then(Document::fields)
            .unwrap_or_default()
        {
            let left = unkeyed(&listing(&place.join(tree), place));
            same_listing(name, &left, &unkeyed(recorded), &[]);
        }
    }
    assert_eq!(cases.len(), 120);
    assert_eq!(statuses, [38, 76]);
    assert_eq!(stopped, 6);
    assert_eq!(summaries, 115);
    assert_eq!(cached, 29);
    assert_eq!(directories, 118);
}

#[test]
fn every_recorded_upload_readiness_of_the_workflow_answers_alike() {
    let recorded = recording(RECORDED);
    let base = recorded.get("base").expect("a base");
    let revision = text(&recorded, "revision");
    let mut statuses = [0; 2];
    for case in items(&recorded, "snippets") {
        let name = text(case, "name");
        let scratch = Scratch::new("codeql-local-snippet");
        let place = &scratch.0;
        planted(place, &merged(base, case.get("tree").expect("a tree")));
        let asked = Upload {
            output: place.join("codeql-local"),
            revision: revision.to_owned(),
            language: "rust".to_owned(),
            categories: vec!["security".to_owned(), "quality-advisory".to_owned()],
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let status = upload(&place.join("root"), &asked, &mut out, &mut err);
        assert_eq!(
            i128::from(status),
            case.get("status")
                .and_then(Document::whole)
                .expect("a status"),
            "{name}"
        );
        assert_eq!(
            String::from_utf8(out).expect("text"),
            text(case, "output"),
            "{name}"
        );
        let raised = text(case, "raised");
        let said = match raised.split_once(": ") {
            Some((class, words)) => {
                let class = class.rsplit('.').next().expect("a class");
                let reason = if class == "ToolError" { words } else { class };
                format!("CodeQL upload readiness failed: {reason}\n")
            }
            None => String::new(),
        };
        assert_eq!(String::from_utf8(err).expect("text"), said, "{name}");
        statuses[usize::from(status)] += 1;
    }
    assert_eq!(statuses, [5, 3]);
}

#[test]
fn an_upload_readiness_line_names_its_evidence_revision_language_and_categories() {
    let line =
        |words: &[&str]| -> Vec<String> { words.iter().map(|word| (*word).to_owned()).collect() };
    let asked = |categories: &[&str]| {
        Line::Asked(Upload {
            output: "/o".into(),
            revision: "r".to_owned(),
            language: "rust".to_owned(),
            categories: categories
                .iter()
                .map(|category| (*category).to_owned())
                .collect(),
        })
    };
    let base = ["--output", "/o", "--revision", "r", "--language", "rust"];
    for (more, expected) in [
        (&[][..], asked(&["security", "quality-advisory"])),
        (&["--category", "security"], asked(&["security"])),
        (
            &["--category", "quality-advisory"],
            asked(&["quality-advisory"]),
        ),
        (
            &["--category", "quality-advisory", "--category", "security"],
            asked(&["quality-advisory", "security"]),
        ),
        (
            &["--category=security", "--cat", "security"],
            asked(&["security", "security"]),
        ),
        (&["--category", "all"], Line::Refused),
        (&["--category"], Line::Refused),
        (&["--category", "--output"], Line::Refused),
        (&["extra"], Line::Refused),
        (&["--bogus"], Line::Refused),
        (&["-h"], Line::Help),
        (&["--help"], Line::Help),
        (&["--language", "python"], {
            let mut found = asked(&["security", "quality-advisory"]);
            if let Line::Asked(upload) = &mut found {
                upload.language = "python".to_owned();
            }
            found
        }),
        (&["--output", "/p/"], {
            let mut found = asked(&["security", "quality-advisory"]);
            if let Line::Asked(upload) = &mut found {
                upload.output = "/p".into();
            }
            found
        }),
    ] {
        let arguments = [line(&base), line(more)].concat();
        assert_eq!(upload_parsed(&arguments), expected, "{more:?}");
    }
    for missing in 0..3 {
        let mut arguments = line(&base);
        arguments.drain(missing * 2..missing * 2 + 2);
        assert_eq!(upload_parsed(&arguments), Line::Refused, "{arguments:?}");
    }
    assert_eq!(upload_parsed(&[]), Line::Refused);
    assert_eq!(upload_parsed(&line(&["-h"])), Line::Help);
}

#[test]
fn an_upload_readiness_command_tells_its_usage_and_refusals() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let line =
        |words: &[&str]| -> Vec<String> { words.iter().map(|word| (*word).to_owned()).collect() };
    let usage = format!("{}\n", super::UPLOAD_USAGE);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(
        super::upload_command(root, &line(&["--help"]), &mut out, &mut err),
        0
    );
    assert_eq!(
        (String::from_utf8(out).expect("text"), err),
        (usage.clone(), Vec::new())
    );
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(
        super::upload_command(root, &line(&["--output"]), &mut out, &mut err),
        2
    );
    assert_eq!(
        (out, String::from_utf8(err).expect("text")),
        (Vec::new(), usage)
    );
    let scratch = Scratch::new("codeql-local-upload");
    let output = scratch.0.to_string_lossy().into_owned();
    let asked = [
        "--output",
        &output,
        "--revision",
        "r",
        "--language",
        "c-cpp",
    ];
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(
        super::upload_command(root, &line(&asked), &mut out, &mut err),
        1
    );
    let refusal = "CodeQL upload readiness failed: codeql_vendor_upload_forbidden\n";
    assert_eq!(
        (out, String::from_utf8(err).expect("text")),
        (Vec::new(), refusal.to_owned())
    );
    let asked = ["--output", &output, "--revision", "r", "--language", "rust"];
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(
        super::upload_command(root, &line(&asked), &mut out, &mut err),
        0
    );
    assert_eq!((out, err), (Vec::new(), Vec::new()));
}

#[test]
fn a_database_cache_without_its_tool_is_refused_before_anything_is_made() {
    let recorded = recording(RECORDED);
    let revision = text(&recorded, "revision");
    let scratch = Scratch::new("codeql-local-tool");
    let place = &scratch.0;
    let output = place.join("out");
    let named = |path: &Path| path.to_string_lossy().into_owned();
    let common = [
        "--revision",
        revision,
        "--output",
        &named(&output),
        "--database-cache",
        &named(&place.join("cache")),
    ];
    let environment = Document::record([
        ("PATH", "<place>/bin".into()),
        ("HOME", "<place>/caller".into()),
    ]);
    // A relative output is refused first, as the reference refused it.
    let relative = [
        "--revision",
        revision,
        "--output",
        "out",
        "--database-cache",
        "/c",
    ];
    let relative: Vec<String> = relative.map(str::to_owned).to_vec();
    let replay = Replay::new(place, &environment, &[], false);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let status =
        replay.with(|host| command(&place.join("root"), &relative, host, &mut out, &mut err));
    let said = "Local CodeQL stopped: codeql_output_absolute\n";
    assert_eq!(
        (status, String::from_utf8(err).expect("text")),
        (1, said.to_owned())
    );
    for more in [
        &[][..],
        &["--cache-tool", "bin/cache"],
        &["--cache-tool", ""],
    ] {
        let line: Vec<String> = common
            .iter()
            .chain(more)
            .map(|word| (*word).to_owned())
            .collect();
        let replay = Replay::new(place, &environment, &[], false);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let status =
            replay.with(|host| command(&place.join("root"), &line, host, &mut out, &mut err));
        assert_eq!(status, 1, "{more:?}");
        assert_eq!(out, Vec::<u8>::new(), "{more:?}");
        let said = "Local CodeQL stopped: codeql_cache_tool_required\n";
        assert_eq!(String::from_utf8(err).expect("text"), said, "{more:?}");
        assert!(!output.exists(), "{more:?}: nothing is made");
        assert_eq!(replay.masks.get(), 1, "{more:?}");
        assert_eq!(replay.told(), Document::List(Vec::new()), "{more:?}");
    }
}

/// Run the host's `git` in `directory` with `arguments`, as a fixture.
fn fixture_git(git: &Path, directory: &Path, arguments: &[&str]) -> String {
    let ran = Command::new(git)
        .args(arguments)
        .current_dir(directory)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git runs");
    assert!(ran.status.success(), "{arguments:?}");
    String::from_utf8(ran.stdout).expect("text")
}

#[test]
fn a_commit_that_was_never_pushed_is_bound_and_an_uncommitted_change_is_refused() {
    let git = host_git();
    let scratch = Scratch::new("codeql-local-git");
    let (source, output) = (scratch.0.join("source"), scratch.0.join("output"));
    fs::create_dir_all(&source).expect("a checkout");
    fs::create_dir_all(&output).expect("an output");
    fixture_git(&git, &source, &["init", "--template="]);
    fs::write(source.join("fixture.txt"), "committed\n").expect("a file");
    fixture_git(&git, &source, &["add", "fixture.txt"]);
    let identity = [
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
    ];
    fixture_git(
        &git,
        &source,
        &[&identity[..], &["commit", "-q", "-m", "test: fixture"]].concat(),
    );
    let revision = fixture_git(&git, &source, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    let search = git
        .parent()
        .expect("a directory")
        .to_string_lossy()
        .into_owned();
    let environment = Document::record([
        ("PATH", search.as_str().into()),
        ("HOME", "<place>/caller".into()),
    ]);
    let replay = Replay::new(&scratch.0, &environment, &[], false);
    let ambient = |variable: &str| replay.variable(variable);
    let mut context =
        Context::new(&source, &output, Limits::REVIEWED, &ambient).expect("a context");
    let bound = replay.with(|host| source_identity(&mut context, host, &revision));
    assert_eq!(bound, Ok(()));
    fs::write(source.join("fixture.txt"), "uncommitted\n").expect("a change");
    let bound = replay.with(|host| source_identity(&mut context, host, &revision));
    let refused = bound.map_err(|failure| crate::codeql::local::reason(&failure));
    assert_eq!(refused, Err("codeql_source_not_committed".to_owned()));
}

#[test]
fn the_codeql_group_hands_local_and_upload_ready_their_own_command_lines() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let line =
        |words: &[&str]| -> Vec<String> { words.iter().map(|word| (*word).to_owned()).collect() };
    let run = crate::codeql::run;
    for group in ["local", "upload-ready"] {
        assert_eq!(
            run(root, &line(&[group, "--help"])),
            std::process::ExitCode::SUCCESS
        );
        assert_eq!(run(root, &line(&[group])), std::process::ExitCode::from(2));
        assert_eq!(
            run(root, &line(&[group, "extra"])),
            std::process::ExitCode::from(2)
        );
    }
    // A line that is read runs the command, which keeps its files private
    // for the whole process: the integration tests run it in a child.
}

#[test]
fn every_whole_run_extracts_and_analyzes_as_the_reference_did() {
    let recorded = recording(RECORDED);
    let cases = items(&recorded, "mains");
    let case = |name: &str| {
        let found = cases.iter().find(|case| text(case, "name") == name);
        found.unwrap_or_else(|| panic!("no case {name}"))
    };
    let runs = |case: &Document, check: &str| -> Vec<Vec<String>> {
        let events = items(case, "events").iter();
        let named = events.filter(|event| event.get("run").and_then(Document::text) == Some(check));
        named.map(|event| arguments(event, "argv")).collect()
    };
    // A first-party extraction excludes the vendored tree, and a Rust one
    // its dependencies' sources; a vendor scan excludes nothing.
    for language in ["actions", "javascript-typescript", "python", "rust"] {
        let created = runs(
            case(&format!("{language} alone")),
            &format!("codeql-create-{language}"),
        );
        let configuration =
            "--codescanning-config=<place>/out/source/security/codeql/first-party.yml";
        assert!(
            created[0].iter().any(|word| word == configuration),
            "{language}"
        );
        let dependencies = "--extractor-option=extract_dependencies_as_source=false";
        let kept = created[0].iter().any(|word| word == dependencies);
        assert_eq!(kept, language == "rust", "{language}");
    }
    let created = runs(case("a vendor scan of python"), "codeql-create-python");
    assert!(
        !created[0]
            .iter()
            .any(|word| word.starts_with("--codescanning-config="))
    );
    // The vendored file of the copy is left as it was committed.
    let evidence = case("rust alone").get("evidence").expect("evidence");
    let vendored = evidence
        .get("source/vendor/dependency/lib.rs")
        .expect("a vendored file");
    assert_eq!(text(vendored, "text"), "pub fn dependency() {}\n");
    // The native language is traced in three phases timed apart.
    let native = case("the native language on Linux");
    let names: Vec<&str> = items(native, "events")
        .iter()
        .filter_map(|event| event.get("run").and_then(Document::text))
        .collect();
    let at = names
        .iter()
        .position(|name| *name == "codeql-vendor-integrity")
        .expect("integrity");
    assert_eq!(
        names[at..at + 4],
        [
            "codeql-vendor-integrity",
            "codeql-init-c-cpp",
            "codeql-trace-c-cpp",
            "codeql-finalize-c-cpp"
        ]
    );
}

/// The recorded whole run named `name`.
fn named_case<'a>(recorded: &'a Document, name: &str) -> &'a Document {
    let found = items(recorded, "mains")
        .iter()
        .find(|case| text(case, "name") == name);
    found.unwrap_or_else(|| panic!("no case {name}"))
}

/// What the summary of a run says it failed of.
fn failed_of(ran: &Ran) -> Document {
    let summary = fs::read(ran.scratch.0.join("out/summary.json")).expect("a summary");
    let summary = Document::read(&summary).expect("a summary");
    summary.get("failure").cloned().expect("a failure")
}

/// A recorded whole run, and what is changed of its cache tool's answers.
type Edit<'a> = (&'a Document, &'a dyn Fn(&Path));

#[test]
fn every_answer_of_the_cache_tool_that_is_none_of_its_answers_is_refused() {
    let recorded = recording(RECORDED);
    let hit = named_case(&recorded, "a database cache that hits");
    let miss = named_case(&recorded, "a database cache that misses");
    let receipt = |place: &Path| {
        fs::read_to_string(place.join("fake/cache/restore.1.stdout")).expect("a receipt")
    };
    let write = |place: &Path, name: &str, content: &str| {
        fs::write(place.join("fake/cache").join(name), content).expect("an answer");
    };
    let edits: [Edit<'_>; 12] = [
        (hit, &|place| {
            write(place, "key.1.stdout", "codeql-analyzed-v2-rust-key")
        }),
        (hit, &|place| write(place, "key.1.stdout", "\n")),
        (hit, &|place| write(place, "key.1.stdout", "one\ntwo\n")),
        (hit, &|place| {
            fs::write(place.join("fake/cache/key.1.stdout"), b"\xff\n").expect("an answer");
        }),
        (hit, &|place| write(place, "restore.1.stdout", "{\n")),
        (hit, &|place| write(place, "restore.1.stdout", "[]\n")),
        (hit, &|place| write(place, "restore.1.stdout", "\n")),
        (hit, &|place| {
            let answer = receipt(place).replace("\"reused\":true,", "");
            write(place, "restore.1.stdout", &answer);
        }),
        (hit, &|place| {
            let answer = receipt(place).replacen('{', "{\"more\":1,", 1);
            write(place, "restore.1.stdout", &answer);
        }),
        (hit, &|place| {
            let answer = receipt(place).replacen('{', "{\"key\":\"another\",", 1);
            write(place, "restore.1.stdout", &answer);
        }),
        (miss, &|place| write(place, "save.1.stdout", "{}\n")),
        (miss, &|place| {
            fs::remove_file(place.join("fake/cache/save.1.stdout")).expect("an answer");
        }),
    ];
    // The check whose answer is refused is the run's last.
    let refused_at = [
        "codeql-cache-key-rust",
        "codeql-cache-key-rust",
        "codeql-cache-key-rust",
        "codeql-cache-key-rust",
        "codeql-cache-restore-rust",
        "codeql-cache-restore-rust",
        "codeql-cache-restore-rust",
        "codeql-cache-restore-rust",
        "codeql-cache-restore-rust",
        "codeql-cache-restore-rust",
        "codeql-cache-save-rust",
        "codeql-cache-save-rust",
    ];
    for (at, (case, edit)) in edits.iter().enumerate() {
        let ran = rerun(&recorded, case, edit, &mut Vec::new());
        assert_eq!(ran.status, 1, "answer {at}");
        assert_eq!(
            failed_of(&ran),
            Document::from("codeql_cache_tool_answer"),
            "answer {at}"
        );
        let summary = fs::read(ran.scratch.0.join("out/summary.json")).expect("a summary");
        let summary = Document::read(&summary).expect("a summary");
        let last = items(&summary, "checks").last().expect("a check");
        assert_eq!(text(last, "name"), refused_at[at], "answer {at}");
    }
    // A receipt is taken in the reference's order whatever the order the
    // tool prints it in, and a key without a line end after it is refused.
    let reversed = |place: &Path| {
        let answer = Document::read(receipt(place).as_bytes()).expect("a receipt");
        let mut fields = answer.fields().expect("a record").to_vec();
        fields.reverse();
        write(
            place,
            "restore.1.stdout",
            &Document::Record(fields).compact(),
        );
    };
    let ran = rerun(&recorded, hit, &reversed, &mut Vec::new());
    assert_eq!(ran.status, 0);
    let summary = fs::read_to_string(ran.scratch.0.join("out/summary.json")).expect("a summary");
    let asked = asked_of_the_tool(hit);
    assert_eq!(Some(untimed(&summary)), expected_summary(hit, &asked));
}

/// A standard output that takes nothing.
struct Refusing;

impl Write for Refusing {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::BrokenPipe.into())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_run_whose_line_cannot_be_told_ends_with_one_after_its_summary() {
    let recorded = recording(RECORDED);
    let case = named_case(&recorded, "rust alone");
    let ran = rerun(&recorded, case, &|_| {}, &mut Refusing);
    assert_eq!((ran.status, ran.err.as_str()), (1, ""));
    let summary = fs::read(ran.scratch.0.join("out/summary.json")).expect("a summary");
    let summary = Document::read(&summary).expect("a summary");
    assert_eq!(summary.get("passed"), Some(&Document::Bool(true)));
    let scratch = Scratch::new("codeql-local-upload-told");
    let output = scratch.0.join("out");
    fs::create_dir_all(&output).expect("an output");
    let tree = named_case(&recorded, "rust alone")
        .get("evidence")
        .expect("evidence");
    let report = text(tree.get("rust-security.sarif").expect("a report"), "text");
    let marker = text(
        tree.get("rust-security.validated.json").expect("a marker"),
        "text",
    );
    fs::write(output.join("rust-security.sarif"), report).expect("a report");
    fs::write(output.join("rust-security.validated.json"), marker).expect("a marker");
    let checkout = Scratch::new("codeql-local-upload-checkout");
    let base = recorded.get("base").expect("a base");
    planted(&checkout.0, base);
    let asked = Upload {
        output,
        revision: text(&recorded, "revision").to_owned(),
        language: "rust".to_owned(),
        categories: vec!["security".to_owned()],
    };
    let mut told = Vec::new();
    assert_eq!(
        upload(&checkout.0.join("root"), &asked, &mut told, &mut Vec::new()),
        0
    );
    assert_eq!(told, b"security=true\n");
    assert_eq!(
        upload(
            &checkout.0.join("root"),
            &asked,
            &mut Refusing,
            &mut Vec::new()
        ),
        1
    );
}
