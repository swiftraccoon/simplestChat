//! Replays of `recorded_gate.json`: what the reference in Python made of a
//! whole synthetic image — `execute` run as written, with only its two
//! commands, the engine, the scanners' installation, the runtime proof and
//! the clock replaced and every input they answered with recorded — and of
//! the database's own preparation; with direct tests of what no recorded
//! case reaches.

use super::{
    Branches, Checkout, Host, Options, Preparation, cache_pin, execute, execute_database,
    scan_reports,
};
use crate::{
    Fault, Result,
    document::Document,
    image::{
        advisories, cache,
        policy::{self, Moment},
        runtime,
        sandbox::{
            Engine, Platform, Sandbox, TOOLS,
            tests::{reopened, texts, walked, whole},
        },
        scan::{Limits, Pins},
        secrets::tests::{text, there},
    },
    scratch::Scratch,
};
use std::{
    cell::RefCell,
    ffi::OsString,
    fs,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
};

const RECORDING: &[u8] = include_bytes!("recorded_gate.json");

fn recording() -> Document {
    Document::read(RECORDING).expect("a recording")
}

fn cases(recording: &Document, name: &str) -> Vec<Document> {
    let held = recording.get(name).and_then(Document::items);
    held.unwrap_or_else(|| panic!("no cases {name}")).to_vec()
}

/// The image, the scanner base and the container the recorder's
/// replacements answered with.
const IMAGE: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const BASE: &str = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const IDENTIFIER: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
/// What the recorder's proof refused with.
const PROOF: &str = "runtime_fixture_refusal";
/// What the two steps that run in process here refuse with where the
/// reference's commands failed.
const EXTRACTION: &str = "image_archive_file";
const AUDIT: &str = "image_elf_audit_failed";
/// The evidence the recorder kept of each case, and the digests of the
/// outcome it named rather than gave, with their files.
const FILES: [&str; 9] = [
    "checks.json",
    "native.json",
    "elf.json",
    "runtime-license-evidence.json",
    "runtime-proof.json",
    "vex.openvex.json",
    "database-status.json",
    "secret-paths.json",
    "secret-selftest.json",
];
const WRITTEN: [(&str, &str); 8] = [
    (
        "runtimeLicenseEvidenceSha256",
        "runtime-license-evidence.json",
    ),
    ("runtimeProofSha256", "runtime-proof.json"),
    ("vexSha256", "vex.openvex.json"),
    ("secretPathMapSha256", "secret-paths.json"),
    ("databaseEvidenceSha256", "database-status.json"),
    ("sbomSha256", "spdx/sbom.spdx.json"),
    ("nativeSha256", "native.json"),
    ("elfSha256", "elf.json"),
];

/// The checkout this crate was built in.
fn repository() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    root.canonicalize().expect("the checkout")
}

/// The checkout the cases were recorded in, as the recording holds it: the
/// pins, the image policy and the reviewed native record of that moment, no
/// reviews, and the reviewed advisory policy as this checkout has it, which
/// must not have changed.
fn checkout(recording: &Document) -> Checkout {
    let held = recording.get("checkout").expect("the checkout");
    let pinned = held.get("pins").expect("the pins");
    let words = |name: &str| text(held, name).to_owned();
    let root = repository();
    let advisory = root.join(advisories::POLICY_PATH);
    let current = crate::digest(&fs::read(&advisory).expect("the advisory policy"));
    assert_eq!(
        current,
        words("advisoryPolicySha256"),
        "the advisory policy"
    );
    Checkout {
        pins: Pins {
            lock: words("lockSha256"),
            syft: text(pinned, "syft").to_owned(),
            grype: text(pinned, "grype").to_owned(),
            gitleaks: text(pinned, "gitleaks").to_owned(),
        },
        image_policy: held.get("imagePolicy").cloned().expect("the image policy"),
        policy_sha256: words("policySha256"),
        exceptions: Vec::new(),
        exceptions_sha256: words("exceptionsSha256"),
        native: held
            .get("reviewedNative")
            .cloned()
            .expect("the native record"),
        native_sha256: words("nativeSha256"),
        cargo_lock_sha256: words("cargoLockSha256"),
        advisory,
        advisory_sha256: current,
        cache_pin: "b".repeat(64),
        root,
    }
}

/// When the cases were recorded.
fn now(recording: &Document) -> Moment {
    assert_eq!(text(recording, "now"), "2026-09-30T00:00:00+00:00");
    Moment(1_790_726_400 * 1_000_000)
}

/// Make the file `path` with `bytes`, and every directory above it, as the
/// recorder's mask of 077 let it.
fn made(path: &Path, bytes: &[u8]) {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(path.parent().expect("a directory"))
        .expect("a directory");
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let mut file = options.open(path).expect("a new file");
    std::io::Write::write_all(&mut file, bytes).expect("its bytes");
}

/// How one run of a scanner is answered: the scanner, its first two
/// arguments or `canary` for the detector's self-test, and its status,
/// files and text.
#[derive(Clone, Debug)]
struct Answer {
    tool: String,
    key: String,
    status: i32,
    files: Vec<(String, String)>,
    text: String,
}

fn answered(items: &[Document]) -> Vec<Answer> {
    let answer = |item: &Document| Answer {
        tool: text(item, "tool").to_owned(),
        key: text(item, "key").to_owned(),
        status: i32::try_from(
            item.get("status")
                .and_then(Document::whole)
                .expect("a status"),
        )
        .expect("a status"),
        files: item
            .get("files")
            .and_then(Document::fields)
            .expect("files")
            .iter()
            .map(|(name, body)| (name.clone(), body.text().expect("a body").to_owned()))
            .collect(),
        text: text(item, "text").to_owned(),
    };
    items.iter().map(answer).collect()
}

/// What the engine was asked, by every branch together: how often, and the
/// containers it started with their owners and statuses.
#[derive(Debug, Default)]
struct Asked {
    calls: i128,
    containers: Vec<(String, String, i32)>,
    runs: Vec<(String, String, Vec<String>)>,
}

/// An engine that answers every command of the gate as the recorder's did:
/// a run from its answer, its files written into the scanner's output, a
/// container's record from the run that made it, the base and the selected
/// image as recorded.
#[derive(Clone)]
struct Gates {
    answers: Arc<Vec<Answer>>,
    selected: String,
    asked: Arc<Mutex<Asked>>,
    /// The run that a signal arrives in, and what it sets.
    cancel: Option<(String, Arc<AtomicBool>)>,
    /// Whether every container is another's once it has run.
    foreign: bool,
    /// The run whose scanner breaks down altogether.
    broken: Option<String>,
}

impl Gates {
    fn new(answers: Vec<Answer>, selected: String) -> Self {
        Self {
            answers: Arc::new(answers),
            selected,
            asked: Arc::new(Mutex::new(Asked::default())),
            cancel: None,
            foreign: false,
            broken: None,
        }
    }

    /// Answer the run `argv`.
    fn ran(&self, argv: &[&str], asked: &mut Asked) -> (i32, String) {
        let position = |flag: &str| argv.iter().position(|word| *word == flag).expect(flag);
        let name = argv[position("--name") + 1];
        let owner = argv[position("--label") + 1]
            .split_once('=')
            .expect("an owner")
            .1;
        let mounts: Vec<usize> = (0..argv.len())
            .filter(|at| argv[*at] == "--mount")
            .collect();
        let source = argv[mounts[1] + 1].split_once("src=").expect("a source").1;
        let output = Path::new(source.split_once(',').expect("a target").0);
        let entry = position("--entrypoint");
        let tool = argv[entry + 1].rsplit('/').next().expect("a scanner");
        let rest = &argv[entry + 3..];
        let canary =
            output.file_name().and_then(|name| name.to_str()) == Some("secret-canary-output");
        let key = if tool == "gitleaks" && canary {
            "canary".to_owned()
        } else {
            rest[..rest.len().min(2)].join(" ")
        };
        assert!(
            self.broken.as_deref() != Some(key.as_str()),
            "a scanner that broke down"
        );
        let answer = self
            .answers
            .iter()
            .find(|answer| answer.tool == tool && answer.key == key);
        let answer = answer.unwrap_or_else(|| panic!("no answer to {tool} {key}"));
        for (relative, body) in &answer.files {
            made(&output.join(relative), body.as_bytes());
        }
        if let Some((at, cancelled)) = &self.cancel
            && *at == key
        {
            cancelled.store(true, Ordering::SeqCst);
        }
        asked
            .containers
            .push((name.to_owned(), owner.to_owned(), answer.status));
        let argv = argv.iter().map(|word| (*word).to_owned()).collect();
        asked.runs.push((tool.to_owned(), key, argv));
        (answer.status, answer.text.clone())
    }
}

impl Engine for Gates {
    fn name(&self) -> &str {
        "docker"
    }

    fn log(&self) -> Option<PathBuf> {
        None
    }

    fn command(
        &mut self,
        arguments: &[OsString],
        _seconds: u64,
        _failing: bool,
        _healthy: &mut dyn FnMut() -> bool,
    ) -> Result<(i32, String)> {
        let argv: Vec<&str> = arguments
            .iter()
            .map(|item| item.to_str().expect("text"))
            .collect();
        let mut asked = self.asked.lock().unwrap_or_else(PoisonError::into_inner);
        asked.calls += 1;
        match argv[..] {
            ["pull", ..] => Ok((0, String::new())),
            ["image", "inspect", "--format", "{{.Id}}", ..] => Ok((0, BASE.to_owned())),
            ["image", "inspect", ..] => Ok((0, self.selected.clone())),
            ["run", ..] => Ok(self.ran(&argv, &mut asked)),
            ["container", "inspect", .., name] => {
                let found = asked.containers.iter().find(|(held, ..)| held == name);
                let (_, owner, status) = found.cloned().expect("a container");
                let owner = if self.foreign {
                    "foreign".to_owned()
                } else {
                    owner
                };
                let record = Document::record([
                    ("id", IDENTIFIER.into()),
                    ("image", BASE.into()),
                    ("owner", owner.as_str().into()),
                    ("state", "exited".into()),
                    ("exitStatus", Document::Number(i64::from(status).into())),
                    ("oomKilled", false.into()),
                ]);
                Ok((0, record.compact()))
            }
            ["container", "rm", "--force", ..] => Ok((0, IDENTIFIER.to_owned())),
            ["ps", "--all", ..] => Ok((0, String::new())),
            _ => panic!("no answer to {argv:?}"),
        }
    }

    fn branch(&self, _logs: &Path) -> Box<dyn Engine> {
        Box::new(self.clone())
    }
}

/// The machine of one case: the recorder's replacements for the two
/// commands, the engine, the scanners' installation, the proof and the
/// clock, with what each was asked.
struct Fixture<'a> {
    recording: &'a Document,
    case: &'a Document,
    base: PathBuf,
    output: PathBuf,
    engine: Gates,
    cancelled: Arc<AtomicBool>,
    installed: bool,
    /// Whether a signal arrives while the scanners are installed.
    cancelling: bool,
    proved: RefCell<Vec<Document>>,
    installs: RefCell<Vec<Document>>,
    /// The moment the clock says once it has been asked once, if it moves.
    later: Option<Moment>,
    /// The bytes of the application's file in the layer, if they are not
    /// the recorded ones.
    application: Option<String>,
    asked_now: std::cell::Cell<usize>,
}

impl<'a> Fixture<'a> {
    fn new(recording: &'a Document, case: &'a Document, base: &Path, output: &str) -> Self {
        let answers = answered(
            case.get("answers")
                .and_then(Document::items)
                .unwrap_or_default(),
        );
        let selected = case
            .get("selected")
            .map(Document::compact)
            .unwrap_or_default();
        Self {
            recording,
            case,
            base: base.to_path_buf(),
            output: base.join(output),
            engine: Gates::new(answers, selected),
            cancelled: Arc::new(AtomicBool::new(false)),
            installed: case.get("installed") != Some(&Document::Bool(false)),
            cancelling: false,
            proved: RefCell::new(Vec::new()),
            installs: RefCell::new(Vec::new()),
            later: None,
            application: None,
            asked_now: std::cell::Cell::new(0),
        }
    }

    /// `path` as the recorder named it.
    fn shown(&self, path: &Path) -> String {
        let text = path.to_str().expect("text");
        text.replace(self.base.to_str().expect("text"), "<base>")
    }
}

impl Host for Fixture<'_> {
    fn extract(&self, archive: &Path, manifest: &Path, tree: &Path) -> Result<()> {
        let artifact = self.base.join("artifact");
        assert_eq!(archive, artifact.join("image.tar"));
        assert_eq!(manifest, artifact.join("release.json"));
        assert_eq!(tree, self.output.join("archive"));
        if self.case.get("extraction") == Some(&Document::Bool(false)) {
            return Err(Fault(EXTRACTION));
        }
        let rootfs = tree.join("rootfs");
        let below = |name: &str| rootfs.join(text(self.recording, name).trim_start_matches('/'));
        let native = self.case.get("native").expect("native evidence").spaced();
        let report = self.case.get("report").expect("a report").spaced();
        for (path, body) in [
            (
                rootfs.join("usr/share/simplestchat/native-components.json"),
                native.as_str(),
            ),
            (below("noticePath"), text(self.recording, "notice")),
            (below("library"), text(self.recording, "runtime")),
            (
                tree.join("layers/000/app"),
                self.application
                    .as_deref()
                    .unwrap_or(text(self.recording, "application")),
            ),
            (tree.join("report.json"), report.as_str()),
        ] {
            made(&path, body.as_bytes());
        }
        Ok(())
    }

    fn audit(&self, rootfs: &Path, platform: Platform, report: &Path) -> Result<()> {
        assert_eq!(rootfs, self.output.join("archive/rootfs"));
        assert_eq!(platform, Platform::Amd64);
        made(
            report,
            self.case.get("elf").expect("an audit").spaced().as_bytes(),
        );
        let passed = self.case.get("audit") == Some(&Document::Bool(true));
        crate::require(passed, AUDIT)
    }

    fn prefix(&self) -> Result<Vec<OsString>> {
        Ok(texts(self.recording, "prefix")
            .into_iter()
            .map(OsString::from)
            .collect())
    }

    fn engine(
        &self,
        prefix: &[OsString],
        environment: Vec<(&'static str, OsString)>,
        logs: PathBuf,
    ) -> Box<dyn Engine> {
        assert_eq!(prefix, self.prefix().expect("the prefix"));
        let configuration = self.output.join("engine-config");
        let expected: Vec<(&str, OsString)> = vec![
            ("LC_ALL", "C".into()),
            ("LANG", "C".into()),
            ("DOCKER_CONFIG", configuration.clone().into()),
            (
                "REGISTRY_AUTH_FILE",
                configuration.join("config.json").into(),
            ),
        ];
        assert_eq!(environment, expected);
        assert_eq!(logs, self.output.join("scanner-commands"));
        Box::new(self.engine.clone())
    }

    fn install(&self, platform: &'static str, directory: &Path) -> Result<PathBuf> {
        // What is installed is the image tier's scanners, which the
        // reference named.
        let names = TOOLS.iter().map(|name| (*name).into()).collect();
        self.installs.borrow_mut().push(Document::record([
            ("names", Document::List(names)),
            ("platform", platform.into()),
            ("directory", self.shown(directory).into()),
        ]));
        if self.cancelling {
            self.cancelled.store(true, Ordering::SeqCst);
        }
        crate::require(self.installed, "tool_archive_digest")?;
        Ok(directory.to_path_buf())
    }

    fn locate(
        &self,
        name: &'static str,
        platform: &'static str,
        directory: &Path,
    ) -> Result<PathBuf> {
        assert_eq!(platform, "linux-x86_64");
        Ok(directory.join(name))
    }

    fn prove(&self, evidence: &runtime::Evidence<'_>) -> Result<Document> {
        let conversion = fs::read(self.output.join("spdx/sbom.spdx.json")).expect("a conversion");
        let equal = |held: &Document, name: &str| Some(held) == self.case.get(name);
        self.proved.borrow_mut().push(Document::record([
            ("tree", self.shown(evidence.tree).into()),
            (
                "conversion",
                (evidence.sbom_sha256 == crate::digest(&conversion)).into(),
            ),
            ("checkout", (evidence.root == repository()).into()),
            (
                "manifest",
                (Some(evidence.manifest) == self.recording.get("manifest")).into(),
            ),
            ("elf", equal(evidence.elf, "elf").into()),
            ("native", equal(evidence.native, "native").into()),
        ]));
        let checkout = self.recording.get("checkout").expect("the checkout");
        let advisory = text(checkout, "advisoryPolicySha256");
        assert_eq!(evidence.advisory_policy_sha256, advisory);
        assert_eq!(self.case.get("proof").and_then(Document::text), Some(PROOF));
        Err(Fault(PROOF))
    }

    fn ambient(&self, _name: &str) -> Option<OsString> {
        None
    }

    fn now(&self) -> Moment {
        let asked = self.asked_now.get();
        self.asked_now.set(asked + 1);
        match self.later {
            Some(later) if asked > 0 => later,
            _ => now(self.recording),
        }
    }

    fn cancelled(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }
}

/// `value` with the paths below `base` and the owners of the containers as
/// the recorder named them.
fn masked(value: &Document, base: &Path, asked: &Asked) -> Document {
    let mut shown = value
        .compact()
        .replace(base.to_str().expect("text"), "<base>");
    for (_, owner, _) in &asked.containers {
        shown = shown.replace(owner.as_str(), "<owner>");
    }
    Document::read(shown.as_bytes()).expect("a document")
}

/// Every run's command line as the recorder named it, the account the
/// scanners run as masked, in the order of the scanner and the key.
fn commands(asked: &Asked, base: &Path) -> Document {
    let user = format!(
        "{}:{}",
        rustix::process::getuid().as_raw(),
        rustix::process::getgid().as_raw()
    );
    let mut runs: Vec<&(String, String, Vec<String>)> = asked.runs.iter().collect();
    runs.sort_by(|left, right| (&left.0, &left.1).cmp(&(&right.0, &right.1)));
    let shown = runs.into_iter().map(|(scanner, key, argv)| {
        let argv = argv.iter().map(|word| {
            if *word == user {
                "<user>"
            } else {
                word.as_str()
            }
        });
        let record = Document::record([
            ("scanner", scanner.as_str().into()),
            ("key", key.as_str().into()),
            ("argv", Document::List(argv.map(Document::from).collect())),
        ]);
        masked(&record, base, asked)
    });
    Document::List(shown.collect())
}

/// The record at `path`, if there is one.
fn read(path: &Path) -> Option<Document> {
    path.exists()
        .then(|| Document::read(&fs::read(path).expect("a record")).expect("a record"))
}

/// The names below `root`, in their order.
fn names(root: &Path) -> Vec<String> {
    if !root.exists() {
        return Vec::new();
    }
    let walked = walked(root);
    let fields = walked.fields().expect("a tree");
    fields.iter().map(|(name, _)| name.clone()).collect()
}

/// `outcome` with the digest of each file the gate wrote named, as the
/// recorder named them, once it is that file's digest.
fn unwritten(mut outcome: Document, output: &Path) -> Document {
    for (name, file) in WRITTEN {
        if let Some(held) = outcome
            .get(name)
            .and_then(Document::text)
            .map(str::to_owned)
        {
            let bytes = fs::read(output.join(file)).expect("a written file");
            assert_eq!(held, crate::digest(&bytes), "{name}");
            outcome.put(name, "<written>".into());
        }
    }
    outcome
}

/// What one case came to: the answer, the outcome, the evidence kept, the
/// names of what was left, the engine's commands and what was proved.
#[derive(Debug, PartialEq)]
struct Came {
    result: String,
    outcome: Document,
    files: Document,
    names: Vec<String>,
    calls: i128,
    runs: Document,
    proved: Document,
}

/// Where `left` and `right` differ, each place named by its path below
/// `at`.
fn differences(left: &Document, right: &Document, at: &str) -> Vec<String> {
    match (left, right) {
        (Document::Record(held), Document::Record(other)) => {
            let mut names: Vec<&String> = held.iter().chain(other).map(|(name, _)| name).collect();
            names.sort();
            names.dedup();
            let field = |fields: &[(String, Document)], name: &str| {
                let found = fields.iter().find(|(held, _)| held == name);
                found.map_or(Document::Null, |(_, value)| value.clone())
            };
            let nested = names.into_iter().flat_map(|name| {
                differences(
                    &field(held, name),
                    &field(other, name),
                    &format!("{at}.{name}"),
                )
            });
            nested.collect()
        }
        (Document::List(held), Document::List(other)) if held.len() == other.len() => {
            let pairs = held.iter().zip(other).enumerate();
            let nested = pairs.flat_map(|(index, (held, other))| {
                differences(held, other, &format!("{at}[{index}]"))
            });
            nested.collect()
        }
        _ if left == right => Vec::new(),
        _ => vec![format!(
            "{at}: {} against {}",
            left.compact(),
            right.compact()
        )],
    }
}

/// Hold `came` to `expected` field by field, naming every difference.
fn alike(came: &Came, expected: &Came, name: &str) {
    assert_eq!(came.result, expected.result, "{name}: the answer");
    for (part, held, other) in [
        ("outcome", &came.outcome, &expected.outcome),
        ("files", &came.files, &expected.files),
        ("runs", &came.runs, &expected.runs),
        ("proved", &came.proved, &expected.proved),
    ] {
        let found = differences(held, other, part);
        assert!(found.is_empty(), "{name}: {}", found.join("\n"));
    }
    assert_eq!(came.names, expected.names, "{name}: the names");
    assert_eq!(came.calls, expected.calls, "{name}: the commands");
}

/// The result of an answer as the recording words it.
fn worded(came: &Result<bool>) -> String {
    match came {
        Ok(passed) => format!("held {}", Document::Bool(*passed).pretty()),
        Err(fault) => format!("refused {fault}"),
    }
}

/// The saved image of a case below `base`, and the scanners' bytes.
fn artifact(recording: &Document, case: &Document, base: &Path) {
    let artifact = base.join("artifact");
    made(
        &artifact.join("image.tar"),
        text(recording, "archive").as_bytes(),
    );
    let manifest = recording.get("manifest").expect("a manifest").spaced();
    made(&artifact.join("release.json"), manifest.as_bytes());
    made(
        &artifact.join("outcome.json"),
        case.get("export").expect("an export").spaced().as_bytes(),
    );
    for tool in TOOLS {
        made(
            &base.join("tools").join(tool),
            format!("#!/bin/sh\n# {tool}\n").as_bytes(),
        );
    }
}

/// Judge `case` against `checkout` as the recorder judged it, with
/// `changed` changing its machine first.
fn judged(
    recording: &Document,
    checkout: &Checkout,
    case: &Document,
    changed: impl FnOnce(&mut Fixture<'_>, &mut Options),
) -> Came {
    let scratch = Scratch::new("gate");
    let base = scratch.0.clone();
    artifact(recording, case, &base);
    let mut options = Options {
        image_id: IMAGE.to_owned(),
        artifact: base.join("artifact"),
        output: base.join("output"),
        tools: base.join("tools"),
        platform: Platform::Amd64,
        database_cache: None,
    };
    let mut fixture = Fixture::new(recording, case, &base, "output");
    changed(&mut fixture, &mut options);
    let came = execute(&options, checkout, &fixture);
    let asked = fixture
        .engine
        .asked
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let outcome = read(&options.output.join("outcome.json")).map_or(Document::Null, |outcome| {
        masked(&unwritten(outcome, &options.output), &base, &asked)
    });
    let mut files = Vec::new();
    for name in FILES {
        if let Some(record) = read(&options.output.join(name)) {
            files.push((name.to_owned(), masked(&record, &base, &asked)));
        }
    }
    assert!(
        fixture.installs.borrow().is_empty(),
        "the gate installs nothing"
    );
    let result = Came {
        result: worded(&came),
        outcome,
        files: Document::Record(files),
        names: names(&options.output),
        calls: asked.calls,
        runs: commands(&asked, &base),
        proved: Document::List(fixture.proved.borrow().clone()),
    };
    drop(asked);
    reopened(&base);
    result
}

/// What `case` recorded it came to, and whether a statement it published
/// names this gate as its maker.
fn recorded(case: &Document) -> (Came, bool) {
    let files = case.get("files").and_then(Document::fields).expect("files");
    let mut kept: Vec<(String, Document)> = files.to_vec();
    kept.sort_by_key(|(name, _)| FILES.iter().position(|file| file == name));
    let files = Document::Record(kept);
    let outcome = case.get("outcome").cloned().unwrap_or(Document::Null);
    let maker = "simplestchat-security image;";
    let named = [&files, &outcome]
        .iter()
        .any(|value| value.compact().contains(maker));
    let came = Came {
        result: there(case),
        outcome,
        files,
        names: texts(case, "names"),
        calls: i128::from(whole(case, "calls")),
        runs: case.get("runs").cloned().expect("the runs"),
        proved: case.get("proved").cloned().expect("what was proved"),
    };
    (came, named)
}

/// The cases whose outcome names the code of the step that runs in process
/// here, where the reference recorded only that its command failed.
const REWORDED: [(&str, &str); 2] = [
    ("an archive that cannot be extracted", EXTRACTION),
    ("an audit that fails", AUDIT),
];

#[test]
fn every_recorded_gate_is_judged_alike() {
    let recording = recording();
    let checkout = checkout(&recording);
    let (mut tally, mut statements) = ([0; 3], 0);
    for case in cases(&recording, "gates") {
        let name = text(&case, "name");
        let came = judged(&recording, &checkout, &case, |_, _| {});
        let (mut expected, named) = recorded(&case);
        statements += usize::from(named);
        if let Some((_, code)) = REWORDED.iter().find(|(other, _)| *other == name) {
            assert_eq!(
                expected.outcome.get("error"),
                Some(&"BuildError".into()),
                "{name}"
            );
            expected.outcome.put("error", (*code).into());
            tally[1] += 1;
        } else if expected.outcome == Document::Null {
            tally[2] += 1;
        } else {
            tally[0] += 1;
        }
        alike(&came, &expected, name);
    }
    // Ten outcomes alike, two that name the step's own code, and the one
    // export that did not pass, refused before any evidence; the two whose
    // disposition was published name this gate as its maker.
    assert_eq!(tally, [10, 2, 1]);
    assert_eq!(statements, 2);
}

/// The answers the recorder's engine gave a preparation.
fn preparing(case: &Document) -> Vec<Answer> {
    let failure = text(case, "failure");
    let status = case.get("status").expect("a status").spaced();
    let database = vec![
        (
            "cache/6/vulnerability.db".to_owned(),
            "inert checked database".to_owned(),
        ),
        (
            "cache/6/import.json".to_owned(),
            r#"{"checksum":"fixture"}"#.to_owned(),
        ),
    ];
    let answer = |key: &str, status: i32, files, text: String| Answer {
        tool: "grype".to_owned(),
        key: key.to_owned(),
        status,
        files,
        text,
    };
    vec![
        answer(
            "db update",
            i32::from(failure == "update"),
            database,
            String::new(),
        ),
        match failure {
            "status" => answer("db status", 1, Vec::new(), String::new()),
            _ => answer("db status", 0, Vec::new(), status),
        },
    ]
}

/// What a preparation came to: its answer, its evidence, the cache it
/// published and that cache's receipt, the installations it asked for,
/// the engine's commands and the names of what was left.
#[derive(Debug, PartialEq)]
struct Prepared {
    result: String,
    evidence: Document,
    saved: Document,
    receipt: Document,
    asked: Document,
    calls: i128,
    names: Vec<String>,
}

/// Prepare the database as `case` recorded, with `changed` changing the
/// machine first; the receipt's pin is named as the recorder pinned it,
/// once it is the pin of this checkout's lock.
fn prepared(
    recording: &Document,
    case: &Document,
    changed: impl FnOnce(&mut Fixture<'_>),
) -> Prepared {
    let scratch = Scratch::new("preparation");
    let base = scratch.0.clone();
    for tool in TOOLS {
        made(
            &base.join("tools").join(tool),
            format!("#!/bin/sh\n# {tool}\n").as_bytes(),
        );
    }
    let (output, saved) = (base.join("preparation"), base.join("saved"));
    let relative = case.get("relative").and_then(Document::text);
    let given = |name: &str, path: &Path| match relative {
        Some(asked) if asked == name => PathBuf::from("relative"),
        _ => path.to_path_buf(),
    };
    let preparation = Preparation {
        output: given("output", &output),
        tools: given("tools_directory", &base.join("tools")),
        platform: Platform::Amd64,
        database_cache: (relative != Some("no cache")).then(|| given("database_cache", &saved)),
    };
    let mut fixture = Fixture::new(recording, case, &base, "preparation");
    fixture.engine = Gates::new(preparing(case), String::new());
    let failure = text(case, "failure");
    if failure == "cancel" {
        fixture.engine.cancel = Some(("db update".to_owned(), Arc::clone(&fixture.cancelled)));
    }
    fixture.engine.foreign = failure == "cleanup";
    changed(&mut fixture);
    let root = repository();
    let came = execute_database(&preparation, &root, &fixture);
    let asked = fixture
        .engine
        .asked
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let mut evidence = read(&output.join("database-preparation.json")).unwrap_or(Document::Null);
    if let Some(digest) = evidence
        .get("databaseEvidenceSha256")
        .and_then(Document::text)
    {
        let status = fs::read(output.join("database-status.json")).expect("the status");
        assert_eq!(digest, crate::digest(&status));
        evidence.put("databaseEvidenceSha256", "<written>".into());
    }
    let mut receipt = read(&saved.join("receipt.json")).unwrap_or(Document::Null);
    if receipt != Document::Null {
        let pin = cache_pin(&root).expect("the pin");
        assert_eq!(receipt.get("scannerPin"), Some(&pin.as_str().into()));
        receipt.put("scannerPin", "b".repeat(64).into());
    }
    let saved = saved.exists().then(|| {
        let listing = fs::read_dir(&saved).expect("the cache");
        let mut held: Vec<String> = listing
            .map(|entry| {
                entry
                    .expect("an entry")
                    .file_name()
                    .to_str()
                    .expect("text")
                    .to_owned()
            })
            .collect();
        held.sort();
        Document::List(held.into_iter().map(Document::from).collect())
    });
    let result = Prepared {
        result: worded(&came),
        evidence,
        saved: saved.unwrap_or(Document::Null),
        receipt,
        asked: Document::List(fixture.installs.borrow().clone()),
        calls: asked.calls,
        names: names(&output),
    };
    drop(asked);
    reopened(&base);
    result
}

#[test]
fn every_recorded_preparation_is_judged_alike() {
    let recording = recording();
    let mut tally = [0; 3];
    for case in cases(&recording, "preparations") {
        let name = text(&case, "name");
        let came = prepared(&recording, &case, |_| {});
        let expected = Prepared {
            result: there(&case),
            evidence: case.get("evidence").cloned().unwrap_or(Document::Null),
            saved: case.get("saved").cloned().unwrap_or(Document::Null),
            receipt: case.get("receipt").cloned().unwrap_or(Document::Null),
            asked: case.get("asked").cloned().expect("what was installed"),
            calls: i128::from(whole(&case, "calls")),
            names: texts(&case, "names"),
        };
        for (part, held, other) in [
            ("evidence", &came.evidence, &expected.evidence),
            ("saved", &came.saved, &expected.saved),
            ("receipt", &came.receipt, &expected.receipt),
            ("asked", &came.asked, &expected.asked),
        ] {
            let found = differences(held, other, part);
            assert!(found.is_empty(), "{name}: {}", found.join("\n"));
        }
        assert_eq!(came, expected, "{name}");
        let slot = match came.result.as_str() {
            "held true\n" => 0,
            "held false\n" => 1,
            _ => 2,
        };
        tally[slot] += 1;
    }
    // One preparation that passed, six that failed with their evidence, and
    // four whose paths were refused before any.
    assert_eq!(tally, [1, 6, 4]);
}

/// The first recorded gate, the whole image that passes.
fn whole_image(recording: &Document) -> Document {
    let found = cases(recording, "gates")
        .into_iter()
        .find(|case| text(case, "name") == "a whole image");
    found.expect("the whole image")
}

#[test]
fn a_gate_with_a_database_cache_publishes_the_database_under_the_checkouts_pin() {
    let recording = recording();
    let mut checkout = checkout(&recording);
    checkout.cache_pin = "e".repeat(64);
    let case = whole_image(&recording);
    let scratch = Scratch::new("cached");
    let saved = scratch.0.join("saved");
    let came = judged(&recording, &checkout, &case, |_, options| {
        options.database_cache = Some(saved.clone());
    });
    assert_eq!(came.result, worded(&Ok(true)));
    let receipt = read(&saved.join("receipt.json")).expect("a published cache");
    assert_eq!(receipt.get("scannerPin"), Some(&"e".repeat(64).into()));
}

#[test]
fn a_signal_while_the_branches_scan_is_named_before_their_failures() {
    let recording = recording();
    let checkout = checkout(&recording);
    let case = whole_image(&recording);
    let came = judged(&recording, &checkout, &case, |fixture, _| {
        let cancelled = Arc::clone(&fixture.cancelled);
        fixture.engine.cancel = Some(("db update".to_owned(), cancelled));
    });
    assert_eq!(came.result, worded(&Ok(false)));
    assert_eq!(
        came.outcome.get("error"),
        Some(&"image_scan_cancelled".into())
    );
    let receipt = |name: &str| {
        let found = came.names.iter().any(|held| held == name);
        assert!(found, "{name}");
    };
    receipt("scanner-result-phase-database.json");
    receipt("scanner-result-phase-sbom.json");
    receipt("scanner-result-phase-secrets.json");
    assert!(
        !came
            .names
            .iter()
            .any(|name| name.starts_with("vulnerabilities"))
    );
}

#[test]
fn a_branch_that_breaks_down_cancels_the_others() {
    let recording = recording();
    let checkout = checkout(&recording);
    let case = whole_image(&recording);
    let came = judged(&recording, &checkout, &case, |fixture, _| {
        fixture.engine.broken = Some("db update".to_owned());
    });
    assert_eq!(came.result, worded(&Ok(false)));
    assert_eq!(
        came.outcome.get("error"),
        Some(&"image_scan_cancelled".into())
    );
}

#[test]
fn a_signal_while_the_scanners_are_installed_stops_the_preparation() {
    let recording = recording();
    let found = cases(&recording, "preparations")
        .into_iter()
        .find(|case| text(case, "name") == "a preparation of the database");
    let case = found.expect("the preparation that passed");
    let came = prepared(&recording, &case, |fixture| fixture.cancelling = true);
    assert_eq!(came.result, worded(&Ok(false)));
    assert_eq!(
        came.evidence.get("error"),
        Some(&"image_scan_cancelled".into())
    );
    assert_eq!(came.evidence.get("cancelled"), Some(&Document::Bool(true)));
    assert_eq!(came.names, ["database-preparation.json"]);
    assert_eq!(came.calls, 0);
}

#[test]
fn a_sandbox_past_its_share_of_the_scanners_scans_no_branch() {
    let scratch = Scratch::new("share");
    let tools = scratch.0.join("tools");
    fs::create_dir(&tools).expect("tools");
    let engine = Gates::new(Vec::new(), String::new());
    let asked = Arc::clone(&engine.asked);
    let limits = crate::image::sandbox::Limits::REVIEWED;
    let mut sandbox = Sandbox::new(
        Box::new(engine),
        &scratch.0,
        &tools,
        BASE,
        Platform::Amd64,
        limits,
    );
    sandbox.sequence = 3;
    let document = Document::Null;
    let pins = Pins {
        lock: String::new(),
        syft: String::new(),
        grype: String::new(),
        gitleaks: String::new(),
    };
    let branches = Branches {
        artifact: &scratch.0,
        tree: &scratch.0,
        native: &document,
        elf: &document,
        image_policy: &document,
        pins: &pins,
        limits: &Limits::REVIEWED,
        now: Moment(0),
        free: &|_| Ok(u64::MAX),
    };
    let refused = scan_reports(&mut sandbox, &branches).expect_err("past its share");
    assert_eq!(refused, Fault("image_scanner_count"));
    assert_eq!(sandbox.allowance, 3);
    assert_eq!(
        asked.lock().unwrap_or_else(PoisonError::into_inner).calls,
        0
    );
    assert!(!scratch.0.join("database-commands").exists());
}

#[test]
fn the_checkout_is_read_as_it_is() {
    // Every file is named here as it is reviewed, not as the gate names it.
    let root = repository();
    let read = Checkout::read(&root, crate::policy::Date::MIN).expect("the checkout");
    let digest = |name: &str| crate::digest(&fs::read(root.join(name)).expect(name));
    let lock =
        policy::report(&root.join("build/security-tools.lock.json"), u64::MAX).expect("the lock");
    let version = |name: &str| {
        let tool = lock.get("tools").and_then(|tools| tools.get(name));
        text(tool.expect(name), "version").to_owned()
    };
    assert_eq!(read.root, root);
    assert_eq!(
        read.pins,
        Pins {
            lock: digest("build/security-tools.lock.json"),
            syft: version("syft"),
            grype: version("grype"),
            gitleaks: version("gitleaks"),
        }
    );
    let tools = lock.get("tools").expect("the tools");
    assert_eq!(
        read.cache_pin,
        cache::pin(tools.get("grype").expect("grype"))
    );
    assert_eq!(read.policy_sha256, digest("security/image-policy.json"));
    assert_eq!(read.exceptions_sha256, digest("security/exceptions.json"));
    assert_eq!(read.native_sha256, digest("vendor/native-components.json"));
    assert_eq!(read.cargo_lock_sha256, digest("Cargo.lock"));
    assert_eq!(
        read.advisory,
        root.join("security/advisories/openssl-cve-2026-84782.json")
    );
    assert_eq!(
        read.advisory_sha256,
        digest("security/advisories/openssl-cve-2026-84782.json")
    );
    let policy = policy::load_policy(&root.join("security/image-policy.json")).expect("the policy");
    assert_eq!(read.image_policy, policy);
    let native = fs::read(root.join("vendor/native-components.json")).expect("the native record");
    assert_eq!(read.native, Document::read(&native).expect("a record"));
    let reviews = crate::policy::read(
        &root.join("security/exceptions.json"),
        crate::policy::Date::MIN,
        false,
    );
    assert_eq!(read.exceptions, reviews.expect("the reviews"));
}

/// `case` with the answer of the run `key` changed by `change`.
fn answering(case: &Document, key: &str, change: impl FnOnce(&mut Document)) -> Document {
    let mut case = case.clone();
    let mut answers = case
        .get("answers")
        .and_then(Document::items)
        .expect("answers")
        .to_vec();
    let found = answers
        .iter_mut()
        .find(|answer| answer.get("key").and_then(Document::text) == Some(key));
    change(found.expect(key));
    case.put("answers", Document::List(answers));
    case
}

/// `case` with the record `file` the run `key` writes changed by `change`.
fn writing(case: &Document, key: &str, file: &str, change: impl FnOnce(&mut Document)) -> Document {
    answering(case, key, |answer| {
        let mut files = answer.get("files").cloned().expect("files");
        let body = files.get(file).and_then(Document::text).expect(file);
        let mut value = Document::read(body.as_bytes()).expect("a record");
        change(&mut value);
        files.put(file, value.compact().into());
        answer.put("files", files);
    })
}

/// A review of the finding `fingerprint` of `scanner` in `scope`.
fn review(scanner: &str, fingerprint: &str, scope: &str) -> crate::policy::Exception {
    crate::policy::Exception {
        scanner: scanner.to_owned(),
        fingerprint: fingerprint.to_owned(),
        scope: scope.to_owned(),
        owner: "security".to_owned(),
        rationale: "an inert fixture".to_owned(),
        reachability: "never reached".to_owned(),
        expires: crate::policy::Date::MIN,
        review: "https://example.invalid/review".to_owned(),
    }
}

/// The finding `index` blocked by the check `check` of `came`.
fn blocked<'a>(came: &'a Came, check: &str, index: usize) -> &'a Document {
    let checks = came
        .outcome
        .get("checks")
        .and_then(|checks| checks.get(check));
    let found = checks
        .and_then(|check| check.get("blocked"))
        .and_then(Document::items);
    &found.expect("blocked findings")[index]
}

/// Whether the check `check` of `came` passed, and how many findings it
/// waived.
fn judgement(came: &Came, check: &str) -> (bool, usize) {
    let checks = came
        .outcome
        .get("checks")
        .and_then(|checks| checks.get(check));
    let check = checks.expect("a check");
    let waived = check
        .get("waived")
        .and_then(Document::items)
        .map_or(0, <[Document]>::len);
    (check.get("passed") == Some(&Document::Bool(true)), waived)
}

#[test]
fn a_secret_scan_that_fails_alone_fails_the_branches() {
    let recording = recording();
    let checkout = checkout(&recording);
    let case = answering(&whole_image(&recording), "dir /layers", |answer| {
        answer.put("status", 1_u64.into());
    });
    let came = judged(&recording, &checkout, &case, |_, _| {});
    assert_eq!(came.result, worded(&Ok(false)));
    let error = came.outcome.get("error");
    assert_eq!(error, Some(&"image_parallel_scan_failed".into()));
}

#[test]
fn a_database_that_goes_stale_while_the_branches_scan_is_refused() {
    let recording = recording();
    let checkout = checkout(&recording);
    let case = whole_image(&recording);
    let came = judged(&recording, &checkout, &case, |fixture, _| {
        fixture.later = Some(Moment(now(&recording).0 + 121 * 3600 * 1_000_000));
    });
    assert_eq!(came.result, worded(&Ok(false)));
    assert_eq!(
        came.outcome.get("error"),
        Some(&"image_database_stale".into())
    );
    assert!(
        came.names
            .iter()
            .any(|name| name == "vulnerabilities/grype.json")
    );
}

#[test]
fn a_match_made_with_another_database_is_refused() {
    let recording = recording();
    let checkout = checkout(&recording);
    let key = "sbom:/input/sbom.syft.json -o";
    let case = writing(&whole_image(&recording), key, "grype.json", |report| {
        let mut descriptor = report.get("descriptor").cloned().expect("a descriptor");
        let mut db = descriptor.get("db").cloned().expect("a database");
        let mut status = db.get("status").cloned().expect("a status");
        status.put("built", "2026-09-29T00:00:00+00:00".into());
        db.put("status", status);
        descriptor.put("db", db);
        report.put("descriptor", descriptor);
    });
    let came = judged(&recording, &checkout, &case, |_, _| {});
    assert_eq!(came.result, worded(&Ok(false)));
    let error = came.outcome.get("error");
    assert_eq!(error, Some(&"image_vulnerability_database_changed".into()));
}

#[test]
fn a_review_of_a_vulnerability_waives_only_it() {
    let recording = recording();
    let mut checkout = checkout(&recording);
    let key = "sbom:/input/sbom.syft.json -o";
    let case = writing(&whole_image(&recording), key, "grype.json", |report| {
        let found = Document::record([
            (
                "vulnerability",
                Document::record([
                    ("id", "CVE-fixture".into()),
                    ("severity", "High".into()),
                    ("fix", Document::record([("state", "not-fixed".into())])),
                ]),
            ),
            (
                "artifact",
                Document::record([("purl", "pkg:rpm/fedora/fixture@1".into())]),
            ),
        ]);
        report.put("matches", Document::List(vec![found]));
    });
    let came = judged(&recording, &checkout, &case, |_, _| {});
    assert_eq!(judgement(&came, "vulnerabilities"), (false, 0));
    assert_eq!(came.result, worded(&Ok(false)));
    let finding = blocked(&came, "vulnerabilities", 0);
    let (id, scope) = (text(finding, "id"), text(finding, "scope"));
    checkout.exceptions = vec![review("grype", id, scope)];
    let came = judged(&recording, &checkout, &case, |_, _| {});
    assert_eq!(judgement(&came, "vulnerabilities"), (true, 1));
    assert_eq!(came.result, worded(&Ok(true)));
}

#[test]
fn a_review_of_a_secret_waives_only_it() {
    let recording = recording();
    let mut checkout = checkout(&recording);
    let found = cases(&recording, "gates")
        .into_iter()
        .find(|case| text(case, "name") == "a secret found in the layers");
    let case = found.expect("the secret");
    // The layer holds the canary as the detector's self-test projects its
    // text, so the recorded finding is at its very place.
    let canary = format!("TOKEN={} # gitleaks:allow\n", crate::image::scan::canary());
    let placed = |fixture: &mut Fixture<'_>, _: &mut Options| {
        fixture.application = Some(canary.clone());
    };
    let came = judged(&recording, &checkout, &case, placed);
    assert_eq!(judgement(&came, "secrets"), (false, 0));
    let finding = blocked(&came, "secrets", 0);
    let (fingerprint, path) = (text(finding, "fingerprint"), text(finding, "path"));
    checkout.exceptions = vec![review("gitleaks", fingerprint, path)];
    let came = judged(&recording, &checkout, &case, placed);
    assert_eq!(judgement(&came, "secrets"), (true, 1));
    assert_eq!(came.result, worded(&Ok(true)));
}

#[test]
fn a_review_of_a_licence_waives_only_it() {
    let recording = recording();
    let mut checkout = checkout(&recording);
    let key = "scan docker-archive:/input/image.tar";
    let case = writing(&whole_image(&recording), key, "sbom.syft.json", |catalog| {
        let mut artifacts = catalog
            .get("artifacts")
            .and_then(Document::items)
            .expect("artifacts")
            .to_vec();
        let named = |artifact: &&mut Document| {
            artifact.get("name").and_then(Document::text) == Some("openssl-libs")
        };
        let rpm = artifacts.iter_mut().find(named).expect("the rpm");
        let licence = Document::record([("spdxExpression", "SSPL-1.0".into())]);
        rpm.put("licenses", Document::List(vec![licence]));
        catalog.put("artifacts", Document::List(artifacts));
    });
    let came = judged(&recording, &checkout, &case, |_, _| {});
    assert_eq!(
        judgement(&came, "licenses"),
        (false, 0),
        "{:?}",
        came.outcome.get("error")
    );
    let finding = blocked(&came, "licenses", 0);
    let (fingerprint, scope) = (text(finding, "fingerprint"), text(finding, "scope"));
    checkout.exceptions = vec![review("image-license", fingerprint, scope)];
    let came = judged(&recording, &checkout, &case, |_, _| {});
    assert_eq!(judgement(&came, "licenses"), (true, 1));
    assert_eq!(came.result, worded(&Ok(true)));
}

#[test]
fn reviews_that_have_expired_are_no_checkout_to_judge_by() {
    let far = crate::policy::Date::parse("9999-12-31").expect("a day");
    let refused = Checkout::read(&repository(), far).expect_err("expired reviews");
    assert_eq!(refused, Fault("expired_security_exception"));
}
