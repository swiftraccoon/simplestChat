use super::fixture::{RECORDED as PREPARATIONS, REVISION, hex, plant, planted};
use super::*;
use crate::host::command::Request as Asked;
use crate::host::command::tests::Scripted;
use crate::host::replay::{TOPS, chmod, held_lock, mode};
use crate::host::state::tests::Scratch;
use crate::host::tests::{Fixed, paths};
use serde_json::{Map, Value, json};
use std::os::unix::fs::MetadataExt;

/// The size above which a listed file is named by its size, not its digest.
const LARGE: u64 = 64 * 1024 * 1024;
/// The three directories a recorded name may start with.
const RECORDED: [&str; 3] = ["srv", "etc", "run"];
/// Refusals of the workload lock, which the helper told apart only inside
/// its own process: either unfinished journal, or an unsafe configuration
/// directory.
const LOCK: [&str; 4] = [
    "ReleaseError: Resolve unfinished benchmark cleanup before releasing",
    "ReleaseError: Inspect and recover the unfinished release before starting another",
    "ReleaseError: Unexpected ownership or permissions",
    "OSError",
];

fn fixed(top: &str) -> &'static str {
    TOPS.iter().find(|(name, _)| *name == top).unwrap().1
}

/// Where a recorded name lives under the scratch root.
fn place(base: &Path, recorded: &str) -> PathBuf {
    match recorded.split_once('/') {
        Some((top, rest)) => base.join(fixed(top)).join(rest),
        None => base.join(fixed(recorded)),
    }
}

/// The recorded name of a path under the recorded directories.
fn named(base: &Path, path: &Path) -> String {
    for top in RECORDED {
        if let Ok(rest) = path.strip_prefix(base.join(fixed(top))) {
            let rest = rest.to_str().unwrap();
            return match rest {
                "" => top.to_owned(),
                rest => format!("{top}/{rest}"),
            };
        }
    }
    path.to_str().unwrap().to_owned()
}

/// A recorded entry's bytes.
fn content(recorded: &Value, tree: &Map<String, Value>, name: &str) -> Vec<u8> {
    let entry = &tree[name];
    if let Some(other) = entry["same"].as_str() {
        return content(recorded, tree, other);
    }
    if let Some(blob) = entry["blob"].as_str() {
        return hex(recorded["blobs"][blob].as_str().unwrap());
    }
    let spelled = &entry["text"];
    match spelled.as_u64() {
        Some(index) => {
            let index = usize::try_from(index).unwrap();
            let text = recorded["texts"][index].as_str().unwrap();
            // Padding in front fills a file to an exact size.
            let pad = usize::try_from(entry["pad"].as_u64().unwrap_or_default()).unwrap();
            let mut bytes = vec![b' '; pad];
            bytes.extend_from_slice(text.as_bytes());
            bytes
        }
        None => {
            let count = usize::try_from(spelled["count"].as_u64().unwrap()).unwrap();
            spelled["repeat"]
                .as_str()
                .unwrap()
                .repeat(count)
                .into_bytes()
        }
    }
}

fn directory(path: &Path) {
    std::fs::DirBuilder::new().mode(0o700).create(path).unwrap();
}

/// Build the recorded tree: entries in name order, second names once their
/// first exists, and modes last, deepest first.
fn build(base: &Path, recorded: &Value, tree: &Map<String, Value>, aside: bool) {
    chmod(base, 0o700);
    if aside {
        directory(&base.join("aside"));
        std::os::unix::fs::symlink(base.join("aside"), base.join("srv")).unwrap();
    }
    for top in RECORDED.iter().filter(|top| !base.join(top).exists()) {
        directory(&base.join(top));
    }
    let mut names: Vec<&String> = tree.keys().collect();
    names.sort();
    for name in &names {
        let (entry, path) = (&tree[*name], place(base, name));
        if let Some(link) = entry["link"].as_str() {
            std::os::unix::fs::symlink(place(base, link), &path).unwrap();
        } else if entry["directory"] == true {
            directory(&path);
        } else if let Some(length) = entry["sparse"].as_u64() {
            // A size without the bytes: nothing may read it.
            std::fs::File::create(&path)
                .unwrap()
                .set_len(length)
                .unwrap();
        } else if entry["same"].is_null() {
            std::fs::write(&path, content(recorded, tree, name)).unwrap();
        }
    }
    for name in &names {
        if let Some(other) = tree[*name]["same"].as_str() {
            std::fs::hard_link(place(base, other), place(base, name)).unwrap();
        }
    }
    for name in names.iter().rev() {
        if !tree[*name]["link"].is_string() {
            chmod(&place(base, name), mode(&tree[*name]));
        }
    }
}

/// One retained outcome with its varying parts named, as the recorder
/// names them.
fn report(path: &Path, base: &Path, now: &str) -> Value {
    let mut value: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let fields = value.as_object_mut().unwrap();
    assert_eq!(fields.remove("action").unwrap(), "force-admit");
    for key in ["startedAt", "finishedAt"] {
        assert_eq!(fields[key], now);
        fields[key] = json!("<time>");
    }
    let evidence = PathBuf::from(fields["evidence"].as_str().unwrap());
    assert_eq!(evidence.parent().unwrap(), place(base, "srv/results"));
    assert_eq!(evidence, path.parent().unwrap());
    fields["evidence"] = json!("srv/results/force.<name>");
    value
}

/// Every entry under the recorded directories as the recorder lists it:
/// links told apart by inode, the request and every other name of it without
/// a digest, a retained outcome by its content.
struct Listing<'a> {
    base: &'a Path,
    chosen: Option<(u64, u64)>,
    now: &'a str,
    groups: Vec<(u64, u64)>,
    found: Map<String, Value>,
}

impl Listing<'_> {
    fn walk(&mut self, path: &Path, name: &str) {
        let status = std::fs::symlink_metadata(path).unwrap();
        if status.is_symlink() {
            let target = named(self.base, &std::fs::read_link(path).unwrap());
            self.found.insert(name.to_owned(), json!({"link": target}));
            return;
        }
        let spelled = format!("0o{:o}", status.mode() & 0o7777);
        if status.is_dir() {
            let entry = json!({"directory": true, "mode": spelled});
            self.found.insert(name.to_owned(), entry);
            let children = std::fs::read_dir(path).unwrap();
            let mut children: Vec<String> = children
                .map(|child| child.unwrap().file_name().into_string().unwrap())
                .collect();
            children.sort();
            for child in children {
                let evidence = name == "srv/results" && child.starts_with("force.");
                let shown = if evidence { "force.<name>" } else { &child };
                self.walk(&path.join(&child), &format!("{name}/{shown}"));
            }
            return;
        }
        let mut entry = json!({"mode": spelled});
        let identity = (status.dev(), status.ino());
        if status.nlink() > 1 {
            let known = self.groups.iter().position(|group| *group == identity);
            let group = known.unwrap_or_else(|| {
                self.groups.push(identity);
                self.groups.len() - 1
            });
            entry["group"] = json!(group);
        }
        if Some(identity) == self.chosen {
            entry["request"] = json!(true);
        } else if status.len() > LARGE {
            entry["bytes"] = json!(status.len());
        } else if name.starts_with("srv/results/force.<name>/") {
            entry["report"] = report(path, self.base, self.now);
        } else {
            entry["sha256"] = json!(sha256(path).unwrap());
        }
        self.found.insert(name.to_owned(), entry);
    }
}

fn listing(base: &Path, chosen: Option<(u64, u64)>, now: &str) -> Map<String, Value> {
    let mut listing = Listing {
        base,
        chosen,
        now,
        groups: Vec::new(),
        found: Map::new(),
    };
    for top in RECORDED {
        if std::fs::symlink_metadata(place(base, top)).is_ok() {
            listing.walk(&place(base, top), top);
        }
    }
    listing.found
}

/// What a run changed: entries it added or altered, and names it removed.
fn difference(before: &Map<String, Value>, after: &Map<String, Value>) -> Value {
    let changed = after
        .iter()
        .filter(|(name, entry)| before.get(*name) != Some(*entry));
    let changed: Map<String, Value> = changed
        .map(|(name, entry)| (name.clone(), entry.clone()))
        .collect();
    let removed: Vec<&String> = before
        .keys()
        .filter(|name| !after.contains_key(*name))
        .collect();
    json!({"changed": changed, "removed": removed})
}

/// A recorded word with its recorded path made concrete, spelled as it was.
fn concrete(word: &str, base: &Path) -> OsString {
    for top in RECORDED {
        for prefix in ["", "--request="] {
            if let Some(rest) = word.strip_prefix(&format!("{prefix}{top}/")) {
                let root = base.join(fixed(top));
                return format!("{prefix}{}/{rest}", root.to_str().unwrap()).into();
            }
        }
    }
    word.into()
}

/// What the helper's reasons are called here: a refusal by its own words up
/// to the path it names, every other fault by its class.
fn reason(fault: &Fault) -> String {
    match fault {
        Fault::Refused(text) => format!("ReleaseError: {}", text.split(": ").next().unwrap()),
        Fault::Kind("ArtifactError" | "BlockingIOError") => fault.kind().to_owned(),
        Fault::Kind("ValueError" | "KeyError" | "JSONDecodeError") => "ValueError".to_owned(),
        Fault::Kind(_) => "OSError".to_owned(),
    }
}

/// Two hundred and sixteen recorded invocations of the forced-release helper,
/// replayed on the recorded trees: each is a usage error, a refusal for the
/// recorded reason or an admission, and leaves the recorded tree — what is
/// published under the revision, which names share bytes with the operation's
/// own files, and the retained outcome. The helper's request carried two
/// deployment fields the native one does not; its staging and deployment were
/// stubbed, since admission does neither.
#[test]
fn every_recorded_forced_admission_is_replayed() {
    let recorded: Value = serde_json::from_str(include_str!("recorded_admit.json")).unwrap();
    let scenarios = recorded["scenarios"].as_array().unwrap();
    assert_eq!(scenarios.len(), 216);
    let now = recorded["now"].as_str().unwrap();
    let uid = rustix::process::geteuid().as_raw();
    let (mut usage, mut admitted, mut refused, mut locked) = (0, 0, 0, 0);
    for scenario in scenarios {
        let name = scenario["name"].as_str().unwrap();
        let scratch = Scratch::new();
        let base = scratch.path();
        let start = usize::try_from(scenario["start"].as_u64().unwrap()).unwrap();
        let tree = recorded["trees"][start].as_object().unwrap();
        build(base, &recorded, tree, scenario["aside"] == true);
        let words = scenario["arguments"].as_array().unwrap().iter();
        let words: Vec<OsString> = words
            .map(|word| concrete(word.as_str().unwrap(), base))
            .collect();
        let request = parse(&words);
        // The file the command line names, however the path is spelled.
        let plain = request.as_ref().map(|path| path.components().collect());
        let chosen = plain.and_then(|path: PathBuf| std::fs::metadata(path).ok());
        let chosen = chosen.filter(std::fs::Metadata::is_file);
        let chosen = chosen.map(|status| (status.dev(), status.ino()));
        let before = listing(base, chosen, now);
        let status = scenario["status"].as_i64().unwrap();
        let commands = Scripted::new(
            |asked: &Asked<'_>| -> crate::host::failure::Result<Vec<u8>> {
                panic!("{name}: an admission runs no command, not {:?}", asked.argv)
            },
        );
        let clock = Fixed::at("2026-11-12T00:00:00+00:00");
        let host = Host {
            commands: &commands,
            clock: &clock,
            paths: Paths {
                root: place(base, "srv"),
                config: place(base, "etc"),
                work: place(base, "run"),
                ..paths(base)
            },
            owner: uid,
        };
        let holder = (scenario["busy"] == true).then(|| held_lock(base));
        let outcome = request.as_ref().map(|request| {
            let privileged = scenario["euid"] == 0;
            execute(&host, request, privileged, |takeover| {
                Workload::fixture_for(base, uid, takeover)
            })
        });
        drop(holder);
        let after = listing(base, chosen, now);
        let left = difference(&before, &after);
        // Restore what a scenario made unreadable, so the scratch can go.
        for (name, entry) in tree {
            if entry["directory"] == true {
                chmod(&place(base, name), 0o700);
            }
        }
        let Some(outcome) = outcome else {
            assert_eq!(status, 2, "{name}");
            assert_eq!(left, scenario["after"], "{name}");
            usage += 1;
            continue;
        };
        assert_ne!(status, 2, "{name}");
        let said = outcome.as_ref().err().map(reason);
        let context = format!("{name}: {said:?}");
        assert_eq!(i64::from(outcome.is_err()), status, "{context}");
        let expected = scenario["reason"].as_str();
        let at_lock = said.as_deref() == Some(LOCK[1])
            && expected.is_some_and(|reason| LOCK.contains(&reason));
        if at_lock {
            locked += 1;
        } else {
            assert_eq!(said.as_deref(), expected, "{context}");
        }
        assert_eq!(left, scenario["after"], "{context}");
        match outcome {
            Ok(line) => {
                // The printed line is the retained outcome.
                let printed: Value =
                    serde_json::from_str(line.strip_suffix('\n').unwrap()).unwrap();
                let evidence = PathBuf::from(printed["evidence"].as_str().unwrap());
                let retained = std::fs::read(evidence.join("outcome.json")).unwrap();
                let retained: Value = serde_json::from_slice(&retained).unwrap();
                assert_eq!(printed, retained, "{context}");
                assert_eq!(printed["passed"], true, "{context}");
                assert_eq!(printed["githubAttested"], false, "{context}");
                assert_eq!(
                    printed["ciVerification"], "skipped-explicit-force",
                    "{context}"
                );
                admitted += 1;
            }
            Err(_) => refused += 1,
        }
    }
    assert_eq!((usage, admitted, refused, locked), (4, 23, 189, 7));
}

/// The command line is one request, named once, in either spelling.
#[test]
fn only_one_named_request_is_a_command_line() {
    let words = |line: &[&str]| -> Vec<OsString> { line.iter().map(OsString::from).collect() };
    let path = "/srv/simplestchat-public/forced/x/force.json";
    assert_eq!(
        parse(&words(&["--request", path])),
        Some(PathBuf::from(path))
    );
    assert_eq!(
        parse(&words(&[&format!("--request={path}")])),
        Some(PathBuf::from(path))
    );
    assert_eq!(parse(&words(&["--request="])), Some(PathBuf::new()));
    for refused in [
        &[][..],
        &["--request"],
        &["--request", "--force"],
        &["--request", path, path],
        &["--request", path, "--request", path],
        &["--force", "--request", path],
        &["--request=--force"],
        &["--requests", path],
        &["-r", path],
        &[path],
        &["admit-unsigned", "--request", path],
    ] {
        assert_eq!(parse(&words(refused)), None, "{refused:?}");
    }
    let unspeakable = std::os::unix::ffi::OsStringExt::from_vec(vec![0xff]);
    assert_eq!(parse(&["--request".into(), unspeakable]), None);
}

/// Only the image archive may be large.
#[test]
fn each_member_is_bounded_by_its_kind() {
    assert_eq!(limit("image.tar"), 8 * 1024 * 1024 * 1024);
    for name in ["release.json", "source.json", "outcome.json", REQUEST] {
        assert_eq!(limit(name), 1024 * 1024, "{name}");
    }
}

/// A hash is of the named file itself: a link in its place is not read.
#[test]
fn a_hash_follows_no_link() {
    let scratch = Scratch::new();
    let file = scratch.path().join("file");
    std::fs::write(&file, b"abc").unwrap();
    std::os::unix::fs::symlink(&file, scratch.path().join("link")).unwrap();
    assert_eq!(
        sha256(&file).unwrap(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert!(sha256(&scratch.path().join("link")).is_err());
}

/// The helper's words for what the native preparation names by a code.
fn translated(reason: &str) -> &str {
    let reason = reason.strip_prefix("ReleaseError: ").unwrap_or(reason);
    match reason.strip_prefix("BuildError: ").unwrap_or(reason) {
        "Invalid migration filename" => "invalid_migration_filename",
        "Invalid or duplicate migration version" => "invalid_migration_version",
        "Source archive must contain 1\u{2013}1000 migrations" => "invalid_migration_count",
        other => other,
    }
}

/// Sixty-nine recorded preparations of a forced release by the controller
/// helper: each export is authorized with the recorded request, spelled as
/// the helper spelled it apart from its two deployment fields, or refused
/// for the recorded reason. What is authorized is what the host admits.
#[test]
fn every_recorded_preparation_is_replayed() {
    let recorded: Value = serde_json::from_str(PREPARATIONS).unwrap();
    assert_eq!(recorded["revision"], REVISION);
    let cases = recorded["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 69);
    let stricter = recorded["stricter"].as_array().unwrap();
    let (mut authorized, mut refused, mut tightened) = (0, 0, 0);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let scratch = Scratch::new();
        let base = scratch.path();
        planted(base, &recorded["blobs"], case["tree"].as_object().unwrap());
        let artifact = base.join(case["artifact"].as_str().unwrap());
        let repository = case["repository"].as_str().unwrap();
        let revision = case["revision"].as_str().unwrap();
        let outcome = prepared(&artifact, &base.join("checkout"), repository, revision);
        let said = outcome.as_ref().err().map(|fault| match fault {
            Fault::Refused(text) => text.clone(),
            Fault::Kind("ArtifactError") => "ArtifactError".to_owned(),
            Fault::Kind(_) => "OSError".to_owned(),
        });
        if stricter.iter().any(|stricter| stricter == name) {
            // The helper read a source input through a link.
            assert!(case["reason"].is_null(), "{name}");
            assert_eq!(said.as_deref(), Some("OSError"), "{name}");
            tightened += 1;
            continue;
        }
        assert_eq!(
            said.as_deref(),
            case["reason"].as_str().map(translated),
            "{name}"
        );
        let Ok(outcome) = outcome else {
            refused += 1;
            continue;
        };
        let spelled = String::from_utf8(outcome.request.clone()).unwrap();
        assert_eq!(spelled, case["spelled"].as_str().unwrap(), "{name}");
        let request: Value = serde_json::from_str(&spelled).unwrap();
        assert_eq!(request, case["request"], "{name}");
        assert_eq!(json!(outcome.digests), request["fileDigests"], "{name}");
        // The host's own reading of the request agrees with it.
        let read = validate(&decode(&outcome.request).unwrap()).unwrap();
        assert_eq!(read.files, outcome.digests, "{name}");
        assert_eq!(
            (read.repository.as_str(), read.revision.as_str()),
            (repository, revision)
        );
        authorized += 1;
    }
    assert_eq!((authorized, refused, tightened), (7, 61, 1));
}

/// The controller's request and the export it names are admitted by the
/// host as they are: nothing the one writes is refused by the other.
#[test]
fn an_authorized_export_is_admitted_as_written() {
    let scratch = Scratch::new();
    let base = scratch.path();
    plant(base);
    let revision = REVISION;
    let artifact = base.join("artifact");
    let written = authorize(&artifact, &base.join("checkout"), "owner/repo", revision).unwrap();
    let uid = rustix::process::geteuid().as_raw();
    for top in RECORDED {
        directory(&base.join(top));
        directory(&place(base, top));
    }
    for name in ["results", "releases", "forced"] {
        directory(&place(base, "srv").join(name));
    }
    let operation = place(base, "srv").join("forced").join("0".repeat(32));
    directory(&operation);
    for name in FILES {
        std::fs::copy(artifact.join(name), operation.join(name)).unwrap();
        chmod(&operation.join(name), 0o600);
    }
    std::fs::write(operation.join(REQUEST), &written.request).unwrap();
    chmod(&operation.join(REQUEST), 0o600);
    let commands = Scripted::new(
        |asked: &Asked<'_>| -> crate::host::failure::Result<Vec<u8>> {
            panic!("an admission runs no command, not {:?}", asked.argv)
        },
    );
    let clock = Fixed::at("2026-11-12T00:00:00+00:00");
    let host = Host {
        commands: &commands,
        clock: &clock,
        paths: Paths {
            root: place(base, "srv"),
            config: place(base, "etc"),
            work: place(base, "run"),
            ..paths(base)
        },
        owner: uid,
    };
    let line = execute(&host, &operation.join(REQUEST), true, |takeover| {
        Workload::fixture_for(base, uid, takeover)
    });
    let printed: Value = serde_json::from_str(line.unwrap().trim_end()).unwrap();
    assert_eq!(printed["passed"], true);
    assert_eq!(printed["revision"], revision);
    assert_eq!(printed["fileDigests"], json!(written.digests));
    let published = place(base, "srv").join("releases").join(revision);
    for name in FILES.into_iter().chain([REQUEST]) {
        let (one, other) = (operation.join(name), published.join(name));
        let same = std::fs::metadata(one).unwrap().ino() == std::fs::metadata(other).unwrap().ino();
        assert!(same, "{name}");
    }
    // A fixed code, not a fault's kind, names a refusal to the controller.
    let refused = authorize(&artifact, &base.join("checkout"), "owner", revision);
    assert_eq!(refused.err().as_deref(), Some("invalid_force_repository"));
}

/// A checkout carries at most a thousand migrations.
#[test]
fn a_thousand_migrations_are_listed_and_one_more_is_refused() {
    let scratch = Scratch::new();
    let directory = scratch.path().join("migrations");
    std::fs::create_dir(&directory).unwrap();
    for version in 1..=1000 {
        std::fs::write(
            directory.join(format!("{version:04}_step.sql")),
            b"SELECT 1;\n",
        )
        .unwrap();
    }
    let listed = migrations(scratch.path()).unwrap();
    assert_eq!(listed.len(), 1000);
    assert!(listed.contains_key("1") && listed.contains_key("1000"));
    std::fs::write(directory.join("1001_step.sql"), b"SELECT 1;\n").unwrap();
    let refused = migrations(scratch.path()).unwrap_err();
    assert_eq!(
        refused,
        Fault::Refused("invalid_migration_count".to_owned())
    );
}
