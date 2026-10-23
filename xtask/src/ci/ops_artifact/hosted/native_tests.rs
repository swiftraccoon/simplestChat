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
    tooling::Environment,
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
