//! Actual reserved Native groups and original Finished seals. The selected gh
//! is an owned inert shell fixture: no GitHub, credential helper or verifier runs.

use super::{files::Output, hash::Checkpoint, request::Request, tools};
use crate::{
    ci::{
        command::{Command, Program},
        process::{Executor, Native},
    },
    owned_process::Signals,
    tooling::{Environment, Result},
};
use serde_json::json;
use std::{
    ffi::OsString,
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use super::super::namespace::Budget;

static NEXT: AtomicUsize = AtomicUsize::new(0);
const JSON: &[u8] = b"{\"value\":1}\n";

struct Fixture {
    root: PathBuf,
    environment: Environment,
    request: Request,
    budget: Budget,
}
impl Fixture {
    fn new(body: &str) -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "ops-verifier-native-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        fs::create_dir(root.join("tools")).unwrap();
        fs::create_dir(root.join("retained")).unwrap();
        let executable = root.join("tools/gh");
        fs::write(
            &executable,
            format!(
                concat!(
                    "#!/bin/sh\nset -eu\n",
                    "[ \"$GH_TOKEN\" = inert-owned-token ]\n",
                    "[ \"$HOME\" = \"$PWD/work/home\" ]\n",
                    "[ \"$GH_CONFIG_DIR\" = \"$PWD/work/gh-config\" ]\n",
                    "[ \"$GH_TELEMETRY\" = disabled ]\n",
                    "[ \"$PATH\" = /usr/bin:/bin ]\n",
                    "[ -z \"${{GH_HOST+x}}\" ]\n",
                    "[ -z \"${{HTTPS_PROXY+x}}\" ]\n",
                    "printf A >> ../did-run\n",
                    "{}\n"
                ),
                body
            ),
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let encoded = serde_json::to_vec(&json!({"schemaVersion":1,"selection":{
            "repository":"swiftraccoon/simplestChat","revision":"a".repeat(40),
            "packageVersion":"0.1.0","protocolVersion":1,
            "target":"x86_64-unknown-linux-musl","buildRunId":123,"runAttempt":2},
            "directory":root.join("retained"),"output":root.join("result"),
            "authentication":"gh-token"}))
        .unwrap();
        let request = Request::decode(&encoded, &super::super::sha(&encoded)).unwrap();
        let environment = Environment::from([
            ("PATH".into(), root.join("tools").into_os_string()),
            ("GH_TOKEN".into(), "inert-owned-token".into()),
            ("GH_HOST".into(), "PRIVATE-wrong-host.invalid".into()),
            ("HTTPS_PROXY".into(), "PRIVATE-wrong-proxy.invalid".into()),
            ("GH_TELEMETRY".into(), "enabled".into()),
        ]);
        Self {
            root,
            environment,
            request,
            budget: Budget::default(),
        }
    }
    fn owners(&self) -> (Output, tools::Tools) {
        let output = Output::new(&self.request.output, &self.budget).unwrap();
        let tools =
            match tools::Tools::open(&self.root, &self.environment, &self.request, &self.budget) {
                Ok(tools) => tools,
                Err(error) => {
                    let closed = output.finish();
                    panic!("inert tool selection failed: {error}; closed={closed:?}");
                }
            };
        (output, tools)
    }
    fn poison_ambient_selection(&mut self) {
        self.environment
            .insert("PATH".into(), "/not-the-selected-gh".into());
        self.environment
            .insert("GH_TOKEN".into(), "PRIVATE-late-token".into());
    }
    fn runs(&self) -> Vec<u8> {
        fs::read(self.root.join("did-run")).unwrap_or_default()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

struct Dispatched {
    result: Result<Vec<u8>>,
    captures: Result<tools::Captures>,
    names: [String; 2],
}
fn invocation(tools: &tools::Tools, index: usize) -> Arc<tools::Invocation> {
    let proof = tools.invocation(index).unwrap();
    proof
        .schedule(&super::plan::Context::new(Arc::new(
            super::plan::NativeClock,
        )))
        .unwrap();
    proof
}
async fn dispatch(
    output: &Output,
    native: &mut Native<'_>,
    proof: &Arc<tools::Invocation>,
    arguments: Vec<OsString>,
    index: usize,
    maximum: u64,
    program: Program,
) -> Dispatched {
    let (logs, names, permits) = output.capture(index, maximum).await.unwrap();
    let mut command = Command::capture(program, &[]);
    command.arguments = arguments;
    command.timeout = Duration::from_secs(2);
    command.grace = Duration::from_millis(20);
    command.capture = None;
    command.separate_logs = Some(logs);
    command.retained_ops = Some(proof.clone());
    let result = native.run(command).await;
    let captures = proof.captures();
    // Native returned only after attempting both sink owners. Their retained
    // namespace/readback descriptors are separately budgeted below.
    drop(permits);
    Dispatched {
        result,
        captures,
        names,
    }
}
fn finish(output: Output, tools: tools::Tools) {
    let tool_close = tools.finish();
    let output_close = output.finish();
    assert!(tool_close.is_ok(), "selected tool close: {tool_close:?}");
    assert!(output_close.is_ok(), "output close: {output_close:?}");
}

#[tokio::test]
async fn actual_selected_gh_context_captures_version_and_json_after_original_stream_closes() {
    let mut fixture = Fixture::new(concat!(
        "printf PRIVATE-STDERR >&2\n",
        "case \"$1\" in\n",
        "  --version) printf 'gh version 2.102.0 (inert)\\n' ;;\n",
        "  api) printf '{\"value\":1}\\n' ;;\n",
        "  *) exit 89 ;;\n",
        "esac"
    ));
    let (mut output, tools) = fixture.owners();
    fixture.poison_ambient_selection();
    let mut native = Native::new(&fixture.root, &fixture.environment, Signals::new().unwrap());
    let mut observations = Vec::new();
    for (index, expected) in [(0, b"gh version 2.102.0 (inert)\n".as_slice()), (1, JSON)] {
        let proof = invocation(&tools, index);
        let call = dispatch(
            &output,
            &mut native,
            &proof,
            tools.arguments(index).unwrap(),
            index,
            4096,
            Program::OpsGh,
        )
        .await;
        let data = call
            .captures
            .as_ref()
            .map_err(|_| super::failure(1))
            .and_then(|seal| output.capture_read(&call.names, seal, &Checkpoint::fixture()));
        let stamp = call
            .captures
            .as_ref()
            .map(|seal| {
                (
                    seal.stdout.sha256.clone(),
                    seal.stdout.bytes,
                    seal.stderr.bytes,
                )
            })
            .map_err(|_| super::failure(1));
        drop(proof);
        observations.push((call.result, data, stamp, expected.to_vec()));
    }
    let clean = native.cleanup_passed();
    drop(native);
    finish(output, tools);
    for (result, data, stamp, expected) in observations {
        assert!(result.is_ok());
        assert_eq!(data.unwrap(), expected);
        assert_eq!(
            stamp.unwrap(),
            (super::super::sha(&expected), expected.len() as u64, 14)
        );
    }
    assert!(clean);
    assert_eq!(fixture.runs(), b"AA");
    assert_eq!(fixture.budget.count(), 0);
}

#[tokio::test]
async fn real_nonzero_gh_status_wins_even_with_valid_json_and_closed_original_seals() {
    let fixture = Fixture::new("printf '{\"value\":1}\\n'\nexit 42");
    let (mut output, tools) = fixture.owners();
    let proof = invocation(&tools, 1);
    let mut native = Native::new(&fixture.root, &fixture.environment, Signals::new().unwrap());
    let call = dispatch(
        &output,
        &mut native,
        &proof,
        tools.arguments(1).unwrap(),
        1,
        4096,
        Program::OpsGh,
    )
    .await;
    let data = call
        .captures
        .as_ref()
        .map_err(|_| super::failure(1))
        .and_then(|seal| output.capture_read(&call.names, seal, &Checkpoint::fixture()));
    let clean = native.cleanup_passed();
    drop(native);
    drop(proof);
    finish(output, tools);
    assert_eq!(call.result.unwrap_err().code, 42);
    assert_eq!(data.unwrap(), JSON);
    assert!(clean);
    assert_eq!(fixture.runs(), b"A");
    assert_eq!(fixture.budget.count(), 0);
}

#[tokio::test]
async fn either_actual_stream_overflow_cannot_produce_a_positive_original_capture() {
    for stderr in [false, true] {
        // Ignore SIGPIPE so the shell's failed printf observes EPIPE and set -e
        // selects exit 1, even if the sink closes before Native samples failure.
        let fixture = Fixture::new(&format!(
            concat!(
                "trap '' PIPE\n",
                "i=0\nwhile [ \"$i\" -lt 5000 ]; do\n",
                "  printf '0123456789abcdef\\n' {}\n  i=$((i+1))\ndone"
            ),
            if stderr { ">&2" } else { "" }
        ));
        let (output, tools) = fixture.owners();
        let proof = invocation(&tools, 0);
        let mut native = Native::new(&fixture.root, &fixture.environment, Signals::new().unwrap());
        let call = dispatch(
            &output,
            &mut native,
            &proof,
            tools.arguments(0).unwrap(),
            0,
            32,
            Program::OpsGh,
        )
        .await;
        let clean = native.cleanup_passed();
        drop(native);
        drop(proof);
        finish(output, tools);
        assert_eq!(call.result.unwrap_err().code, 1);
        assert!(call.captures.is_err());
        assert!(clean);
        assert_eq!(fixture.runs(), b"A");
        assert_eq!(fixture.budget.count(), 0);
    }
}

#[tokio::test]
async fn wrong_program_and_changed_arguments_refuse_before_dispatch_but_finish_peers() {
    for program in [Program::Git, Program::OpsGh] {
        let fixture = Fixture::new("printf never-dispatched");
        let (output, tools) = fixture.owners();
        let proof = invocation(&tools, 0);
        let mut args = tools.arguments(0).unwrap();
        if program == Program::OpsGh {
            args.push("PRIVATE-extra-argument".into());
        }
        let mut native = Native::new(&fixture.root, &fixture.environment, Signals::new().unwrap());
        let call = dispatch(&output, &mut native, &proof, args, 0, 4096, program).await;
        let clean = native.cleanup_passed();
        drop(native);
        drop(proof);
        finish(output, tools);
        assert_eq!(call.result.unwrap_err().code, 1);
        // Empty seals prove both unique log finalizers ran on rejection; they
        // do not imply the body ran or verification succeeded.
        let capture = call.captures.unwrap();
        assert_eq!((capture.stdout.bytes, capture.stderr.bytes), (0, 0));
        assert!(clean);
        assert!(fixture.runs().is_empty());
        assert_eq!(fixture.budget.count(), 0);
    }
}

#[tokio::test]
async fn invocation_reuse_refuses_a_second_dispatch_and_invalidates_old_observations() {
    let fixture = Fixture::new("printf '{\"value\":1}\\n'");
    let (mut output, tools) = fixture.owners();
    let proof = invocation(&tools, 0);
    let mut native = Native::new(&fixture.root, &fixture.environment, Signals::new().unwrap());
    let first = dispatch(
        &output,
        &mut native,
        &proof,
        tools.arguments(0).unwrap(),
        0,
        4096,
        Program::OpsGh,
    )
    .await;
    let first_data = first
        .captures
        .as_ref()
        .map_err(|_| super::failure(1))
        .and_then(|seal| output.capture_read(&first.names, seal, &Checkpoint::fixture()));
    let second = dispatch(
        &output,
        &mut native,
        &proof,
        tools.arguments(0).unwrap(),
        1,
        4096,
        Program::OpsGh,
    )
    .await;
    let observed = proof.captures();
    let clean = native.cleanup_passed();
    drop(native);
    drop(proof);
    finish(output, tools);
    assert!(first.result.is_ok());
    assert_eq!(first_data.unwrap(), JSON);
    assert_eq!(second.result.unwrap_err().code, 1);
    assert!(second.captures.is_err());
    assert!(observed.is_err());
    assert!(clean);
    assert_eq!(fixture.runs(), b"A");
    assert_eq!(fixture.budget.count(), 0);
}

#[tokio::test]
async fn same_length_change_after_finished_before_first_read_refuses_the_new_bytes() {
    let fixture = Fixture::new("printf '{\"value\":1}\\n'");
    let (mut output, tools) = fixture.owners();
    let proof = invocation(&tools, 1);
    let mut native = Native::new(&fixture.root, &fixture.environment, Signals::new().unwrap());
    let call = dispatch(
        &output,
        &mut native,
        &proof,
        tools.arguments(1).unwrap(),
        1,
        4096,
        Program::OpsGh,
    )
    .await;
    let replacement = b"{\"value\":2}\n";
    assert_eq!(replacement.len(), JSON.len());
    let changed = fs::write(output.path(&call.names[0]), replacement);
    let data = call
        .captures
        .as_ref()
        .map_err(|_| super::failure(1))
        .and_then(|seal| output.capture_read(&call.names, seal, &Checkpoint::fixture()));
    let clean = native.cleanup_passed();
    drop(native);
    drop(proof);
    finish(output, tools);
    assert!(changed.is_ok());
    assert!(call.result.is_ok());
    assert!(data.is_err());
    assert!(clean);
    assert_eq!(fixture.runs(), b"A");
    assert_eq!(fixture.budget.count(), 0);
}

#[tokio::test]
async fn unarmed_or_expired_after_capture_dispatch_guards_finish_both_logs_without_spawn() {
    for expired in [false, true] {
        let fixture = Fixture::new("printf never-dispatched");
        let (output, tools) = fixture.owners();
        let proof = tools.invocation(0).unwrap();
        let clock = Arc::new(super::fixture::Clock::new());
        let context = super::plan::Context::new(clock.clone());
        if expired {
            proof.schedule(&context).unwrap();
        }
        let (logs, _, permits) = output.capture(0, 4096).await.unwrap();
        if expired {
            clock.advance(Duration::from_secs(71));
        }
        let mut command = Command::capture(Program::OpsGh, &[]);
        command.arguments = tools.arguments(0).unwrap();
        command.capture = None;
        command.separate_logs = Some(logs);
        command.retained_ops = Some(proof.clone());
        let mut native = Native::new(&fixture.root, &fixture.environment, Signals::new().unwrap());
        let result = native.run(command).await;
        let captured = proof.captures();
        let clean = native.cleanup_passed();
        drop(permits);
        drop(native);
        drop(proof);
        finish(output, tools);
        assert_eq!(result.unwrap_err().code, if expired { 124 } else { 1 });
        let captured = captured.unwrap();
        assert_eq!((captured.stdout.bytes, captured.stderr.bytes), (0, 0));
        assert!(clean);
        assert!(fixture.runs().is_empty());
        assert_eq!(fixture.budget.count(), 0);
    }
}
