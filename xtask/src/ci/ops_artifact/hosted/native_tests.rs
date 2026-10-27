//! Actual reserved Native groups with fixed inert curl/dpkg/cc fixtures.

use super::super::namespace::Budget;
use super::{
    fetch,
    files::Tree,
    fixture, plan,
    tools::{InvocationSpec, Role, Tools},
};
use crate::{
    ci::{
        command::Command,
        process::{Executor, Native},
    },
    owned_process::Signals,
    tooling::{Environment, Failure, Result},
};
use std::{
    fs,
    io::{Seek, SeekFrom, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    sync::Arc,
    time::Duration,
};

fn tools(fixture: &fixture::Fixture, body: &str, budget: &Budget) -> Tools {
    fs::create_dir(fixture.root.join("tools")).unwrap();
    for name in ["git", "curl", "dpkg-deb"] {
        let text = if name == "git" {
            "#!/bin/sh\nexit 0\n".to_owned()
        } else {
            format!(
                concat!(
                    "#!/bin/sh\nset -eu\n[ -z \"${{GH_TOKEN+x}}\" ]\n",
                    "[ -z \"${{HTTPS_PROXY+x}}\" ]\nprintf A >> ../did-run\n{}\n"
                ),
                body
            )
        };
        let path = fixture.root.join("tools").join(name);
        fs::write(&path, text).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    Tools::select(
        &fixture.root,
        &Environment::from([
            ("PATH".into(), fixture.root.join("tools").into_os_string()),
            ("GH_TOKEN".into(), "PRIVATE-token".into()),
            ("HTTPS_PROXY".into(), "https://PRIVATE.invalid".into()),
        ]),
        budget,
    )
    .unwrap()
}
fn closed(budget: &Budget) {
    assert_eq!(budget.count(), 0);
    let (attempted, fallback) = budget.close_counts();
    assert!(attempted > 0);
    assert_eq!(fallback, 0);
}
fn download() -> fetch::Download {
    fetch::Download {
        url: "https://static.crates.io/crates/inert/inert-1.0.0.crate".into(),
        sha256: super::super::sha(b"archive-bytes"),
        bytes: Some(13),
        maximum: 32,
    }
}

#[tokio::test]
async fn actual_curl_original_streams_are_independent_and_environment_is_credential_free() {
    let fixture = fixture::Fixture::new();
    let budget = Budget::default();
    let tools = tools(
        &fixture,
        "[ \"$1\" = --disable ]\nprintf archive-bytes\nprintf 'OPS_HTTP_STATUS:200:200\\n' >&2",
        &budget,
    );
    let plan = plan::Plan::new(Arc::new(plan::NativeClock));
    let end = plan.begin(1).err();
    assert!(end.is_some());
    let end = plan.now().unwrap() + Duration::from_secs(90);
    let env = Environment::new();
    let mut native = Native::new(&fixture.root, &env, Signals::new().unwrap());
    let tree = Tree::create(&fixture.root.join("download"), &budget).unwrap();
    let (tree, name) = fetch::run(tree, 0, download(), &tools, &mut native, &plan, end)
        .await
        .unwrap();
    assert_eq!(
        tree.read(&name, 32, &mut || Ok(())).unwrap(),
        b"archive-bytes"
    );
    assert_eq!(fs::read(fixture.root.join("did-run")).unwrap(), b"A");
    assert!(native.cleanup_passed());
    tree.finish().unwrap();
    tools.finish().unwrap();
    closed(&budget);
}

#[tokio::test]
async fn actual_nonzero_overflow_and_metadata_failure_cannot_authorize_material_bytes() {
    for (body, maximum, expected) in [
        (
            "printf archive-bytes; printf 'OPS_HTTP_STATUS:200:200\\n' >&2; exit 42",
            32,
            42,
        ),
        (
            "trap '' PIPE; while :; do printf 12345678901234567890; done",
            8,
            1,
        ),
        (
            "printf archive-bytes; printf 'PRIVATE-server-diagnostic\\n' >&2",
            32,
            1,
        ),
        (
            "printf wrong-bytes; printf 'OPS_HTTP_STATUS:200:200\\n' >&2",
            32,
            1,
        ),
    ] {
        let fixture = fixture::Fixture::new();
        let budget = Budget::default();
        let tools = tools(&fixture, body, &budget);
        let plan = plan::Plan::new(Arc::new(plan::NativeClock));
        let end = plan.now().unwrap() + Duration::from_secs(90);
        let env = Environment::new();
        let mut native = Native::new(&fixture.root, &env, Signals::new().unwrap());
        let tree = Tree::create(&fixture.root.join("download"), &budget).unwrap();
        let mut selected = download();
        selected.maximum = maximum;
        fixture::expect_error(
            fetch::run(tree, 0, selected, &tools, &mut native, &plan, end).await,
            expected,
        );
        assert!(native.cleanup_passed());
        tools.finish().unwrap();
        closed(&budget);
    }
}

#[tokio::test]
async fn actual_failed_http_child_seals_are_admitted_for_complete_tree_settlement() {
    let fixture = fixture::Fixture::new();
    let budget = Budget::default();
    let tools = tools(
        &fixture,
        "printf partial; printf 'PRIVATE-http-error\\n' >&2; exit 22",
        &budget,
    );
    let clock = Arc::new(plan::NativeClock);
    let mut tree = Tree::create(&fixture.root.join("download"), &budget).unwrap();
    let proof = tools
        .invocation(InvocationSpec {
            role: Role::Curl,
            args: vec![],
            directory: tree.run.path.clone(),
            environment: Environment::new(),
            body: Duration::from_secs(5),
            stdout_maximum: 32,
            input: None,
            clock: clock.clone(),
            latest: plan::Clock::now(&*clock) + Duration::from_secs(10),
        })
        .unwrap();
    let (logs, names, permits) = tree.capture(7, 32).await.unwrap();
    let mut command = Command::capture(Role::Curl.program(), &[]);
    command.capture = None;
    command.timeout = Duration::from_secs(5);
    command.grace = Duration::from_secs(5);
    command.separate_logs = Some(logs);
    command.hosted_ops = Some(proof.clone());
    let env = Environment::new();
    let mut native = Native::new(&fixture.root, &env, Signals::new().unwrap());
    let error = native.run(command).await.unwrap_err();
    assert_eq!(error.code, 22);
    assert!(error.is_child_exit());
    drop(permits);
    let seals = proof.captures().unwrap();
    drop(proof);
    fetch::admit(
        &mut tree,
        &names,
        &seals,
        &download(),
        false,
        &mut || Ok(()),
    )
    .unwrap();
    assert_eq!(tree.records.len(), 2);
    assert_eq!(
        tree.records[&names[0]].sha256,
        super::super::sha(b"partial")
    );
    assert_eq!(
        tree.records[&names[1]].sha256,
        super::super::sha(b"PRIVATE-http-error\n")
    );
    assert_ne!(tree.records[&names[0]].sha256, download().sha256);
    assert!(native.cleanup_passed());
    tree.finish().unwrap();
    tools.finish().unwrap();
    closed(&budget);
}

/// Run the actual reserved group first, then inject only a post-body ownership
/// fault. No Log or executable Arc survives beyond this adapter invocation.
struct AfterCurl<'a> {
    native: Native<'a>,
    fault: &'static str,
    clock: Arc<fixture::Clock>,
}
impl Executor for AfterCurl<'_> {
    async fn run(&mut self, command: Command) -> Result<Vec<u8>> {
        let proof = command.hosted_ops.as_ref().unwrap().clone();
        let program = command.program;
        let arguments = command.arguments.clone();
        let ran = self.native.run(command).await;
        let directory = proof.context(program, &arguments)?.0.to_owned();
        match self.fault {
            "missing-seal" => proof.observed_io(None),
            "changed-seal" => {
                fs::write(directory.join("capture-0.stdout"), b"changed-bytes").unwrap()
            }
            "deadline" => self.clock.advance(Duration::from_secs(90)),
            _ => {}
        }
        drop(proof);
        if self.fault == "body-term" {
            Err(Failure::new(143, "PRIVATE original terminal cause"))
        } else {
            ran
        }
    }
    async fn ready(&mut self) -> Result<()> {
        self.native.ready().await?;
        if self.fault == "late-term" {
            Err(Failure::new(143, "PRIVATE later terminal cause"))
        } else {
            Ok(())
        }
    }
    fn cleanup_passed(&self) -> bool {
        self.native.cleanup_passed()
    }
}

#[tokio::test]
async fn actual_fetch_keeps_original_child_class_and_status_over_post_body_faults() {
    for (fault, exit, expected, child_exit) in [
        ("none", 22, 22, true),
        ("missing-seal", 22, 22, true),
        ("changed-seal", 22, 22, true),
        ("deadline", 22, 22, true),
        ("late-term", 22, 22, true),
        ("body-term", 0, 143, false),
        ("missing-seal", 0, 1, false),
        ("changed-seal", 0, 1, false),
    ] {
        let fixture = fixture::Fixture::new();
        let budget = Budget::default();
        let body =
            format!("printf archive-bytes; printf 'OPS_HTTP_STATUS:200:200\\n' >&2; exit {exit}");
        let tools = tools(&fixture, &body, &budget);
        let clock = Arc::new(fixture::Clock::new());
        let plan = plan::Plan::new(clock.clone());
        let end = plan.now().unwrap() + Duration::from_secs(90);
        let env = Environment::new();
        let mut executor = AfterCurl {
            native: Native::new(&fixture.root, &env, Signals::new().unwrap()),
            fault,
            clock,
        };
        let tree = Tree::create(&fixture.root.join("download"), &budget).unwrap();
        let result = fetch::run(tree, 0, download(), &tools, &mut executor, &plan, end).await;
        let error = match result {
            Err(error) => error,
            Ok((tree, _)) => {
                tree.finish().unwrap();
                panic!("post-body fault must remain a refusal");
            }
        };
        assert_eq!(error.code, expected, "{fault}");
        assert_eq!(error.is_child_exit(), child_exit, "{fault}");
        assert!(executor.cleanup_passed());
        assert!(fixture.root.join("download/capture-0.stdout").is_file());
        assert!(fixture.root.join("download/capture-0.stderr").is_file());
        tools.finish().unwrap();
        closed(&budget);
    }
}

#[tokio::test]
async fn actual_dpkg_reads_original_retained_stdin_and_returns_separate_binary_stdout() {
    let fixture = fixture::Fixture::new();
    let budget = Budget::default();
    let tools = tools(
        &fixture,
        "[ \"$#\" = 2 ]; [ \"$1\" = --fsys-tarfile ]; [ \"$2\" = - ]; /bin/cat",
        &budget,
    );
    let plan = plan::Plan::new(Arc::new(plan::NativeClock));
    let end = plan.now().unwrap() + Duration::from_secs(90);
    let env = Environment::new();
    let mut native = Native::new(&fixture.root, &env, Signals::new().unwrap());
    let mut tree = Tree::create(&fixture.root.join("download"), &budget).unwrap();
    let bytes = b"inert\0binary\xfftar";
    tree.write("original.deb", bytes, false).unwrap();
    let (tree, name) = fetch::decoded_deb(tree, 0, "original.deb", &tools, &mut native, &plan, end)
        .await
        .unwrap();
    assert_eq!(tree.read(&name, 32, &mut || Ok(())).unwrap(), bytes);
    assert!(native.cleanup_passed());
    tree.finish().unwrap();
    tools.finish().unwrap();
    closed(&budget);
}

#[tokio::test]
async fn capsule_freezes_role_argv_body_caps_clock_and_retained_input_cursor() {
    for case in ["role", "args", "body", "cap", "late", "cursor", "stamp"] {
        let fixture = fixture::Fixture::new();
        let budget = Budget::default();
        let tools = tools(&fixture, "exit 0", &budget);
        let clock = Arc::new(fixture::Clock::new());
        let plan = plan::Plan::new(clock.clone());
        let mut tree = Tree::create(&fixture.root.join("download"), &budget).unwrap();
        tree.write("input", b"abc", false).unwrap();
        let with_input = matches!(case, "cursor" | "stamp");
        let role = if with_input {
            Role::DpkgDeb
        } else {
            Role::Curl
        };
        let expected = vec!["fixed".into()];
        let body = Duration::from_secs(5);
        let input_stamp = with_input.then(|| tree.records["input"].stamp.clone());
        let proof = tools
            .invocation(InvocationSpec {
                role,
                args: expected.clone(),
                directory: tree.run.path.clone(),
                environment: Environment::new(),
                body,
                stdout_maximum: 16,
                input: input_stamp,
                clock: clock.clone(),
                latest: plan.now().unwrap() + Duration::from_secs(10),
            })
            .unwrap();
        let (logs, _, permits) = tree
            .capture(0, if case == "cap" { 17 } else { 16 })
            .await
            .unwrap();
        let pipes = if case == "stamp" {
            logs.stdout
                .pipe_fds()
                .into_iter()
                .chain(logs.stderr.pipe_fds())
                .map(|fd| {
                    let path = std::path::PathBuf::from(format!("/dev/fd/{fd}"));
                    let stamp = fs::metadata(&path).unwrap();
                    (path, stamp)
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let mut command = Command::capture(role.program(), &[]);
        command.capture = None;
        command.arguments = expected;
        command.timeout = body;
        command.grace = Duration::from_secs(5);
        command.separate_logs = Some(logs);
        command.hosted_ops = Some(proof.clone());
        let input_permit = if with_input {
            let (input, permit) = tree.input("input").unwrap();
            if case == "cursor" {
                (&*input).seek(SeekFrom::Start(1)).unwrap();
            }
            command.stdin = Some(input);
            Some(permit)
        } else {
            None
        };
        match case {
            "role" => command.program = Role::DpkgDeb.program(),
            "args" => command.arguments.push("PRIVATE".into()),
            "body" => command.timeout += Duration::from_secs(1),
            "late" => clock.advance(Duration::from_secs(10)),
            "stamp" => {
                let original = &tree.records["input"].stamp;
                let file = fs::OpenOptions::new()
                    .write(true)
                    .open(tree.run.path.join("input"))
                    .unwrap();
                let changed = (|| {
                    (&file).write_all(b"xyz")?;
                    file.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1))?;
                    file.metadata()
                })();
                let settled = nix::unistd::close(file);
                let changed = changed.unwrap();
                settled.unwrap();
                assert_eq!(changed.ino(), original.ino());
                assert_eq!(changed.dev(), original.dev());
                assert_eq!(changed.len(), original.len());
                assert!(!super::super::namespace::same(original, &changed));
            }
            _ => {}
        }
        let env = Environment::new();
        let mut native = Native::new(&fixture.root, &env, Signals::new().unwrap());
        fixture::expect_error(
            native.run(command).await,
            if case == "late" { 124 } else { 1 },
        );
        drop(permits);
        drop(input_permit);
        assert!(native.cleanup_passed());
        assert!(!fixture.root.join("did-run").exists());
        assert!(proof.captures().is_err());
        if case == "stamp" {
            assert_eq!(pipes.len(), 4);
            for (path, original) in pipes {
                assert!(!fs::metadata(path).is_ok_and(|observed| {
                    super::super::namespace::binding(&original, &observed)
                }));
            }
            assert_eq!(fs::read(tree.run.path.join("input")).unwrap(), b"xyz");
        }
        drop(proof);
        let _ = tree.finish();
        tools.finish().unwrap();
        closed(&budget);
    }
}

#[tokio::test]
async fn original_finished_seal_refuses_same_length_mutation_before_first_read() {
    let fixture = fixture::Fixture::new();
    let budget = Budget::default();
    let tools = tools(&fixture, "printf original", &budget);
    let clock = Arc::new(fixture::Clock::new());
    let plan = plan::Plan::new(clock.clone());
    let mut tree = Tree::create(&fixture.root.join("download"), &budget).unwrap();
    let proof = tools
        .invocation(InvocationSpec {
            role: Role::Curl,
            args: vec![],
            directory: tree.run.path.clone(),
            environment: Environment::new(),
            body: Duration::from_secs(5),
            stdout_maximum: 16,
            input: None,
            clock,
            latest: plan.now().unwrap() + Duration::from_secs(10),
        })
        .unwrap();
    let (logs, names, permits) = tree.capture(0, 16).await.unwrap();
    let mut command = Command::capture(Role::Curl.program(), &[]);
    command.capture = None;
    command.timeout = Duration::from_secs(5);
    command.grace = Duration::from_secs(5);
    command.separate_logs = Some(logs);
    command.hosted_ops = Some(proof.clone());
    let env = Environment::new();
    let mut native = Native::new(&fixture.root, &env, Signals::new().unwrap());
    native.run(command).await.unwrap();
    drop(permits);
    let seals = proof.captures().unwrap();
    fs::write(tree.run.path.join(&names[0]), b"tampered").unwrap();
    fixture::expect_error(tree.captured(&names, &seals, &mut || Ok(())), 1);
    drop(proof);
    let _ = tree.finish();
    tools.finish().unwrap();
    closed(&budget);
}

#[test]
fn generated_cc_exec_uses_its_relocated_prefix_and_preserves_argument_bytes() {
    let fixture = fixture::Fixture::new();
    let original = fixture.root.join("before/host-tools");
    fs::create_dir_all(original.join("bin")).unwrap();
    fs::create_dir_all(original.join("sysroot/usr/bin")).unwrap();
    fs::write(original.join("bin/cc"), super::shim::CC).unwrap();
    let gcc = original.join("sysroot/usr/bin/x86_64-linux-gnu-gcc-13");
    fs::write(
        &gcc,
        b"#!/bin/sh\n[ \"$#\" = 6 ] || exit 42\nprintf '%s\\000' \"$@\"\n",
    )
    .unwrap();
    for path in [original.join("bin/cc"), gcc] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::rename(
        fixture.root.join("before"),
        fixture.root.join("after space"),
    )
    .unwrap();
    let moved = fixture.root.join("after space/host-tools");
    let output = std::process::Command::new(moved.join("bin/cc"))
        .env_clear()
        .args(["a space $()", "a'quote;*"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let args = output
        .stdout
        .split(|b| *b == 0)
        .filter(|b| !b.is_empty())
        .map(|b| std::str::from_utf8(b).unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(&args[4..], ["a space $()", "a'quote;*"]);
    assert_eq!(args[0], format!("--sysroot={}/sysroot", moved.display()));
    assert!(
        args[..4]
            .iter()
            .all(|arg| arg.contains(&moved.display().to_string()))
    );
}
