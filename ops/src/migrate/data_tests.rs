//! Every recorded invocation of the Python helper's data steps, replayed
//! command by command against the same machine.

use super::super::client::{Client, Seen, number};
use super::super::snapshot_sql::{CATALOG, METADATA, SETUP};
use super::*;
use crate::host::command::{Commands, Request as Asked};
use crate::host::dialogue::Dialogue;
use crate::host::evidence::ENVIRONMENT;
use crate::host::replay::{
    build, chmod, concrete, expected, held_lock, listed, normalized, placed,
};
use crate::host::state::tests::Scratch;
use crate::host::tests::{Fixed, paths};
use crate::json::InOrder;
use serde_json::{Map, Value};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::PathBuf;
use std::time::Duration;

const RECORDED: &str = include_str!("recorded_data.json");
const ECHO: &str = "\n\\echo <MARKER>\n";
const GIB: u64 = 1024 * 1024 * 1024;
/// Command lines the earlier helper's parser accepted and this one refuses
/// as a syntax error.
const STRICTER: [&str; 4] = [
    "the helper's own inspection of a source",
    "the request named with an equals sign",
    "the request named before the step",
    "the request option shortened",
];
/// What a step is recorded with when its recording says nothing else.
const FREE: u64 = 100 * GIB;

/// A clock whose every reading of the time of day is a second later, so
/// that each record shows when it was written.
struct Ticking(Fixed);

impl crate::host::Clock for Ticking {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        let now = self.0.now.get();
        self.0.now.set(now + chrono::Duration::seconds(1));
        now
    }

    fn monotonic(&self) -> Duration {
        self.0.monotonic()
    }

    fn sleep(&self, duration: Duration) {
        self.0.sleep(duration);
    }
}

fn octal(value: &Value) -> u32 {
    let text = value.as_str().unwrap().strip_prefix("0o").unwrap();
    u32::from_str_radix(text, 8).unwrap()
}

/// A recorded value with every part it names in its place.
fn whole(value: &Json, table: &[Json]) -> Json {
    match value {
        Json::Object(entries, order) => {
            if let (1, Some(named)) = (order.len(), entries.get("$ref")) {
                let place = usize::try_from(named.integer().unwrap()).unwrap();
                return table[place].clone();
            }
            let parts = entries.iter();
            let parts = parts.map(|(name, part)| (name.clone(), whole(part, table)));
            Json::Object(parts.collect(), order.clone())
        }
        Json::Array(items) => Json::Array(items.iter().map(|item| whole(item, table)).collect()),
        other => other.clone(),
    }
}

/// The recorded parts of long records, each whole.
fn values(recorded: &Value) -> Vec<Json> {
    let mut table: Vec<Json> = Vec::new();
    for text in recorded["values"].as_array().unwrap() {
        let value = state::decode(text.as_str().unwrap().as_bytes()).unwrap();
        table.push(whole(&value, &table));
    }
    table
}

/// Recorded entries with each long record written out as the helper wrote
/// it: indented, in recorded order, with a final line break.
fn written(entries: &Map<String, Value>, table: &[Json]) -> Map<String, Value> {
    let mut entries = entries.clone();
    for entry in entries.values_mut() {
        if let Some(record) = entry["text"]["pretty"].as_str() {
            let value = whole(&state::decode(record.as_bytes()).unwrap(), table);
            let text = serde_json::to_string_pretty(&InOrder(&value)).unwrap() + "\n";
            entry["text"] = Value::String(text);
        }
    }
    entries
}

/// One scenario's scratch machine and the evidence directories its steps
/// made so far.
struct Run<'a> {
    base: &'a Path,
    operation: &'a str,
    made: RefCell<Vec<String>>,
}

impl Run<'_> {
    fn directory(&self) -> PathBuf {
        placed(self.base, &format!("srv/migrations/{}", self.operation))
    }

    fn placeholder(made: &str, position: usize) -> String {
        let (step, _) = made.split_once('.').unwrap();
        format!("{step}.<name{}>", position + 1)
    }

    /// Note the evidence directory a step made.
    fn observe(&self) {
        let Ok(entries) = std::fs::read_dir(self.directory()) else {
            return;
        };
        let names = entries.map(|entry| entry.unwrap().file_name().into_string().unwrap());
        let mut made = self.made.borrow_mut();
        let evidence = |name: &String| {
            let step = name
                .split_once('.')
                .and_then(|(step, _)| Action::named(step));
            step.is_some() && !made.contains(name)
        };
        let fresh: Vec<String> = names.filter(evidence).collect();
        assert!(fresh.len() <= 1, "{fresh:?}");
        made.extend(fresh);
    }

    /// This run's text under the recorder's placeholders.
    fn recorded(&self, text: &str) -> String {
        self.observe();
        let mut text = text.to_owned();
        for (position, made) in self.made.borrow().iter().enumerate() {
            text = text.replace(made, &Self::placeholder(made, position));
        }
        normalized(&text, self.base)
    }

    /// A recorded text with its placeholders made this run's.
    fn concrete(&self, text: &str) -> String {
        let mut text = text.to_owned();
        for (position, made) in self.made.borrow().iter().enumerate() {
            text = text.replace(&Self::placeholder(made, position), made);
        }
        concrete(&text, self.base)
    }

    fn path(&self, recorded: &str) -> PathBuf {
        placed(self.base, recorded)
    }

    /// Make one recorded change to the machine between two invocations, and
    /// return the lock when the change is another operation holding it.
    fn edit(&self, edit: &Value) -> Option<std::fs::File> {
        if edit["hold"] == true {
            return Some(held_lock(self.base));
        }
        let path = self.path(edit["path"].as_str().unwrap());
        if !edit["chmod"].is_null() {
            chmod(&path, octal(&edit["chmod"]));
        } else if let Some(old) = edit["old"].as_str() {
            let text = std::fs::read_to_string(&path).unwrap();
            assert_eq!(text.matches(old).count(), 1, "{old}");
            let mode = std::fs::metadata(&path).unwrap().mode() & 0o7777;
            chmod(&path, 0o600);
            std::fs::write(&path, text.replace(old, edit["new"].as_str().unwrap())).unwrap();
            chmod(&path, mode);
        } else if edit["remove"] == true {
            if path.is_dir() && !path.is_symlink() {
                std::fs::remove_dir_all(&path).unwrap();
            } else {
                std::fs::remove_file(&path).unwrap();
            }
        } else if let Some(target) = edit["link"].as_str() {
            let _ = std::fs::remove_file(&path);
            std::os::unix::fs::symlink(self.path(target), &path).unwrap();
        } else if let Some(other) = edit["hardlink"].as_str() {
            std::fs::hard_link(&path, self.path(other)).unwrap();
        } else if edit["directory"] == true {
            let mut parents = std::fs::DirBuilder::new();
            parents.recursive(true).mode(0o700);
            parents.create(&path).unwrap();
            chmod(&path, octal(&edit["mode"]));
        } else {
            let mut parents = std::fs::DirBuilder::new();
            parents.recursive(true).mode(0o700);
            parents.create(path.parent().unwrap()).unwrap();
            let _ = std::fs::remove_file(&path);
            std::fs::write(&path, self.concrete(edit["text"].as_str().unwrap())).unwrap();
            chmod(&path, octal(&edit["mode"]));
        }
        None
    }
}

/// Whether a file is a command's numbered output, which the recorder's
/// scripted runner never wrote.
fn numbered(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap();
    file.len() == 10 && (file.ends_with(".stdout") || file.ends_with(".stderr"))
}

/// The machine of one recorded invocation: every command, status question
/// and database session must be the next recorded one, and gets its
/// recorded answer.
struct Machine<'a> {
    name: &'a str,
    recorded: &'a Value,
    calls: Vec<&'a Value>,
    position: Cell<usize>,
    run: &'a Run<'a>,
    clock: &'a Fixed,
    sessions: &'a [RefCell<Seen>],
    opened: Cell<usize>,
    free: u64,
}

impl<'a> Machine<'a> {
    fn next(&self, kind: &str, argv: &[String]) -> &'a Value {
        let name = self.name;
        let call = self.calls.get(self.position.get());
        let call = call.unwrap_or_else(|| panic!("{name}: unrecorded {kind} {argv:?}"));
        self.position.set(self.position.get() + 1);
        assert_eq!(call["kind"], kind, "{name}: {argv:?}");
        let line = self.recorded["commandLines"][number(&call["argv"])]
            .as_array()
            .unwrap();
        let line: Vec<&str> = line.iter().map(|word| word.as_str().unwrap()).collect();
        let said: Vec<String> = argv.iter().map(|word| self.run.recorded(word)).collect();
        assert_eq!(said, line, "{name}");
        call
    }

    fn answer(&self, call: &Value) -> Vec<u8> {
        let Some(answer) = call.get("answer") else {
            return Vec::new();
        };
        let answer = self.recorded["answers"][number(answer)].as_str().unwrap();
        self.run.concrete(answer).into_bytes()
    }

    fn relative(&self, path: &Path) -> String {
        let path = path.strip_prefix(self.run.base).unwrap();
        self.run.recorded(path.to_str().unwrap())
    }

    fn environment(&self, environment: &[(&str, &str)]) {
        assert_eq!(environment, ENVIRONMENT, "{}", self.name);
    }
}

impl Commands for Machine<'_> {
    fn run(&self, asked: &Asked<'_>) -> Result<Vec<u8>> {
        let name = self.name;
        let call = self.next("run", &asked.argv);
        self.environment(asked.environment);
        let seconds = call["timeout"].as_f64().unwrap();
        assert!((asked.seconds - seconds).abs() < f64::EPSILON, "{name}");
        let data = asked.input.map(|data| std::str::from_utf8(data).unwrap());
        assert_eq!(data, call.get("data").and_then(Value::as_str), "{name}");
        let source = asked.source.map(|source| self.relative(source));
        let recorded = call.get("source").and_then(Value::as_str);
        assert_eq!(source.as_deref(), recorded, "{name}");
        let kept = asked.archive.unwrap();
        let attempt = call["attempt"].as_str().unwrap();
        match call.get("archive").and_then(Value::as_str) {
            Some(archive) => {
                assert_eq!(self.relative(kept), archive, "{name}");
                let room = (4 * GIB).min(self.free.saturating_sub(GIB));
                assert_eq!(asked.ceiling, Some(room), "{name}");
            }
            None => {
                assert_eq!(self.relative(kept.parent().unwrap()), attempt, "{name}");
                assert!(numbered(kept.to_str().unwrap()), "{name}");
                assert_eq!(asked.ceiling, Some(2 * 1024 * 1024), "{name}");
            }
        }
        let errors = asked.errors.unwrap();
        assert_eq!(self.relative(errors.parent().unwrap()), attempt, "{name}");
        // As on a host, both files exist from before the command starts.
        let private = |path: &Path| {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true).mode(0o600);
            options.open(path).unwrap()
        };
        let mut output = private(kept);
        private(errors);
        output.write_all(&self.answer(call)).unwrap();
        match call.get("raises").and_then(Value::as_str) {
            Some("timeout") => Err(Failure::Timeout),
            Some("command") => Err(Failure::Command),
            Some(other) => panic!("{name}: unexpected recorded failure {other}"),
            None => Ok(Vec::new()),
        }
    }

    fn attempt(&self, asked: &Asked<'_>) -> Result<(bool, Vec<u8>)> {
        let name = self.name;
        let call = self.next("attempt", &asked.argv);
        self.environment(asked.environment);
        assert!((asked.seconds - 10.0).abs() < f64::EPSILON, "{name}");
        assert_eq!(asked.limit, 4096, "{name}");
        let plain = asked.input.is_none() && asked.source.is_none() && asked.archive.is_none();
        assert!(plain && asked.errors.is_none(), "{name}");
        Ok((call["status"] == 0, self.answer(call)))
    }

    fn converse<'a>(
        &'a self,
        argv: &[String],
        environment: &[(&str, &str)],
    ) -> Result<Box<dyn Dialogue + 'a>> {
        let call = self.next("session", argv);
        self.environment(environment);
        let seen = &self.sessions[self.opened.get()];
        self.opened.set(self.opened.get() + 1);
        Ok(Box::new(Client::new(call, self.clock, seen)))
    }

    fn free(&self, _: &Path) -> Result<u64> {
        Ok(self.free)
    }
}

/// How this command reports what the earlier helper raised. A refusal keeps
/// its code; a refusal that named a path, a failed command and a reviewed
/// image are told apart from it only by their kind.
fn reported(failure: &Value) -> String {
    if let Some(kind) = failure["kind"].as_str() {
        let reported = match kind {
            "KeyError" => "Key",
            "ValueError" | "JSONDecodeError" | "DuplicateJsonError" | "UnicodeDecodeError" => {
                "Value"
            }
            "TimeoutExpired" => "Timeout",
            "BlockingIOError" => "Refused(\"migration_host_busy\")",
            "RuntimeProfileError" => "Refused(\"destination_image_unreviewed\")",
            "FileNotFoundError" | "NotADirectoryError" | "IsADirectoryError" | "OSError"
            | "PermissionError" | "FileExistsError" => "Os",
            other => panic!("unexpected recorded kind {other}"),
        };
        return reported.to_owned();
    }
    let text = failure["refused"].as_str().unwrap();
    if text.starts_with("Command ") && text.ends_with(" failed; inspect private output") {
        return "Command".to_owned();
    }
    let protection = [
        "Unexpected ownership or permissions",
        "Unexpected file type",
        "Oversized configuration",
    ];
    let image = [
        "Missing local content-addressed image ID",
        "Unexpected image platform or runtime user",
        "Image revision mismatch",
        "Unexpected image entrypoint",
        "Unexpected image execution defaults",
    ];
    let code = match text.split_once(": ") {
        Some((reason, _)) if protection.contains(&reason) => reason,
        _ if image.contains(&text) => "destination_image_unreviewed",
        _ if text == "unfinished_other_release" => "unfinished_host_work",
        _ => text,
    };
    format!("Refused({code:?})")
}

/// The statements one recorded session was asked.
fn asked(call: &Value) -> Vec<String> {
    let groups = call["asked"].as_array().unwrap().iter();
    let groups = groups.map(|group| match group.as_str().unwrap() {
        "<SETUP>" => format!("{SETUP}{ECHO}"),
        "<CATALOG>" => format!("{CATALOG}{ECHO}"),
        "<METADATA>" => format!("{METADATA}{ECHO}"),
        other => other.to_owned(),
    });
    groups.collect()
}

#[test]
fn every_recorded_data_step_is_replayed_command_by_command() {
    let recorded: Value = serde_json::from_str(RECORDED).unwrap();
    let scenarios = recorded["scenarios"].as_array().unwrap();
    let table = values(&recorded);
    let uid = rustix::process::geteuid().as_raw();
    // A group of this user other than the one new files get, when it has
    // one: a directory given to the database's group then shows it.
    let own = rustix::process::getegid().as_raw();
    let groups = rustix::process::getgroups().unwrap();
    let other = groups
        .iter()
        .map(|group| group.as_raw())
        .find(|group| *group != own);
    let gid = other.unwrap_or(own);
    let operation = recorded["operation"].as_str().unwrap();
    assert_eq!(recorded["maxDump"], MAX_DUMP);
    let journal = format!("srv/migrations/{operation}/migration.json");
    let (mut invocations, mut passed, mut usage) = (0, 0, 0);
    for scenario in scenarios {
        let name = scenario["name"].as_str().unwrap();
        let scratch = Scratch::new();
        let base = scratch.path();
        let start = recorded["trees"][number(&scenario["start"])]
            .as_object()
            .unwrap();
        let mut tree = written(start, &table);
        build(base, &tree, 0);
        let run = Run {
            base,
            operation,
            made: RefCell::new(Vec::new()),
        };
        let archive = scenario["dumpLimit"].as_u64().unwrap_or(MAX_DUMP);
        let role = if scenario["source"] == true { "1" } else { "2" };
        let usual = format!("{}\n", role.repeat(32));
        for step in scenario["steps"].as_array().unwrap() {
            let step = &recorded["steps"][number(step)];
            invocations += 1;
            let words = step["arguments"].as_array().unwrap().iter();
            let words = words.map(|word| run.concrete(word.as_str().unwrap()).into());
            let words: Vec<OsString> = words.collect();
            let status = step["status"].as_i64().unwrap();
            let edits = step.get("edits").and_then(Value::as_array);
            let edits = edits.into_iter().flatten();
            let holder: Vec<std::fs::File> = edits.filter_map(|edit| run.edit(edit)).collect();
            let identity = base.join("etc/machine-id");
            match step.get("machine") {
                None => std::fs::write(&identity, &usual).unwrap(),
                Some(Value::String(text)) => std::fs::write(&identity, text).unwrap(),
                Some(_) => drop(std::fs::remove_file(&identity)),
            }
            let Some((action, path)) = parse(&words) else {
                assert!(status == 2 || STRICTER.contains(&name), "{name}: {words:?}");
                usage += 1;
                continue;
            };
            assert_ne!(status, 2, "{name}");
            let calls = recorded["sequences"][number(&step["calls"])].as_array();
            let calls = calls.unwrap().iter();
            let calls: Vec<&Value> = calls.map(|call| &recorded["calls"][number(call)]).collect();
            let sessions = calls.iter().filter(|call| call["kind"] == "session");
            let sessions: Vec<&Value> = sessions.copied().collect();
            let seen: Vec<RefCell<Seen>> = sessions.iter().map(|_| RefCell::default()).collect();
            let clock = Ticking(Fixed::at(recorded["now"].as_str().unwrap()));
            clock.0.tick.set(Duration::from_millis(250));
            let machine = Machine {
                name,
                recorded: &recorded,
                calls,
                position: Cell::new(0),
                run: &run,
                clock: &clock.0,
                sessions: &seen,
                opened: Cell::new(0),
                free: step["free"].as_u64().unwrap_or(FREE),
            };
            let host = Host {
                commands: &machine,
                clock: &clock,
                paths: Paths {
                    root: base.join("srv/simplestchat-public"),
                    config: base.join("etc/simplestchat-public"),
                    work: base.join("run/simplestchat-bench"),
                    ..paths(base)
                },
                owner: uid,
            };
            let rearmed = Cell::new(0_usize);
            let rearm = || rearmed.set(rearmed.get() + 1);
            let owner = uid + u32::from(step["databaseOwner"] == false);
            let database = Database {
                owner,
                group: gid,
                archive,
            };
            let privileged = step.get("euid").is_none_or(|euid| euid == 0);
            let outcome = execute(
                &host,
                action,
                path,
                privileged,
                &rearm,
                database,
                |takeover| Workload::fixture_for(base, uid, takeover),
            );
            drop(holder);
            run.observe();
            let context = format!("{name}: {:?}: {outcome:?}", step["arguments"]);
            // Every recorded command was issued, and no other.
            assert_eq!(machine.position.get(), machine.calls.len(), "{context}");
            for (session, seen) in sessions.iter().zip(&seen) {
                let seen = seen.borrow();
                assert_eq!(seen.asked, asked(session), "{context}");
                let farewell = session.get("farewell").and_then(Value::as_str);
                assert_eq!(seen.farewell.as_deref(), farewell, "{context}");
            }
            // The recorded tree: the one before with what this step changed.
            let removed = step.get("removed").and_then(Value::as_array);
            for removed in removed.into_iter().flatten() {
                tree.remove(removed.as_str().unwrap());
            }
            let changed = recorded["changes"][number(&step["changed"])].as_object();
            let changed = changed.unwrap();
            for (path, entry) in written(changed, &table) {
                tree.insert(path, entry);
            }
            let mut left = BTreeMap::new();
            for (path, (mode, content)) in listed(base) {
                if !numbered(&path) {
                    left.insert(run.recorded(&path), (mode, run.recorded(&content)));
                }
            }
            let wanted = expected(&tree);
            if left != wanted {
                let names: std::collections::BTreeSet<&String> =
                    left.keys().chain(wanted.keys()).collect();
                let differing = names
                    .into_iter()
                    .filter(|name| left.get(*name) != wanted.get(*name));
                let brief = |entry: Option<&(u32, String)>| {
                    entry.map(|(mode, content)| {
                        (
                            format!("{mode:o}"),
                            content.chars().take(900).collect::<String>(),
                        )
                    })
                };
                let differing: Vec<String> = differing
                    .map(|name| {
                        format!(
                            "{name}:\n  left {:?}\n  want {:?}",
                            brief(left.get(name)),
                            brief(wanted.get(name))
                        )
                    })
                    .collect();
                panic!("{context}: the tree differs\n{}", differing.join("\n"));
            }
            match (&outcome, status) {
                (Ok(line), 0) => {
                    let said = match step["stdout"].as_str() {
                        Some(said) => said.to_owned(),
                        None => {
                            assert_eq!(step["stdout"]["report"], "journal", "{context}");
                            // The line reports the journal the step left.
                            let kept = std::fs::read(run.path(&journal)).unwrap();
                            let kept = state::decode(&kept).unwrap();
                            let report = object(vec![
                                ("passed", Json::Bool(true)),
                                ("action", string(action.name())),
                                ("operationId", string(operation)),
                                ("phase", kept.get("phase").cloned().unwrap()),
                                ("result", kept),
                            ]);
                            format!("{}\n", python_dump(&report, false))
                        }
                    };
                    assert_eq!(run.recorded(line), said, "{context}");
                    passed += 1;
                }
                (Err(failure), 1) => {
                    assert_eq!(
                        format!("{failure:?}"),
                        reported(&step["failure"]),
                        "{context}"
                    );
                    assert!(step.get("stdout").is_none(), "{context}");
                }
                _ => panic!("{context}: recorded status {status}"),
            }
            // What was made for the database belongs to its account and group.
            let given = step.get("chowned").and_then(Value::as_array);
            for path in given.into_iter().flatten() {
                let made = std::fs::metadata(run.path(path.as_str().unwrap())).unwrap();
                assert_eq!((made.uid(), made.gid()), (owner, gid), "{context}");
                assert_eq!(made.mode() & 0o7777, 0o700, "{context}");
            }
            // One request to stop is forgotten exactly when a freeze that
            // had begun fails and turns to putting the source back.
            let began = changed.contains_key(&journal);
            let recovering = action == Action::Freeze && status == 1 && began;
            assert_eq!(rearmed.get(), usize::from(recovering), "{context}");
        }
    }
    assert_eq!(
        (scenarios.len(), invocations, passed, usage),
        (394, 618, 203, 10)
    );
}

/// The recorded refusals are exactly the ones the data steps can give,
/// beside the few this command names differently or that no recorded
/// machine reaches.
#[test]
fn the_recording_covers_every_refusal_a_data_step_names() {
    let recorded: Value = serde_json::from_str(RECORDED).unwrap();
    let mut given = std::collections::BTreeSet::new();
    for step in recorded["steps"].as_array().unwrap() {
        if !step["failure"].is_null() {
            let reported = reported(&step["failure"]);
            if let Some(code) = reported.strip_prefix("Refused(\"") {
                given.insert(code.trim_end_matches("\")").to_owned());
            }
        }
    }
    // A termination request, and two checks behind values that are proven
    // earlier in every flow.
    for unrecorded in [
        "migration_interrupted",
        "invalid_database_container",
        "invalid_target_system_identity",
    ] {
        assert!(given.insert(unrecorded.to_owned()), "{unrecorded}");
    }
    let sources = [
        include_str!("request.rs"),
        include_str!("deployment.rs"),
        include_str!("source.rs"),
        include_str!("target.rs"),
        include_str!("data.rs"),
    ];
    // Names in these files that are not refusals.
    let other = ["server_migration", "owner_password", "pg_restore"];
    let mut named = std::collections::BTreeSet::new();
    for word in sources.iter().flat_map(|source| source.split('"')) {
        let code = word
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_');
        if code && word.contains('_') && !other.contains(&word) {
            named.insert(word.to_owned());
        }
    }
    // The refusals of the pieces a step borrows are recorded with it.
    let borrowed = |code: &&String| {
        !named.contains(*code) && !code.contains(' ') && !code.starts_with("postgres_")
    };
    let foreign: Vec<&String> = given.iter().filter(borrowed).collect();
    assert_eq!(foreign, ["database_identity", "database_snapshot_mismatch"]);
    let unrecorded: Vec<&String> = named.difference(&given).collect();
    assert!(unrecorded.is_empty(), "{unrecorded:?}");
}

/// A machine no command may reach.
struct Unreachable;

impl Commands for Unreachable {
    fn run(&self, asked: &Asked<'_>) -> Result<Vec<u8>> {
        panic!("no command may run: {:?}", asked.argv);
    }
}

/// Two checks stand behind values every flow has already proven: a
/// statement is never sent to a container that is not named by its
/// identity, and an owner is never renamed inside a cluster whose identity
/// is not a number.
#[test]
fn a_statement_needs_a_container_identity_and_a_rename_a_cluster_number() {
    use super::super::deployment::sql;
    use super::super::request::Request;
    use crate::host::evidence::Evidence;
    let scratch = Scratch::new();
    let clock = Fixed::at("2026-11-12T04:00:07Z");
    let host = Host {
        commands: &Unreachable,
        clock: &clock,
        paths: paths(scratch.path()),
        owner: 0,
    };
    let request = Request {
        operation: "a".repeat(32),
        source: "https://old.example.test".into(),
        destination: "https://new.example.test".into(),
        revision: "b".repeat(40),
    };
    let mut session = Move {
        host: &host,
        request: &request,
        directory: scratch.path().to_owned(),
        runner: Evidence::new(&host, scratch.path().to_owned()),
        rearm: &|| {},
        database: Database {
            owner: 0,
            group: 0,
            archive: MAX_DUMP,
        },
    };
    let refused = Err(Failure::Refused("invalid_database_container"));
    for container in [
        "simplestchat-public-postgres-1",
        &"A".repeat(64),
        &"1".repeat(63),
    ] {
        assert_eq!(sql(&mut session, container, "SELECT 1;", true), refused);
    }
    let renamed = |cluster: &str| {
        super::super::target::owner_sql(&request.source, &request.destination, cluster)
    };
    let refused = Err(Failure::Refused("invalid_target_system_identity"));
    for cluster in [
        "",
        "74x",
        "7412345678901234567'; DROP TABLE users; --",
        "-1",
    ] {
        assert_eq!(renamed(cluster), refused);
    }
    let statement = renamed("7412345678901234567").unwrap();
    assert!(statement.contains("system_identifier::text<>'7412345678901234567'"));
    assert!(statement.contains("WHERE email='owner@old.example.test' FOR UPDATE"));
    // An origin that is not canonical is refused before any statement.
    let unsafe_origin = super::super::target::owner_sql(
        "https://old'.example.test",
        "https://new.example.test",
        "1",
    );
    assert_eq!(
        unsafe_origin,
        Err(Failure::Refused("invalid_migration_origin"))
    );
}

/// Private files that are not text are a failure of their value, never
/// half-read: a request, the application's settings and a container listing.
#[test]
fn bytes_that_are_not_text_are_not_values() {
    use super::super::deployment::{configuration, owned_container};
    use super::super::request::{Request, request_file};
    use crate::host::command::tests::Scripted;
    use crate::host::evidence::Evidence;
    let scratch = Scratch::new();
    let base = scratch.path();
    let operation = "a".repeat(32);
    let listed = Scripted::new(|asked: &Asked<'_>| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        options
            .open(asked.archive.unwrap())
            .unwrap()
            .write_all(b"\xff\n")
            .unwrap();
        options.open(asked.errors.unwrap()).unwrap();
        Ok(Vec::new())
    });
    let clock = Fixed::at("2026-11-12T04:00:07Z");
    let uid = rustix::process::geteuid().as_raw();
    let host = Host {
        commands: &listed,
        clock: &clock,
        paths: paths(base),
        owner: uid,
    };
    let directory = host.paths.root.join("migrations").join(&operation);
    let mut private = std::fs::DirBuilder::new();
    private.recursive(true).mode(0o700);
    private.create(&directory).unwrap();
    private.create(&host.paths.config).unwrap();
    let write = |path: &Path, bytes: &[u8]| {
        std::fs::write(path, bytes).unwrap();
        chmod(path, 0o600);
    };
    let request = directory.join("request.json");
    write(&request, b"{\"operationId\": \"\xff\"}");
    assert_eq!(request_file(&host, &request).err(), Some(Failure::Value));
    write(&host.paths.config.join("images.json"), b"{}");
    write(
        &host.paths.config.join("app.env"),
        b"ALLOWED_ORIGINS=https://old.example.test\n\xff\n",
    );
    assert_eq!(configuration(&host).err(), Some(Failure::Value));
    let request = Request {
        operation,
        source: "https://old.example.test".into(),
        destination: "https://new.example.test".into(),
        revision: "b".repeat(40),
    };
    let mut session = Move {
        host: &host,
        request: &request,
        directory: directory.clone(),
        runner: Evidence::new(&host, directory),
        rearm: &|| {},
        database: Database {
            owner: uid,
            group: 0,
            archive: MAX_DUMP,
        },
    };
    let listing = owned_container(&mut session, "postgres", true);
    assert_eq!(listing.err(), Some(Failure::Value));
}

/// A phase reaches the record every release on the machine consults before
/// it reaches the move's own journal: when the second write fails, nothing
/// that reads the shared record can take the earlier phase for current.
#[test]
fn a_phase_reaches_the_shared_record_before_the_moves_own_journal() {
    use super::super::deployment::record;
    use super::super::request::Request;
    use crate::host::evidence::Evidence;
    let scratch = Scratch::new();
    let clock = Fixed::at("2026-11-12T04:00:07Z");
    let host = Host {
        commands: &Unreachable,
        clock: &clock,
        paths: paths(scratch.path()),
        owner: 0,
    };
    std::fs::create_dir(&host.paths.root).unwrap();
    let request = Request {
        operation: "a".repeat(32),
        source: "https://old.example.test".into(),
        destination: "https://new.example.test".into(),
        revision: "b".repeat(40),
    };
    // The move's own directory is gone: its journal cannot be written.
    let directory = host.paths.root.join("migrations").join(&request.operation);
    let session = Move {
        host: &host,
        request: &request,
        directory: directory.clone(),
        runner: Evidence::new(&host, directory),
        rearm: &|| {},
        database: Database {
            owner: 0,
            group: 0,
            archive: MAX_DUMP,
        },
    };
    let details = object(vec![("role", string("source"))]);
    let failed = record(&session, "sealing", &details, false);
    assert_eq!(failed.err(), Some(Failure::Os));
    let shared = std::fs::read(host.paths.root.join("release-state.json")).unwrap();
    let shared = state::decode(&shared).unwrap();
    assert_eq!(shared.get("phase").and_then(Json::string), Some("sealing"));
    assert_eq!(shared.get("finalized").and_then(Json::boolean), Some(false));
    assert!(shared.get("request").unwrap().semantic_eq(&request.json()));
}
