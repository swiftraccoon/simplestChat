//! Replays of `recorded_check.json`: what the image tier did with each of
//! its recorded command lines, its own context running every check for real
//! and only the process each check starts and the scanners' installation
//! replaced; with direct tests of the command line and of this machine.

use super::{Here, Machine, PATIENCE, Request, concluded, parsed, ran, tiered};
use crate::{
    Fault, Result,
    context::{Context, Outcome},
    document::Document,
    image::{sandbox::tests::texts, secrets::tests::text},
    scratch::Scratch,
};
use std::{
    cell::RefCell,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

const RECORDING: &[u8] = include_bytes!("recorded_check.json");

fn recording() -> Document {
    Document::read(RECORDING).expect("a recording")
}

fn cases() -> Vec<Document> {
    let held = recording()
        .get("cases")
        .and_then(Document::items)
        .map(<[Document]>::to_vec);
    held.expect("cases")
}

/// The checkout this crate was built in.
fn repository() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    root.canonicalize().expect("the checkout")
}

/// A program at `path` that keeps the words it was run with in `log`, and
/// where it was run in `log.cwd`, and ends with `status`.
fn script(path: &Path, log: &Path, status: i128) {
    let log = log.display();
    let body = format!(
        "#!/bin/sh\nfor word in \"$0\" \"$@\"; do printf '%s\\n' \"$word\"; done > '{log}'\n\
         pwd > '{log}.cwd'\nexit {status}\n"
    );
    fs::write(path, body).expect("a program");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("its mode");
}

/// The machine of one case: the exporter's Cargo and the gate, which end as
/// the recorded processes ended, and the recorded installation.
struct Faked {
    cargo: PathBuf,
    program: PathBuf,
    installed: bool,
    base: PathBuf,
    installs: RefCell<Vec<Document>>,
}

impl Faked {
    /// `path` as the recorder named it.
    fn shown(&self, path: &Path) -> String {
        let text = path.to_str().expect("text");
        text.replace(self.base.to_str().expect("text"), "<base>")
    }
}

impl Machine for Faked {
    fn cargo(&self, _context: &mut Context) -> Outcome<PathBuf> {
        Ok(self.cargo.clone())
    }

    fn install(
        &self,
        names: &[String],
        directory: &Path,
        platform: &'static str,
    ) -> Result<PathBuf> {
        let listed = names.iter().map(|name| name.as_str().into()).collect();
        self.installs.borrow_mut().push(Document::record([
            ("names", Document::List(listed)),
            ("directory", self.shown(directory).into()),
            ("platform", platform.into()),
        ]));
        crate::require(self.installed, "tool_executable_digest")?;
        Ok(directory.to_path_buf())
    }

    fn program(&self) -> &Path {
        &self.program
    }
}

/// What one case came to: whether it passed, its line, its summary, the
/// commands it ran, the installations it asked for and every name it left.
#[derive(Debug, PartialEq)]
struct Came {
    code: i128,
    said: String,
    summary: Document,
    commands: Document,
    installs: Document,
    names: Vec<String>,
}

/// The summary at `path` with each check's time named as the recorder
/// named it.
fn timeless(path: &Path) -> Document {
    let mut summary = Document::read(&fs::read(path).expect("a summary")).expect("a record");
    let mut checks = summary
        .get("checks")
        .and_then(Document::items)
        .expect("checks")
        .to_vec();
    for check in &mut checks {
        check.put("elapsedSeconds", "<elapsed>".into());
    }
    summary.put("checks", Document::List(checks));
    summary
}

/// The command `name` ran, if it ran, as the recorder named it.
fn logged(machine: &Faked, base: &Path, root: &Path, name: &str) -> Option<Document> {
    let log = base.join(format!("{name}.argv"));
    let words = fs::read_to_string(&log).ok()?;
    let shown = |word: &str| {
        let word = word.replace(machine.cargo.to_str().expect("text"), "<cargo>");
        let word = word.replace(machine.program.to_str().expect("text"), "<program>");
        machine.shown(Path::new(&word))
    };
    let cwd = fs::read_to_string(base.join(format!("{name}.argv.cwd"))).expect("a place");
    let cwd = cwd
        .trim_end()
        .replace(root.to_str().expect("text"), "<root>");
    let argv = words.lines().map(|word| shown(word).into()).collect();
    Some(Document::record([
        ("argv", Document::List(argv)),
        ("cwd", cwd.into()),
        ("timeout", PATIENCE.as_secs().into()),
    ]))
}

/// The case `case` run here as the recorder ran it.
fn replayed(case: &Document) -> Came {
    let scratch = Scratch::new("tier");
    let base = scratch.0.clone();
    let root = repository();
    let flag = |name: &str| case.get(name) == Some(&Document::Bool(true));
    if flag("artifact") {
        fs::create_dir(base.join("export")).expect("an export");
    }
    match case.get("tools").and_then(Document::text) {
        Some("prepared") => fs::create_dir(base.join("tools")).expect("scanners"),
        Some("link") => {
            fs::create_dir(base.join("real")).expect("scanners");
            std::os::unix::fs::symlink(base.join("real"), base.join("link")).expect("a link");
        }
        _ => {}
    }
    let statuses: Vec<i128> = case
        .get("statuses")
        .and_then(Document::items)
        .expect("statuses")
        .iter()
        .map(|status| status.whole().expect("a status"))
        .collect();
    let status = |at: usize| statuses.get(at).copied().unwrap_or(0);
    let (exported, judged) = if flag("artifact") {
        (0, status(0))
    } else {
        (status(0), status(1))
    };
    let machine = Faked {
        cargo: base.join("cargo"),
        program: base.join("gate"),
        installed: flag("installed"),
        base: base.clone(),
        installs: RefCell::new(Vec::new()),
    };
    script(&machine.cargo, &base.join("cargo.argv"), exported);
    script(&machine.program, &base.join("gate.argv"), judged);
    let unmasked = |word: &str| {
        let word = word.replace("<base>", base.to_str().expect("text"));
        word.replace("<root>", root.to_str().expect("text"))
    };
    let words = texts(case, "argv");
    assert_eq!(words[0], "image");
    let arguments: Vec<String> = words[1..].iter().map(|word| unmasked(word)).collect();
    let request = parsed(&arguments).expect("a request");
    let ambient = |name: &str| std::env::var_os(name);
    let (passed, output) = tiered(&root, &request, &machine, &ambient).expect("evidence");
    let line = crate::tier::proclaimed("image", passed, &output);
    let said = String::from_utf8(line).expect("text");
    let commands = ["cargo", "gate"]
        .iter()
        .filter_map(|name| logged(&machine, &base, &root, name));
    let walked = crate::image::sandbox::tests::walked(&output);
    let names = walked
        .fields()
        .expect("a tree")
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    let came = Came {
        code: i128::from(!passed),
        said: said.replace(base.to_str().expect("text"), "<base>"),
        summary: timeless(&output.join("summary.json")),
        commands: Document::List(commands.collect()),
        installs: Document::List(machine.installs.borrow().clone()),
        names,
    };
    crate::image::sandbox::tests::reopened(&base);
    came
}

/// What `case` recorded, and how many times it ran the gate: this
/// program's own `image scan`.
fn recorded(case: &Document) -> (Came, usize) {
    let commands = case.get("commands").cloned().expect("commands");
    let scan = ["<program>", "image", "scan"].map(Document::from);
    let gates = commands
        .items()
        .expect("commands")
        .iter()
        .filter(|command| {
            let argv = command.get("argv").and_then(Document::items);
            argv.is_some_and(|argv| argv.starts_with(&scan))
        });
    let gates = gates.count();
    let came = Came {
        code: case.get("code").and_then(Document::whole).expect("a code"),
        said: text(case, "said").to_owned(),
        summary: case.get("summary").cloned().expect("a summary"),
        commands,
        installs: case.get("installs").cloned().expect("installs"),
        names: texts(case, "names"),
    };
    (came, gates)
}

#[test]
fn every_recorded_tier_is_run_alike() {
    let (mut tally, mut ran) = ([0; 2], 0);
    for case in cases() {
        let name = text(&case, "name");
        let came = replayed(&case);
        let (expected, gates) = recorded(&case);
        ran += gates;
        assert_eq!(came, expected, "{name}");
        tally[usize::try_from(came.code).expect("a code")] += 1;
    }
    // Five tiers that passed and seven that failed; six ran this program's
    // own gate.
    assert_eq!(tally, [5, 7]);
    assert_eq!(ran, 6);
}

#[test]
fn every_recorded_check_may_take_as_long() {
    for case in cases() {
        for command in case
            .get("commands")
            .and_then(Document::items)
            .expect("commands")
        {
            let timeout = command.get("timeout").and_then(Document::whole);
            assert_eq!(timeout, Some(i128::from(PATIENCE.as_secs())));
        }
    }
}

fn words(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_owned).collect()
}

#[test]
fn a_tier_names_its_image_and_every_option_once() {
    let line = "sha256:x --output /o --engine podman --artifact-dir /a --image-tools /t \
        --image-database-cache /c";
    assert_eq!(
        parsed(&words(line)),
        Some(Request {
            image: "sha256:x".to_owned(),
            output: Some("/o".into()),
            engine: "podman",
            artifact: Some("/a".into()),
            tools: Some("/t".into()),
            cache: Some("/c".into()),
        })
    );
    let bare = parsed(&words("sha256:x")).expect("a tier");
    assert_eq!(
        (bare.engine, bare.output, bare.artifact),
        ("docker", None, None)
    );
    assert_eq!((bare.tools, bare.cache), (None, None));
    for line in [
        "",
        "sha256:x sha256:y",
        "sha256:x --output /o --output /p",
        "sha256:x --engine docker --engine podman",
        "sha256:x --artifact-dir /a --artifact-dir /b",
        "sha256:x --image-tools /t --image-tools /u",
        "sha256:x --image-database-cache /c --image-database-cache /d",
        "sha256:x --engine rkt",
        "sha256:x --output",
        "sha256:x --output -o",
        "sha256:x --unknown /u",
        "--output /o",
        "-x --output /o",
        "sha256:x -x",
    ] {
        assert_eq!(parsed(&words(line)), None, "{line}");
    }
    let empty = vec!["sha256:x".to_owned(), "--output".to_owned(), String::new()];
    assert_eq!(parsed(&empty), None);
    assert_eq!(parsed(&[String::new()]), None);
}

#[test]
fn a_refusal_is_the_tiers_own_fault() {
    let refused: crate::context::Failure = Fault("invalid_prepared_image_tools").into();
    assert_eq!(refused.to_string(), "invalid_prepared_image_tools");
}

#[test]
fn a_tier_that_is_refused_says_so_with_its_evidence() {
    let scratch = Scratch::new("refused");
    let output = scratch.0.join("evidence");
    // Podman, which cannot export, so that no reading of this line reaches
    // an interpreter or an installation.
    let line = format!(
        "sha256:x --engine podman --artifact-dir {} --image-tools relative-tools --output {}",
        scratch.0.display(),
        output.display()
    );
    let mut said = Vec::new();
    let code = ran(&repository(), &words(&line), &mut said);
    assert_eq!(code, std::process::ExitCode::from(1));
    let expected = format!("Security image: failed; evidence: {}\n", output.display());
    assert_eq!(String::from_utf8(said).expect("text"), expected);
    let summary = timeless(&output.join("summary.json"));
    assert_eq!(summary.get("tier"), Some(&"image".into()));
    assert_eq!(
        summary.get("failure"),
        Some(&"invalid_prepared_image_tools".into())
    );
    let mut usage = Vec::new();
    assert_eq!(
        ran(&repository(), &words("--output"), &mut usage),
        std::process::ExitCode::from(2)
    );
    assert!(usage.is_empty());
}

#[test]
fn this_machine_installs_with_this_checkouts_lock_and_runs_this_program() {
    let scratch = Scratch::new("here");
    let root = repository();
    let program = scratch.0.join("program");
    let machine = Here {
        root: &root,
        program: program.clone(),
    };
    assert_eq!(machine.program(), program.as_path());
    let names: Vec<String> = ["syft", "grype", "gitleaks"]
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    let refused = machine.install(&names, &scratch.0.join("tools"), "no-such-platform");
    assert_eq!(refused, Err(Fault("unsupported_tool_platform")));
}

#[test]
fn this_machine_exports_with_the_cargo_of_the_checkouts_pinned_toolchain() {
    let scratch = Scratch::new("pinned");
    let (root, search) = (scratch.0.join("root"), scratch.0.join("bin"));
    let toolchain = scratch.0.join("toolchain/bin");
    for directory in [&root, &search, &toolchain] {
        fs::create_dir_all(directory).expect("a directory");
    }
    let file = "[toolchain]\nchannel = \"1.2.3\"\n";
    fs::write(root.join("rust-toolchain.toml"), file).expect("a toolchain file");
    // `rustup` names a toolchain's cargo only for the channel the file pins.
    let pinned = toolchain.join("cargo");
    let rustup = search.join("rustup");
    let answer = format!(
        "#!/bin/sh\n[ \"$*\" = 'which --toolchain 1.2.3 cargo' ] || exit 3\n\
         printf '%s\\n' '{}'\n",
        pinned.display()
    );
    fs::write(&rustup, answer).expect("rustup");
    fs::set_permissions(&rustup, fs::Permissions::from_mode(0o755)).expect("its mode");
    // The pinned cargo fails, so that the tier stops after the export; the
    // one on the caller's search path would pass.
    script(&search.join("cargo"), &scratch.0.join("unpinned.argv"), 0);
    script(&pinned, &scratch.0.join("pinned.argv"), 1);
    let machine = Here {
        root: &root,
        program: scratch.0.join("gate"),
    };
    let line = format!("sha256:x --output {}", scratch.0.join("evidence").display());
    let request = parsed(&words(&line)).expect("a request");
    let home = scratch.0.join("home");
    let ambient = |name: &str| match name {
        "PATH" => Some(search.clone().into_os_string()),
        "HOME" => Some(home.clone().into_os_string()),
        _ => None,
    };
    let (passed, output) = tiered(&root, &request, &machine, &ambient).expect("evidence");
    assert!(!passed);
    let summary = timeless(&output.join("summary.json"));
    assert_eq!(
        summary.get("failure"),
        Some(&"scanner_command_failed_image-export".into())
    );
    let asked = fs::read_to_string(scratch.0.join("pinned.argv")).expect("the pinned cargo ran");
    let mut expected = vec![pinned.display().to_string()];
    // Written out, not read from the constant it checks: the production
    // builder, built with the release profile, exporting this image.
    expected.extend(
        [
            "run",
            "--locked",
            "-q",
            "--release",
            "-p",
            "simplestchat-ops",
            "--bin",
            "simplestchat-ops",
            "--",
            "release",
            "build",
            "--image-id",
        ]
        .map(str::to_owned),
    );
    let export = output.join("export").display().to_string();
    expected.extend(["sha256:x".to_owned(), "--output".to_owned(), export]);
    assert_eq!(asked.lines().collect::<Vec<_>>(), expected);
    let place = fs::read_to_string(scratch.0.join("pinned.argv.cwd")).expect("a place");
    assert_eq!(
        Path::new(place.trim_end()),
        root.canonicalize().expect("root")
    );
    assert!(!scratch.0.join("unpinned.argv").exists());
}

#[test]
fn only_a_tier_that_passed_and_said_so_ends_well() {
    let lost = || Err(std::io::Error::other("the line was lost"));
    assert_eq!(concluded(true, Ok(())), std::process::ExitCode::SUCCESS);
    assert_eq!(concluded(true, lost()), std::process::ExitCode::from(1));
    assert_eq!(concluded(false, Ok(())), std::process::ExitCode::from(1));
    assert_eq!(concluded(false, lost()), std::process::ExitCode::from(1));
}

#[test]
fn prepared_scanners_named_from_where_the_tier_runs_are_refused() {
    let scratch = Scratch::new("relative");
    let output = scratch.0.join("evidence");
    // A directory there is, named as the tier's own directory names it.
    assert!(Path::new("src").is_dir());
    let line = format!(
        "sha256:x --artifact-dir {} --image-tools src --output {}",
        scratch.0.display(),
        output.display()
    );
    let request = parsed(&words(&line)).expect("a request");
    let machine = Faked {
        cargo: scratch.0.join("cargo"),
        program: scratch.0.join("gate"),
        installed: true,
        base: scratch.0.clone(),
        installs: RefCell::new(Vec::new()),
    };
    let ambient = |name: &str| std::env::var_os(name);
    let (passed, output) = tiered(&repository(), &request, &machine, &ambient).expect("evidence");
    assert!(!passed);
    let summary = timeless(&output.join("summary.json"));
    assert_eq!(
        summary.get("failure"),
        Some(&"invalid_prepared_image_tools".into())
    );
    assert!(machine.installs.borrow().is_empty());
}
