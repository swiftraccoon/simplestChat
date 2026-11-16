//! Tests of the machine the gate runs on for real: each of its steps on
//! small inputs of this host, and the signals that cancel the gate.

use super::{Local, Signals, asked};
use crate::{
    Fault,
    document::Document,
    engine,
    image::{
        gate::{Host, LOCK},
        policy::Moment,
        runtime,
        sandbox::Platform,
    },
    policy::Date,
    scratch::Scratch,
    tools,
};
use std::{
    ffi::OsString,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// The checkout this crate was built in.
fn repository() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    root.canonicalize().expect("the checkout")
}

/// This machine for `root`, with `ambient` as the caller's environment.
fn local<'a>(root: &'a Path, ambient: &'a dyn Fn(&str) -> Option<OsString>) -> Local<'a> {
    Local {
        root,
        engine: engine::Name::Docker,
        ambient,
        today: Date::MIN,
        cancelled: Arc::new(AtomicBool::new(false)),
    }
}

/// The new executable script `path` with `body`.
fn script(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().expect("a directory")).expect("a directory");
    fs::write(path, format!("#!/bin/sh\n{body}\n")).expect("a script");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("its mode");
}

#[test]
fn a_command_is_answered_only_when_it_ends_well_within_its_bounds() {
    let scratch = Scratch::new("asked");
    let (speaking, failing) = (scratch.0.join("speaking"), scratch.0.join("failing"));
    script(&speaking, "printf '%s' \"$1\"");
    script(&failing, "printf 'said'; exit 3");
    let (full, loud) = (scratch.0.join("full"), scratch.0.join("loud"));
    script(&full, "head -c 2097152 /dev/zero");
    script(&loud, "head -c 2097153 /dev/zero");
    let argv = |program: &Path, rest: &[&str]| {
        let mut argv = vec![program.as_os_str().to_owned()];
        argv.extend(rest.iter().map(OsString::from));
        argv
    };
    assert_eq!(asked(&argv(&speaking, &["told"])), Ok(b"told".to_vec()));
    assert_eq!(asked(&argv(&failing, &[])), Err(Fault("command_failed")));
    let printed = asked(&argv(&full, &[])).expect("all it may print");
    assert_eq!(printed.len(), 2 << 20);
    assert_eq!(asked(&argv(&loud, &[])), Err(Fault("command_failed")));
    assert_eq!(asked(&[]), Err(Fault("command_failed")));
}

#[test]
fn the_engine_is_found_on_the_callers_search_path() {
    let scratch = Scratch::new("engines");
    let root = repository();
    let empty = scratch.0.join("empty");
    fs::create_dir(&empty).expect("a directory");
    let search = empty.clone().into_os_string();
    let ambient = move |name: &str| (name == "PATH").then(|| search.clone());
    assert_eq!(
        local(&root, &ambient).prefix(),
        Err(Fault("engine_missing"))
    );
    let bin = scratch.0.join("bin");
    let listing = concat!(
        r#"[{"Name":"podman-machine-default","#,
        r#""URI":"ssh://core@127.0.0.1:52000/run/podman/podman.sock","Default":true}]"#,
    );
    script(&bin.join("podman"), &format!("printf '%s' '{listing}'"));
    let search = bin.clone().into_os_string();
    let ambient = move |name: &str| (name == "PATH").then(|| search.clone());
    let mut podman = local(&root, &ambient);
    podman.engine = engine::Name::Podman;
    let program = bin.join("podman").into_os_string();
    let expected: Vec<OsString> = if cfg!(target_os = "macos") {
        vec![
            program,
            "--connection".into(),
            "podman-machine-default".into(),
        ]
    } else {
        vec![program, "--remote=false".into()]
    };
    assert_eq!(podman.prefix(), Ok(expected));
}

#[test]
fn docker_is_reached_through_the_socket_of_this_host_or_of_its_home() {
    let scratch = Scratch::new("docker");
    let root = repository();
    let (bin, home) = (scratch.0.join("bin"), scratch.0.join("home"));
    script(&bin.join("docker"), "exit 0");
    let socket = home.join(".docker/run/docker.sock");
    fs::create_dir_all(socket.parent().expect("a directory")).expect("a directory");
    fs::write(&socket, b"").expect("a socket's place");
    let (search, house) = (bin.clone().into_os_string(), home.into_os_string());
    let ambient = move |name: &str| match name {
        "PATH" => Some(search.clone()),
        "HOME" => Some(house.clone()),
        _ => None,
    };
    let host = Path::new("/var/run/docker.sock");
    let reached = if host.exists() {
        host
    } else {
        socket.as_path()
    };
    let expected: Vec<OsString> = vec![
        bin.join("docker").into_os_string(),
        "--host".into(),
        format!("unix://{}", reached.display()).into(),
    ];
    assert_eq!(local(&root, &ambient).prefix(), Ok(expected));
}

#[test]
fn git_answers_in_the_checkout_within_its_bounds() {
    let scratch = Scratch::new("git");
    let (checkout, elsewhere) = (scratch.0.join("checkout"), scratch.0.join("elsewhere"));
    fs::create_dir(&elsewhere).expect("a directory");
    let made = std::process::Command::new("git")
        .args(["init", "-q"])
        .arg(&checkout)
        .status();
    assert!(made.expect("git").success());
    let search = std::env::var_os("PATH");
    let ambient = move |name: &str| (name == "PATH").then(|| search.clone()).flatten();
    let inside = ["rev-parse".to_owned(), "--is-inside-work-tree".to_owned()];
    assert_eq!(
        local(&checkout, &ambient).git(&inside, 64),
        Ok(b"true\n".to_vec())
    );
    let refused = Err(Fault("runtime_source_git"));
    assert_eq!(local(&checkout, &ambient).git(&inside, 4), refused);
    assert_eq!(local(&elsewhere, &ambient).git(&inside, 64), refused);
    let unknown = ["no-such-command".to_owned()];
    assert_eq!(local(&checkout, &ambient).git(&unknown, 64), refused);
    let nowhere = scratch.0.join("nowhere");
    fs::create_dir(&nowhere).expect("a directory");
    let search = nowhere.into_os_string();
    let lost = move |name: &str| (name == "PATH").then(|| search.clone());
    assert_eq!(local(&checkout, &lost).git(&inside, 64), refused);
}

#[test]
fn an_audit_that_finds_no_executable_is_written_and_refused() {
    let scratch = Scratch::new("audit");
    let rootfs = scratch.0.join("rootfs");
    fs::create_dir(&rootfs).expect("a root");
    let report = scratch.0.join("elf.json");
    let root = repository();
    let refused = local(&root, &|_| None).audit(&rootfs, Platform::Amd64, &report);
    assert_eq!(refused, Err(Fault("image_elf_audit_failed")));
    let written = Document::read(&fs::read(&report).expect("a report")).expect("a record");
    let expected = Document::record([
        ("passed", false.into()),
        ("reason", "elf_filesystem_error".into()),
    ]);
    assert_eq!(written, expected);
    let mode = fs::metadata(&report)
        .expect("a report")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn an_archive_that_is_not_there_is_not_extracted() {
    let scratch = Scratch::new("extract");
    let root = repository();
    let (archive, manifest) = (scratch.0.join("image.tar"), scratch.0.join("release.json"));
    let tree = scratch.0.join("archive");
    let refused = local(&root, &|_| None).extract(&archive, &manifest, &tree);
    assert_eq!(refused, Err(Fault("image_archive_file")));
    assert!(!tree.exists());
}

#[test]
fn the_engine_runs_through_its_prefix_in_its_environment_from_the_checkout() {
    let scratch = Scratch::new("runner");
    let logs = scratch.0.join("logs");
    fs::create_dir(&logs).expect("logs");
    fs::set_permissions(&logs, fs::Permissions::from_mode(0o700)).expect("private logs");
    // The caller's home is not handed on: only the environment given is.
    assert!(std::env::var_os("HOME").is_some());
    let shell = r#"printf '%s:%s:' "$LC_ALL" "${HOME-unset}"; pwd"#;
    let prefix: Vec<OsString> = vec!["/bin/sh".into(), "-c".into(), shell.into(), "sh".into()];
    let root = scratch.0.clone();
    let machine = local(&root, &|_| None);
    let mut engine = machine.engine(&prefix, vec![("LC_ALL", "C".into())], logs.clone());
    assert_eq!(engine.name(), "sh");
    assert!(engine.log().is_some_and(|log| log.starts_with(&logs)));
    let answered = engine.command(&[], 30, false, &mut || true);
    let expected = format!("C:unset:{}", root.display());
    assert_eq!(answered, Ok((0, expected)));
}

#[test]
fn scanners_are_looked_for_with_this_checkouts_lock_and_cache() {
    let scratch = Scratch::new("site");
    let root = repository();
    let machine = local(&root, &|_| None);
    let answered = machine.sited("linux-x86_64", |site| {
        assert_eq!(site.lock, root.join(LOCK));
        assert_eq!(site.cache, root.join(".cache/security-tools"));
        assert_eq!(site.platform, "linux-x86_64");
        let reviewed = tools::Limits::REVIEWED;
        assert_eq!(site.limits.archive, reviewed.archive);
        assert_eq!(site.limits.executable, reviewed.executable);
        assert_eq!(site.limits.members, reviewed.members);
        assert_eq!(site.limits.unpacked, reviewed.unpacked);
        Ok(PathBuf::from("answered"))
    });
    assert_eq!(answered, Ok(PathBuf::from("answered")));
    let lock = root.join(LOCK);
    let cache = root.join(".cache/security-tools");
    let site = tools::Site {
        lock: &lock,
        cache: &cache,
        platform: "linux-x86_64",
        limits: tools::Limits::REVIEWED,
    };
    // An installation of its own, whose receipt is no receipt: refused for
    // what it holds, where the checkout's default one is not even there.
    let installed = scratch.0.join("installed");
    fs::create_dir_all(installed.join("bin")).expect("an installation");
    for directory in [&installed, &installed.join("bin")] {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).expect("private");
    }
    fs::write(installed.join("receipt.json"), b"{}").expect("a receipt");
    let receipt = installed.join("receipt.json");
    fs::set_permissions(&receipt, fs::Permissions::from_mode(0o600)).expect("private");
    let directly = tools::path(&site, "syft", Some(&installed));
    assert!(directly.is_err());
    assert_ne!(directly, tools::path(&site, "syft", None));
    assert_eq!(machine.locate("syft", "linux-x86_64", &installed), directly);
    let unsupported = machine.install("no-such-platform", &scratch.0.join("tools"));
    assert_eq!(unsupported, Err(Fault("unsupported_tool_platform")));
}

#[test]
fn the_proof_is_the_runtime_proof_of_what_it_binds() {
    let scratch = Scratch::new("proof");
    let root = repository();
    let document = Document::record([]);
    let evidence = runtime::Evidence {
        tree: &scratch.0,
        elf: &document,
        native: &document,
        manifest: &document,
        root: &root,
        sbom_sha256: "",
        advisory_policy_sha256: "",
    };
    let limits = runtime::Limits::REVIEWED;
    let directly = runtime::proof(&evidence, &limits, &mut |_| Err(Fault("never_reviewed")));
    assert!(directly.is_err());
    assert_eq!(local(&root, &|_| None).prove(&evidence), directly);
}

#[test]
fn the_machine_answers_for_its_clock_environment_and_cancellation() {
    let root = repository();
    let ambient = |name: &str| (name == "ASKED").then(|| OsString::from("answer"));
    let machine = local(&root, &ambient);
    assert_eq!(machine.ambient("ASKED"), Some("answer".into()));
    assert_eq!(machine.ambient("OTHER"), None);
    let before = Moment::now();
    let now = machine.now();
    assert!(before <= now && now <= Moment::now());
    machine.cancelled().store(true, Ordering::SeqCst);
    assert!(machine.cancelled.load(Ordering::SeqCst));
}

#[test]
fn sigint_and_sigterm_cancel_while_the_signals_last() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let signals = Signals::cancelling(&cancelled).expect("the signals");
    assert_eq!(signals.0.len(), 2);
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        cancelled.store(false, Ordering::SeqCst);
        signal_hook::low_level::raise(signal).expect("a signal");
        assert!(cancelled.load(Ordering::SeqCst), "{signal}");
    }
    drop(signals);
}
