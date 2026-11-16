//! Replays of `recorded_scan.json`: what the reference in Python made of the
//! projections of the layers for the secret scanner, the detector's
//! self-test, the database's preparation, the archive's catalog, the secret
//! scan and its checks, the receipts of the phases, the runtime
//! disposition, the binding of the archive, the selected image and the
//! native evidence, each scan against its tests' engine.

use super::{
    Disposition, Limits, Pins, Reviewed, SECRET_ARGUMENTS, bind_archive, canary, native_binding,
    prepare_database, runtime_disposition, scan_phase, scan_sbom, scan_secrets, secret_bundle,
    secret_checks, secret_selftest, selected_image,
};
use crate::{
    Fault, Result,
    document::Document,
    image::{
        cache,
        notices::tests::built,
        policy::{
            Moment,
            tests::{There, tallied},
        },
        sandbox::{
            Cache, Engine, Limits as Bounds, Platform, Sandbox,
            tests::{reopened, texts, timeless, walked, whole},
        },
        secrets::tests::{here, text, there},
    },
    scratch::Scratch,
};
use std::{
    ffi::OsString,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
};

const RECORDING: &[u8] = include_bytes!("recorded_scan.json");

fn recording() -> Document {
    Document::read(RECORDING).expect("a recording")
}

fn cases(name: &str) -> Vec<Document> {
    let held = recording()
        .get(name)
        .and_then(Document::items)
        .map(<[Document]>::to_vec);
    held.unwrap_or_else(|| panic!("no cases {name}"))
}

/// The scanner base and container identity the engine answers with.
const BASE: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const IDENTIFIER: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// The pins of the lock the cases were recorded under.
fn pins() -> Pins {
    let recording = recording();
    let pinned = recording.get("pins").expect("the pins");
    Pins {
        lock: text(&recording, "lockSha256").to_owned(),
        syft: text(pinned, "syft").to_owned(),
        grype: text(pinned, "grype").to_owned(),
        gitleaks: text(pinned, "gitleaks").to_owned(),
    }
}

/// The time the database cases were recorded at.
fn now() -> Moment {
    assert_eq!(text(&recording(), "now"), "2026-09-30T00:00:00+00:00");
    Moment(1_790_726_400 * 1_000_000)
}

/// The time the dispositions were recorded at.
fn moment() -> Moment {
    assert_eq!(text(&recording(), "moment"), "2026-10-18T12:00:00+00:00");
    Moment(1_792_324_800 * 1_000_000)
}

/// One run of a scanner as the engine is told to answer it.
#[derive(Clone, Debug)]
struct Told {
    status: i32,
    text: String,
    files: Vec<(String, Document)>,
    cancel: bool,
}

/// The runs of `case`.
fn runs(case: &Document) -> Vec<Told> {
    let runs = case.get("runs").and_then(Document::items).expect("runs");
    let told = |run: &Document| Told {
        status: run
            .get("status")
            .and_then(Document::whole)
            .map_or(0, |status| i32::try_from(status).expect("a status")),
        text: run
            .get("text")
            .and_then(Document::text)
            .unwrap_or_default()
            .to_owned(),
        files: run
            .get("files")
            .and_then(Document::fields)
            .unwrap_or_default()
            .to_vec(),
        cancel: run.get("cancel") == Some(&Document::Bool(true)),
    };
    runs.iter().map(told).collect()
}

/// What the engine was asked and answered.
#[derive(Debug, Default)]
struct Asked {
    calls: Vec<Document>,
    owners: Vec<String>,
    status: i32,
}

/// An engine that answers each run as it is told: it writes the files of
/// the run into the scanner's output and ends with its status.
struct Engines {
    runs: Arc<Mutex<Vec<Told>>>,
    asked: Arc<Mutex<Asked>>,
    cancelled: Option<Arc<AtomicBool>>,
    failing: Option<&'static str>,
    inspected: Option<String>,
}

impl Engine for Engines {
    fn name(&self) -> &str {
        "docker"
    }

    fn log(&self) -> Option<PathBuf> {
        None
    }

    fn command(
        &mut self,
        arguments: &[OsString],
        seconds: u64,
        failing: bool,
        healthy: &mut dyn FnMut() -> bool,
    ) -> Result<(i32, String)> {
        let argv: Vec<String> = arguments
            .iter()
            .map(|item| item.to_str().expect("text").to_owned())
            .collect();
        let mut asked = self.asked.lock().unwrap_or_else(PoisonError::into_inner);
        asked.calls.push(Document::record([
            (
                "argv",
                Document::List(argv.iter().map(|item| item.as_str().into()).collect()),
            ),
            ("timeout", seconds.into()),
            ("allowFailure", failing.into()),
        ]));
        let words: Vec<&str> = argv.iter().map(String::as_str).collect();
        match words[..] {
            ["run", ..] => {
                let label = words
                    .iter()
                    .position(|word| *word == "--label")
                    .expect("a label");
                let owner = words[label + 1]
                    .split_once('=')
                    .expect("an owner")
                    .1
                    .to_owned();
                asked.owners.push(owner);
                let mounts: Vec<usize> = (0..words.len())
                    .filter(|index| words[*index] == "--mount")
                    .collect();
                let output = words[mounts[1] + 1].split_once("src=").expect("a source").1;
                let output = PathBuf::from(output.split_once(',').expect("a target").0);
                let told = self
                    .runs
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(0);
                for (name, body) in &told.files {
                    let path = output.join(name);
                    // The scanner writes as the recorder's mask of 077 let it.
                    let mut builder = fs::DirBuilder::new();
                    builder.recursive(true);
                    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
                    builder
                        .create(path.parent().expect("a directory"))
                        .expect("a directory");
                    match body {
                        Document::Number(_) => {
                            let file = fs::File::create(&path).expect("a file");
                            file.set_len(
                                u64::try_from(body.whole().expect("a size")).expect("a size"),
                            )
                            .expect("a size");
                        }
                        Document::Text(text) => {
                            let bytes: Vec<u8> = text
                                .chars()
                                .map(|character| {
                                    u8::try_from(u32::from(character)).expect("a byte")
                                })
                                .collect();
                            fs::write(&path, bytes).expect("a file");
                        }
                        other => panic!("no body {other:?}"),
                    }
                    let private = fs::Permissions::from_mode(0o600);
                    fs::set_permissions(&path, private).expect("a mode");
                }
                if told.cancel
                    && let Some(cancelled) = &self.cancelled
                {
                    cancelled.store(true, Ordering::SeqCst);
                }
                asked.status = told.status;
                Ok((told.status, told.text))
            }
            ["container", "inspect", ..] => {
                let owner = asked.owners.last().cloned().unwrap_or_default();
                let record = Document::record([
                    ("id", IDENTIFIER.into()),
                    ("image", BASE.into()),
                    ("owner", owner.as_str().into()),
                    ("state", "exited".into()),
                    (
                        "exitStatus",
                        Document::Number(i64::from(asked.status).into()),
                    ),
                    ("oomKilled", false.into()),
                ]);
                Ok((0, record.compact()))
            }
            ["container", "rm", "--force", ..] => Ok((0, IDENTIFIER.to_owned())),
            ["ps", "--all", ..] => Ok((0, String::new())),
            ["image", "inspect", ..] if !healthy() => Err(Fault("command_lease_lost")),
            ["image", "inspect", ..] => match self.failing {
                Some(code) => Err(Fault(code)),
                None => Ok((0, self.inspected.clone().unwrap_or_default())),
            },
            _ => panic!("no answer to {argv:?}"),
        }
    }

    fn branch(&self, _logs: &Path) -> Box<dyn Engine> {
        panic!("no branch")
    }
}

/// A sandbox in `root` whose engine answers `case` as recorded, and what
/// that engine was asked.
fn sandboxed(root: &Path, case: &Document) -> (Sandbox, Arc<Mutex<Asked>>) {
    let asked = Arc::new(Mutex::new(Asked::default()));
    let cancelled = Arc::new(AtomicBool::new(false));
    let engine = Engines {
        runs: Arc::new(Mutex::new(runs(case))),
        asked: Arc::clone(&asked),
        cancelled: Some(Arc::clone(&cancelled)),
        failing: None,
        inspected: None,
    };
    let tools = root.join("tools");
    fs::create_dir_all(&tools).expect("tools");
    let mut sandbox = Sandbox::new(
        Box::new(engine),
        root,
        &tools,
        BASE,
        Platform::Amd64,
        Bounds::REVIEWED,
    );
    sandbox.cancelled = cancelled;
    (sandbox, asked)
}

/// `value` with the paths below `base`, the owners and the account named as
/// the recorder named them.
fn masked(value: &Document, base: &Path, asked: &Asked, user: &str) -> Document {
    let mut shown = value
        .compact()
        .replace(base.to_str().expect("text"), "<base>");
    for owner in &asked.owners {
        shown = shown.replace(owner.as_str(), "<owner>");
    }
    Document::read(shown.replace(user, "<user>").as_bytes()).expect("a document")
}

/// The evidence records a scan left in `root`, as the recorder showed them.
fn evidence(root: &Path, base: &Path, asked: &Asked, user: &str) -> Document {
    let mut found = Vec::new();
    for entry in fs::read_dir(root).expect("evidence") {
        let path = entry.expect("an entry").path();
        let name = path
            .file_name()
            .expect("a name")
            .to_str()
            .expect("text")
            .to_owned();
        if !name.ends_with(".json") || !path.is_file() {
            continue;
        }
        let record = Document::read(&fs::read(&path).expect("a record")).expect("a record");
        found.push((name, timeless(masked(&record, base, asked, user))));
    }
    found.sort_by(|left, right| left.0.cmp(&right.0));
    Document::Record(found)
}

/// What a large tree holds, as the recorder showed one: each entry's kind,
/// its target, or its mode, size and the digest of its bytes.
fn digests(root: &Path) -> Document {
    let walked = walked(root);
    let mut shown = Vec::new();
    for (name, entry) in walked.fields().expect("a tree") {
        let path = root.join(name);
        let kind = path.symlink_metadata().expect("an entry");
        let mode = Document::from(u64::from(kind.permissions().mode() & 0o7777));
        let shown_entry = if let Some(target) = entry.get("link") {
            Document::record([("link", target.clone())])
        } else if entry.get("directory").is_some() {
            Document::record([("directory", true.into()), ("mode", mode)])
        } else {
            let bytes = fs::read(&path).expect("bytes");
            Document::record([
                ("sha256", crate::digest(&bytes).into()),
                ("mode", mode),
                ("bytes", kind.len().into()),
            ])
        };
        shown.push((name.clone(), shown_entry));
    }
    Document::Record(shown)
}

/// The names of what a tree holds, but what lies below `skipped`.
fn names(root: &Path, skipped: Option<&str>) -> Vec<String> {
    let walked = walked(root);
    let fields = walked.fields().expect("a tree");
    let kept = fields.iter().map(|(name, _)| name.clone());
    kept.filter(|name| skipped.is_none_or(|skipped| !name.starts_with(skipped)))
        .collect()
}

/// The bounds a case was recorded with.
fn bounded(case: &Document) -> Limits {
    let mut limits = Limits::REVIEWED;
    let bound = |name: &str| {
        case.get(name)
            .and_then(Document::whole)
            .map(|value| u64::try_from(value).expect("a bound"))
    };
    if let Some(files) = bound("files") {
        limits.projected_files = files;
    }
    if let Some(total) = bound("total") {
        limits.projected_bytes = total;
    }
    if let Some(member) = bound("member") {
        limits.member = member;
    }
    if let Some(reserve) = bound("reserve") {
        limits.reserve = reserve;
    }
    if let Some(database) = bound("limit") {
        limits.database = database;
    }
    limits
}

/// The free space a case was recorded with: what it says, or room enough.
fn free(case: &Document) -> impl Fn(&Path) -> Result<u64> + use<> {
    let given = case
        .get("free")
        .and_then(Document::whole)
        .map(|free| u64::try_from(free).expect("a size"));
    move |_| Ok(given.unwrap_or(u64::MAX))
}

#[test]
fn the_secret_arguments_and_bounds_are_the_reviewed_ones() {
    let recording = recording();
    assert_eq!(SECRET_ARGUMENTS.to_vec(), texts(&recording, "arguments"));
    let limits = recording.get("limits").expect("the limits");
    let reviewed = Limits::REVIEWED;
    for (held, recorded) in [
        (reviewed.projected_files, "MAX_SECRET_FILES"),
        (reviewed.projected_bytes, "MAX_SECRET_BYTES"),
        (reviewed.reserve, "SECRET_DISK_RESERVE"),
        (reviewed.member, "MAX_FILE"),
        (crate::secrets::PREFIX_BYTES, "PREFIX"),
    ] {
        assert_eq!(held, whole(limits, recorded), "{recorded}");
    }
    assert_eq!(reviewed.database, 4 << 30);
    assert_eq!(reviewed.report, 64 << 20);
    assert_eq!(reviewed.export, 65_536);
    assert_eq!(reviewed.native, 8 << 20);
}

/// Bundles that the reference failed on in its interpreter's words: none.
#[test]
fn bundles_match_the_recording() {
    let mut all = Vec::new();
    for case in cases("bundles") {
        let name = text(&case, "name").to_owned();
        let scratch = Scratch::new("bundle");
        let base = scratch.0.clone();
        let layers = built(&case, &base);
        let output = base.join("output");
        fs::create_dir(&output).expect("an output");
        let limits = bounded(&case);
        let kind = case
            .get("cancel")
            .and_then(Document::text)
            .map(str::to_owned);
        let first = output.join("secret-input/content-000000");
        let cancelled = || match kind.as_deref() {
            Some("before") => true,
            Some("after first") => first.exists(),
            _ => false,
        };
        let came = secret_bundle(&layers, &output, &cancelled, &limits, &free(&case)).map(|path| {
            let relative = path.strip_prefix(&base).expect("below the scratch");
            Document::from(relative.to_str().expect("text"))
        });
        let paths = fs::read(output.join("secret-paths.json")).ok();
        let paths = paths.map_or(Document::Null, |bytes| {
            Document::read(&bytes).expect("a map")
        });
        assert_eq!(Some(&paths), case.get("paths"), "{name}");
        let inputs = output.join("secret-input");
        let shown = if inputs.exists() {
            digests(&inputs)
        } else {
            Document::Null
        };
        assert_eq!(Some(&shown), case.get("inputs"), "{name}");
        reopened(&base);
        assert_eq!(names(&output, None), texts(&case, "afterNames"), "{name}");
        all.push((name, here(came), there(&case)));
    }
    assert_eq!(tallied(all, &[]), [9, 10, 0, 0, 0]);
}

/// Self-tests that the reference failed on in its interpreter's words: a
/// report that is not there or is no JSON.
const UNREAD_SELFTESTS: [(&str, &str); 2] = [
    ("the detector writes no report", "tool_entry_missing"),
    ("the detector writes no JSON", "image_report_invalid"),
];

#[test]
fn selftests_match_the_recording() {
    let mut all = Vec::new();
    for case in cases("selftests") {
        let name = text(&case, "name").to_owned();
        let scratch = Scratch::new("selftest");
        let base = scratch.0.clone();
        let root = base.join("evidence");
        fs::create_dir(&root).expect("evidence");
        let (mut sandbox, asked) = sandboxed(&root, &case);
        let came = secret_selftest(&mut sandbox, &pins(), &Limits::REVIEWED, &|_| Ok(u64::MAX));
        let came = came.map(|()| Document::Null);
        let (uid, gid) = sandbox.identity;
        let user = format!("{uid}:{gid}");
        let asked = asked.lock().unwrap_or_else(PoisonError::into_inner);
        let calls = masked(&Document::List(asked.calls.clone()), &base, &asked, &user);
        assert_eq!(Some(&calls), case.get("calls"), "{name}");
        assert_eq!(
            Some(&evidence(&root, &base, &asked, &user)),
            case.get("evidence"),
            "{name}"
        );
        let paths = fs::read(root.join("secret-canary/secret-paths.json")).ok();
        let paths = paths.map_or(Document::Null, |bytes| {
            Document::read(&bytes).expect("a map")
        });
        assert_eq!(Some(&paths), case.get("paths"), "{name}");
        reopened(&base);
        assert_eq!(names(&root, None), texts(&case, "afterNames"), "{name}");
        all.push((name, here(came), there(&case)));
    }
    assert_eq!(
        tallied(all, &[(&UNREAD_SELFTESTS, There::Raised)]),
        [2, 10, 2, 0, 0]
    );
}

/// Preparations that the reference failed on in its interpreter's words:
/// a status that is no JSON or no record.
const UNREAD_DATABASES: [(&str, &str); 2] = [
    ("a status that is no JSON", "image_database_status_unread"),
    ("a status that is no record", "image_database_status_unread"),
];

#[test]
fn databases_match_the_recording() {
    let reviewed =
        crate::image::policy::load_policy(&checkout().join("security/image-policy.json"))
            .expect("a policy");
    let mut all = Vec::new();
    for case in cases("databases") {
        let name = text(&case, "name").to_owned();
        let scratch = Scratch::new("database");
        let base = scratch.0.clone();
        let root = base.join("evidence");
        fs::create_dir(&root).expect("evidence");
        let (mut sandbox, asked) = sandboxed(&root, &case);
        if case.get("cached") == Some(&Document::Bool(true)) {
            sandbox.cache = Some(Cache {
                directory: base.join("cache"),
                pin: "b".repeat(64),
                limits: cache::Limits::REVIEWED,
            });
        }
        let came = prepare_database(&mut sandbox, &reviewed, now(), &bounded(&case));
        let (uid, gid) = sandbox.identity;
        let user = format!("{uid}:{gid}");
        let asked = asked.lock().unwrap_or_else(PoisonError::into_inner);
        let came = came.map(|held| masked(&held, &base, &asked, &user));
        let calls = masked(&Document::List(asked.calls.clone()), &base, &asked, &user);
        assert_eq!(Some(&calls), case.get("calls"), "{name}");
        assert_eq!(
            Some(&evidence(&root, &base, &asked, &user)),
            case.get("evidence"),
            "{name}"
        );
        let database = root.join("database");
        let shown = if database.exists() {
            digests(&database)
        } else {
            Document::Null
        };
        assert_eq!(Some(&shown), case.get("database"), "{name}");
        let cache = base.join("cache");
        let saved = if cache.exists() {
            digests(&cache)
        } else {
            Document::Null
        };
        assert_eq!(Some(&saved), case.get("saved"), "{name}");
        let receipt = fs::read(cache.join("receipt.json")).ok();
        let receipt = receipt.map_or(Document::Null, |bytes| {
            Document::read(&bytes).expect("a receipt")
        });
        assert_eq!(Some(&receipt), case.get("receipt"), "{name}");
        assert_eq!(
            Some(&Document::Bool(sandbox.cancelled())),
            case.get("cancelled"),
            "{name}"
        );
        reopened(&base);
        assert_eq!(names(&root, None), texts(&case, "afterNames"), "{name}");
        all.push((name, here(came), there(&case)));
    }
    assert_eq!(
        tallied(all, &[(&UNREAD_DATABASES, There::Raised)]),
        [3, 8, 2, 0, 0]
    );
}

/// The checkout, whose reviewed files the scans read.
fn checkout() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    root.canonicalize().expect("the checkout")
}

/// Catalogs that the reference failed on in its interpreter's words: an
/// inventory that is not there or is no JSON, and an archive that is not
/// there.
const UNREAD_SBOMS: [(&str, &str); 3] = [
    ("a catalog that writes nothing", "tool_entry_missing"),
    ("a catalog that writes no JSON", "image_report_invalid"),
    ("an archive that is not there", "tool_entry_missing"),
];

#[test]
fn catalogs_match_the_recording() {
    let recording = recording();
    let reviewed = recording.get("imagePolicy").cloned().expect("a policy");
    let mut all = Vec::new();
    for case in cases("sboms") {
        let name = text(&case, "name").to_owned();
        let scratch = Scratch::new("sbom");
        let base = scratch.0.clone();
        let root = base.join("evidence");
        fs::create_dir(&root).expect("evidence");
        let artifact = base.join("artifact");
        fs::create_dir(&artifact).expect("an artifact");
        match case.get("archive") {
            Some(Document::Text(kind)) if kind == "link" => {
                fs::write(base.join("elsewhere.tar"), b"archive bytes").expect("an archive");
                std::os::unix::fs::symlink("../elsewhere.tar", artifact.join("image.tar"))
                    .expect("a link");
            }
            Some(Document::Text(bytes)) => {
                fs::write(artifact.join("image.tar"), bytes).expect("an archive")
            }
            _ => {}
        }
        let (mut sandbox, asked) = sandboxed(&root, &case);
        let native = case.get("native").expect("native");
        let elf = case.get("elf").expect("an audit");
        let came = scan_sbom(
            &mut sandbox,
            &artifact,
            native,
            elf,
            &reviewed,
            &pins(),
            &Limits::REVIEWED,
        );
        let came = came.map(|path| {
            Document::from(
                path.strip_prefix(&base)
                    .expect("below")
                    .to_str()
                    .expect("text"),
            )
        });
        let (uid, gid) = sandbox.identity;
        let user = format!("{uid}:{gid}");
        let asked = asked.lock().unwrap_or_else(PoisonError::into_inner);
        let calls = masked(&Document::List(asked.calls.clone()), &base, &asked, &user);
        assert_eq!(Some(&calls), case.get("calls"), "{name}");
        assert_eq!(
            Some(&evidence(&root, &base, &asked, &user)),
            case.get("evidence"),
            "{name}"
        );
        let enriched = fs::read(root.join("sbom/sbom.syft.json")).ok();
        let enriched = enriched.map_or(Document::Null, |bytes| crate::digest(&bytes).into());
        assert_eq!(Some(&enriched), case.get("enrichedSha256"), "{name}");
        let mut trees = Vec::new();
        for tree in ["scanner-input", "sbom-raw", "sbom", "spdx"] {
            if root.join(tree).exists() {
                trees.push((tree.to_owned(), digests(&root.join(tree))));
            }
        }
        assert_eq!(Some(&Document::Record(trees)), case.get("trees"), "{name}");
        reopened(&base);
        assert_eq!(names(&root, None), texts(&case, "afterNames"), "{name}");
        all.push((name, here(came), there(&case)));
    }
    assert_eq!(
        tallied(all, &[(&UNREAD_SBOMS, There::Raised)]),
        [2, 4, 3, 0, 0]
    );
}

#[test]
fn secret_scans_match_the_recording() {
    let mut all = Vec::new();
    for case in cases("secretScans") {
        let name = text(&case, "name").to_owned();
        let scratch = Scratch::new("secret-scan");
        let base = scratch.0.clone();
        let root = base.join("evidence");
        fs::create_dir(&root).expect("evidence");
        let tree = base.join("tree");
        fs::create_dir(&tree).expect("a tree");
        let layered = Document::record([
            ("tree", case.get("layers").cloned().expect("layers")),
            ("rootfs", "root".into()),
        ]);
        let layers = built(&layered, &tree);
        fs::rename(&layers, tree.join("layers")).expect("the layers");
        let (mut sandbox, asked) = sandboxed(&root, &case);
        let came = scan_secrets(&mut sandbox, &tree, &Limits::REVIEWED, &|_| Ok(u64::MAX));
        let came = came.map(|path| {
            Document::from(
                path.strip_prefix(&base)
                    .expect("below")
                    .to_str()
                    .expect("text"),
            )
        });
        let (uid, gid) = sandbox.identity;
        let user = format!("{uid}:{gid}");
        let asked = asked.lock().unwrap_or_else(PoisonError::into_inner);
        let calls = masked(&Document::List(asked.calls.clone()), &base, &asked, &user);
        assert_eq!(Some(&calls), case.get("calls"), "{name}");
        assert_eq!(
            Some(&evidence(&root, &base, &asked, &user)),
            case.get("evidence"),
            "{name}"
        );
        reopened(&base);
        assert_eq!(
            names(&root, Some("secret-input/")),
            texts(&case, "afterNames"),
            "{name}"
        );
        all.push((name, here(came), there(&case)));
    }
    assert_eq!(tallied(all, &[]), [2, 2, 0, 0, 0]);
}

/// Checks that the reference failed on in its interpreter's words: a report
/// that is no JSON.
const UNREAD_CHECKS: [(&str, &str); 1] =
    [("checks of a report that is no JSON", "image_report_invalid")];

#[test]
fn checks_match_the_recording() {
    let mut all = Vec::new();
    for case in cases("checks") {
        let name = text(&case, "name").to_owned();
        let scratch = Scratch::new("checks");
        let base = scratch.0.clone();
        let output = base.join("evidence");
        fs::create_dir(&output).expect("evidence");
        let layers = base.join("layers");
        fs::create_dir(&layers).expect("layers");
        let token = format!("TOKEN={} # gitleaks:allow\n", canary());
        fs::write(layers.join("env"), token).expect("a file");
        secret_bundle(&layers, &output, &|| false, &Limits::REVIEWED, &|_| {
            Ok(u64::MAX)
        })
        .expect("a bundle");
        let secrets = output.join("secrets");
        fs::create_dir(&secrets).expect("secrets");
        let said = match case.get("said").and_then(Document::text) {
            Some(said) => said.to_owned(),
            None => case.get("report").expect("a report").spaced(),
        };
        fs::write(secrets.join("gitleaks.json"), said).expect("a report");
        let came = secret_checks(&secrets, &output, &[], &pins(), &Limits::REVIEWED);
        reopened(&base);
        all.push((name, here(came), there(&case)));
    }
    assert_eq!(
        tallied(all, &[(&UNREAD_CHECKS, There::Raised)]),
        [3, 0, 1, 0, 0]
    );
}

#[test]
fn phases_match_the_recording() {
    let mut all = Vec::new();
    for case in cases("phases") {
        let name = text(&case, "name").to_owned();
        let scratch = Scratch::new("phase");
        let cancelled = AtomicBool::new(case.get("cancelled") == Some(&Document::Bool(true)));
        let phase = text(&case, "phase");
        let work = || -> Result<Document> {
            if name == "a phase that is refused" {
                Err(Fault("image_sbom_failed"))
            } else {
                Ok("done".into())
            }
        };
        let came = scan_phase(&scratch.0, &cancelled, phase, work);
        let mut receipts = Vec::new();
        for entry in fs::read_dir(&scratch.0).expect("receipts") {
            let path = entry.expect("a receipt").path();
            let named = path
                .file_name()
                .expect("a name")
                .to_str()
                .expect("text")
                .to_owned();
            let record = Document::read(&fs::read(&path).expect("a receipt")).expect("a record");
            receipts.push((named, timeless(record)));
        }
        receipts.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(
            Some(&Document::Record(receipts)),
            case.get("receipts"),
            "{name}"
        );
        all.push((name, here(came), there(&case)));
    }
    assert_eq!(tallied(all, &[]), [2, 1, 0, 0, 0]);
}

#[test]
fn dispositions_match_the_recording() {
    let recording = recording();
    let policy = text(&recording, "advisoryPolicySha256").to_owned();
    let mut all = Vec::new();
    for case in cases("dispositions") {
        let name = text(&case, "name").to_owned();
        let scratch = Scratch::new("disposition");
        let output = scratch.0.join("evidence");
        fs::create_dir_all(output.join("spdx")).expect("evidence");
        if case.get("spdx") == Some(&Document::Bool(true)) {
            fs::write(
                output.join("spdx/sbom.spdx.json"),
                r#"{"spdxVersion": "SPDX-2.3"}"#,
            )
            .expect("a conversion");
        }
        let manifest = case.get("manifest").cloned().expect("a manifest");
        let image = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let disposition = Disposition {
            manifest: &manifest,
            image_id: image,
            output: &output,
            policy: &policy,
            now: moment(),
        };
        let mut outcome = Document::record([("imageId", image.into())]);
        let mut asked = 0;
        let proven = case.get("proven").cloned().expect("a proof");
        let refused = case
            .get("refusedProof")
            .and_then(Document::text)
            .map(str::to_owned);
        let mut prove = |_: &str| -> Result<Document> {
            asked += 1;
            match refused.as_deref() {
                Some("runtime_proof_failed") => Err(Fault("runtime_proof_failed")),
                Some(other) => panic!("no refusal {other}"),
                None => Ok(proven.clone()),
            }
        };
        let verdict = case.get("verdict").cloned().expect("a verdict");
        let came = runtime_disposition(verdict, &disposition, &mut outcome, &mut prove);
        let recorded_asked = case
            .get("asked")
            .and_then(Document::items)
            .map_or(0, <[Document]>::len);
        assert_eq!(asked, recorded_asked, "{name}");
        if here(came.clone()) == there(&case) {
            let mut written = Vec::new();
            for entry in fs::read_dir(&output).expect("evidence") {
                let path = entry.expect("an entry").path();
                if path.is_file() {
                    let named = path
                        .file_name()
                        .expect("a name")
                        .to_str()
                        .expect("text")
                        .to_owned();
                    written.push((
                        named,
                        Document::read(&fs::read(&path).expect("a file")).expect("a record"),
                    ));
                }
            }
            written.sort_by(|left, right| left.0.cmp(&right.0));
            // The statements name this gate as what made them, where the
            // reference named its own source file.
            let recorded = case.get("written").expect("written").compact();
            let recorded =
                recorded.replace("build/security_runtime.py;", "simplestchat-security image;");
            let recorded = Document::read(recorded.as_bytes()).expect("written");
            assert_eq!(Document::Record(written), recorded, "{name}");
            let statement = fs::read(output.join("vex.openvex.json")).ok();
            let made = outcome
                .get("vexSha256")
                .and_then(Document::text)
                .map(str::to_owned);
            assert_eq!(
                made,
                statement.map(|statement| crate::digest(&statement)),
                "{name}"
            );
            let unsigned = |outcome: &Document| {
                let mut outcome = outcome.clone().sorted();
                outcome.put("vexSha256", Document::Null);
                outcome
            };
            let expected = case.get("outcome").map(unsigned);
            assert_eq!(Some(unsigned(&outcome)), expected, "{name}");
        }
        all.push((name, here(came), there(&case)));
    }
    assert_eq!(tallied(all, &[]), [3, 2, 0, 0, 0]);
}

/// Bindings that the reference failed on in its interpreter's words: a
/// record of the export that is not there, no record or no JSON, and every
/// manifest the release verifier refuses.
const UNREAD_BINDINGS: [(&str, &str); 10] = [
    ("an export that is not there", "tool_entry_missing"),
    ("an export that is no record", "image_export_identity"),
    ("an export that is no JSON", "image_report_invalid"),
    ("a manifest that is not there", "release_manifest_invalid"),
    ("a manifest with another field", "release_manifest_invalid"),
    ("a manifest for arm", "release_manifest_invalid"),
    ("a manifest of a short revision", "release_manifest_invalid"),
    (
        "a manifest whose tag is another's",
        "release_manifest_invalid",
    ),
    ("a manifest without migrations", "release_manifest_invalid"),
    (
        "a manifest of an untimely moment",
        "release_manifest_invalid",
    ),
];

#[test]
fn bindings_match_the_recording() {
    let mut all = Vec::new();
    for case in cases("bindings") {
        let name = text(&case, "name").to_owned();
        let scratch = Scratch::new("binding");
        let artifact = built(&case, &scratch.0);
        let platform = Platform::parsed(text(&case, "platform")).expect("a platform");
        let came = bind_archive(
            text(&case, "imageId"),
            &artifact,
            platform,
            &Limits::REVIEWED,
        );
        all.push((name, here(came), there(&case)));
    }
    assert_eq!(
        tallied(all, &[(&UNREAD_BINDINGS, There::Raised)]),
        [3, 12, 10, 0, 0]
    );
}

/// Selections that the reference failed on in its interpreter's words:
/// what the engine said that is no record of an image, a report that is not
/// there or lacks what it must hold, and the engine's own failure.
const UNREAD_SELECTIONS: [(&str, &str); 9] = [
    (
        "an inspection that is no JSON",
        "image_selected_archive_mismatch",
    ),
    (
        "an inspection that is no record",
        "image_selected_archive_mismatch",
    ),
    (
        "an image whose identity is no text",
        "image_selected_archive_mismatch",
    ),
    (
        "an image without its layers",
        "image_selected_archive_mismatch",
    ),
    (
        "an image whose architecture is no text",
        "image_selected_archive_mismatch",
    ),
    ("a report that is not there", "tool_entry_missing"),
    (
        "a report whose layers lack their identity",
        "image_selected_archive_mismatch",
    ),
    (
        "a report whose configuration is no text",
        "image_selected_archive_mismatch",
    ),
    ("an inspection that fails", "image_engine_command_failed"),
];

#[test]
fn selections_match_the_recording() {
    let mut all = Vec::new();
    for case in cases("selections") {
        let name = text(&case, "name").to_owned();
        let scratch = Scratch::new("selection");
        let tree = built(&case, &scratch.0);
        let asked = Arc::new(Mutex::new(Asked::default()));
        let failing = case.get("failing") == Some(&Document::Bool(true));
        let engine = Engines {
            runs: Arc::new(Mutex::new(Vec::new())),
            asked: Arc::clone(&asked),
            cancelled: None,
            failing: failing.then_some("image_engine_command_failed"),
            inspected: Some(text(&case, "said").to_owned()),
        };
        let platform = Platform::parsed(text(&case, "platform")).expect("a platform");
        let tools = scratch.0.join("tools");
        let mut sandbox = Sandbox::new(
            Box::new(engine),
            &scratch.0,
            &tools,
            BASE,
            platform,
            Bounds::REVIEWED,
        );
        let image = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let came = selected_image(&mut sandbox, image, platform, &tree, &Limits::REVIEWED);
        let calls = asked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .calls
            .clone();
        assert_eq!(Some(&Document::List(calls)), case.get("calls"), "{name}");
        all.push((name, here(came), there(&case)));
    }
    assert_eq!(
        tallied(all, &[(&UNREAD_SELECTIONS, There::Raised)]),
        [4, 6, 9, 0, 0]
    );
}

/// Native evidence that the reference failed on in its interpreter's words:
/// evidence that lacks a field or holds one of another kind, that is not
/// there, no JSON or no record.
const UNREAD_NATIVES: [(&str, &str); 7] = [
    (
        "evidence without its executable",
        "image_native_evidence_shape",
    ),
    (
        "evidence whose executable is no record",
        "image_native_evidence_shape",
    ),
    (
        "evidence without its native manifest",
        "image_native_source_mismatch",
    ),
    (
        "evidence whose static archives are no list",
        "image_native_inventory_missing",
    ),
    ("evidence that is not there", "tool_entry_missing"),
    ("evidence that is no JSON", "image_report_invalid"),
    ("evidence that is no record", "image_native_evidence_shape"),
];

#[test]
fn natives_match_the_recording() {
    let recording = recording();
    let reviewed_native = recording
        .get("reviewedNative")
        .cloned()
        .expect("the reviewed record");
    let digests = recording.get("nativeDigests").expect("the digests");
    let reviewed = Reviewed {
        native: &reviewed_native,
        native_sha256: text(digests, "manifest"),
        lock_sha256: text(digests, "lock"),
    };
    let mut all = Vec::new();
    for case in cases("natives") {
        let name = text(&case, "name").to_owned();
        let scratch = Scratch::new("native");
        let tree = built(&case, &scratch.0);
        let elf = case.get("elf").expect("an audit");
        let came = native_binding(&tree, elf, &reviewed, &Limits::REVIEWED);
        all.push((name, here(came), there(&case)));
    }
    assert_eq!(
        tallied(all, &[(&UNREAD_NATIVES, There::Raised)]),
        [1, 11, 7, 0, 0]
    );
}

#[test]
fn the_canary_is_derived_and_never_written_out() {
    let canary = canary();
    assert_eq!(canary.len(), 40);
    assert!(canary.starts_with("ghp_"));
    assert!(canary[4..].bytes().all(|byte| byte.is_ascii_alphanumeric()));
    assert_eq!(super::base64(b""), "");
    assert_eq!(super::base64(b"f"), "Zg==");
    assert_eq!(super::base64(b"fo"), "Zm8=");
    assert_eq!(super::base64(b"foo"), "Zm9v");
    assert_eq!(super::base64(&[0xfb, 0xff]), "+/8=");
}

#[test]
fn the_first_offender_in_the_order_of_their_paths_is_refused() {
    let scratch = Scratch::new("offenders");
    let layers = scratch.0.join("layers");
    fs::create_dir(&layers).expect("layers");
    fs::write(layers.join("a"), b"123456").expect("a file");
    for name in ["b", "c", "d"] {
        std::os::unix::fs::symlink("missing", layers.join(name)).expect("a link");
    }
    let output = scratch.0.join("output");
    fs::create_dir(&output).expect("an output");
    let limits = Limits {
        member: 5,
        ..Limits::REVIEWED
    };
    let found = super::secret_inventory(&layers, &output, &|| false, &limits, &|_| Ok(u64::MAX));
    assert_eq!(found, Err(Fault("image_secret_input_size")));
}

#[test]
fn an_input_that_grows_before_its_projection_is_refused() {
    for (member, code) in [
        (5, "secret_projection_source_size"),
        (10, "image_secret_input_changed"),
    ] {
        let scratch = Scratch::new("grown");
        let layers = scratch.0.join("layers");
        fs::create_dir(&layers).expect("layers");
        fs::write(layers.join("a"), b"12345").expect("a file");
        let output = scratch.0.join("output");
        fs::create_dir(&output).expect("an output");
        let limits = Limits {
            member,
            ..Limits::REVIEWED
        };
        let grown = std::cell::Cell::new(false);
        // Asked once more before each projection: the file grows then.
        let cancelled = || {
            if output.join("secret-input").exists() && !grown.replace(true) {
                let mut file = fs::OpenOptions::new()
                    .append(true)
                    .open(layers.join("a"))
                    .expect("a file");
                std::io::Write::write_all(&mut file, b"6").expect("a byte");
            }
            false
        };
        let came = secret_bundle(&layers, &output, &cancelled, &limits, &|_| Ok(u64::MAX));
        assert_eq!(came, Err(Fault(code)), "{member}");
        reopened(&scratch.0);
    }
}

#[test]
fn a_selection_of_a_cancelled_sandbox_is_stopped() {
    let scratch = Scratch::new("cancelled-selection");
    let engine = Engines {
        runs: Arc::new(Mutex::new(Vec::new())),
        asked: Arc::new(Mutex::new(Asked::default())),
        cancelled: None,
        failing: None,
        inspected: Some("{}".to_owned()),
    };
    let tools = scratch.0.join("tools");
    let mut sandbox = Sandbox::new(
        Box::new(engine),
        &scratch.0,
        &tools,
        BASE,
        Platform::Amd64,
        Bounds::REVIEWED,
    );
    sandbox.cancelled.store(true, Ordering::SeqCst);
    let image = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let came = selected_image(
        &mut sandbox,
        image,
        Platform::Amd64,
        &scratch.0,
        &Limits::REVIEWED,
    );
    assert_eq!(came, Err(Fault("command_lease_lost")));
}

#[test]
fn a_phase_whose_receipt_cannot_be_kept_fails() {
    let scratch = Scratch::new("unkept-receipt");
    fs::write(scratch.0.join("scanner-result-phase-sbom.json"), b"{}").expect("a receipt");
    let cancelled = AtomicBool::new(false);
    let came = scan_phase(&scratch.0, &cancelled, "sbom", || -> Result<()> { Ok(()) });
    assert_eq!(came, Err(Fault("image_filesystem_error")));
}
