//! Actual reserved groups and original capture seals using only owned inert gh.

use super::{
    fixture::{Fixture, bytes},
    *,
};
use crate::{
    ci::{
        command::{Command, Program},
        process::{Executor, Native},
    },
    owned_process::Signals,
    tooling::Environment,
};
use std::{ffi::OsString, fs, os::unix::fs::PermissionsExt, sync::Arc};

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn install(fixture: &Fixture, body: &str) {
    let marker = quote(fixture.root.join("did-run").to_str().unwrap());
    let script = format!(
        concat!(
            "#!/bin/sh\nset -eu\n",
            "[ \"$GH_TOKEN\" = PRIVATE-TOKEN ]\n",
            "[ \"$GH_TELEMETRY\" = disabled ]\n",
            "[ \"$PATH\" = /usr/bin:/bin ]\n",
            "[ -z \"${{GITHUB_TOKEN+x}}\" ]\n",
            "[ -z \"${{GH_DEBUG+x}}\" ]\n",
            "[ -z \"${{HTTPS_PROXY+x}}\" ]\n",
            "printf x >> {}\n{}\n"
        ),
        marker, body
    );
    fs::write(fixture.root.join("gh"), script).unwrap();
    fs::set_permissions(fixture.root.join("gh"), fs::Permissions::from_mode(0o700)).unwrap();
}
fn request(fixture: &Fixture) -> request::Request {
    request::Request::decode(&bytes(&fixture.wire), &fixture.pin()).unwrap()
}
fn context() -> plan::Context {
    plan::Context::new(Arc::new(plan::NativeClock))
}

#[tokio::test]
async fn complete_actual_native_fake_gh_runs_all_nine_closed_calls_and_one_final_replay() {
    let fixture = Fixture::new(1);
    for (name, index) in [
        ("repo", 1),
        ("attempt", 2),
        ("artifact", 3),
        ("zip", 4),
        ("verified", 8),
    ] {
        fs::write(fixture.root.join(name), &fixture.outputs[index]).unwrap();
    }
    let cat = |name: &str| {
        format!(
            "/bin/cat {}",
            quote(fixture.root.join(name).to_str().unwrap())
        )
    };
    install(
        &fixture,
        &format!(
            concat!(
                "case \"$1\" in\n",
                "--version) printf 'gh version 2.102.0 (inert)\\n' ;;\n",
                "attestation) {} ;;\n",
                "api) for last do :; done\n",
                " case \"$last\" in\n",
                " repos/swiftraccoon/simplestChat) {} ;;\n",
                " */actions/runs/123/attempts/2) {} ;;\n",
                " */actions/artifacts/456) {} ;;\n",
                " */actions/artifacts/456/zip) {} ;;\n",
                " *) exit 93 ;;\n esac ;;\n",
                "*) exit 94 ;;\nesac"
            ),
            cat("verified"),
            cat("repo"),
            cat("attempt"),
            cat("artifact"),
            cat("zip")
        ),
    );
    // Forward every command to actual Native while retaining the final public
    // replay in this test. No Node or GitHub verifier is executed.
    struct Replay<'a> {
        native: Native<'a>,
        rows: Vec<Vec<u8>>,
    }
    impl Executor for Replay<'_> {
        async fn run(&mut self, command: Command) -> Result<Vec<u8>> {
            self.native.run(command).await
        }
        async fn ready(&mut self) -> Result<()> {
            self.native.ready().await
        }
        fn cleanup_passed(&self) -> bool {
            self.native.cleanup_passed()
        }
        async fn replay(&mut self, row: &[u8], _: bool, _: std::time::Instant) -> Result<()> {
            self.rows.push(row.into());
            Ok(())
        }
    }
    let replay_environment = Environment::new();
    let native = Native::new(&fixture.root, &replay_environment, Signals::new().unwrap());
    let mut executor = Replay {
        native,
        rows: Vec::new(),
    };
    let budget = super::super::namespace::Budget::default();
    let result = execute::transaction(
        fixture.request.clone(),
        fixture.pin(),
        fixture.root.clone(),
        fixture.environment.clone(),
        budget.clone(),
        &context(),
        &mut executor,
    )
    .await;
    let clean = executor.cleanup_passed();
    let rows = std::mem::take(&mut executor.rows);
    drop(executor);
    assert!(result.is_ok(), "actual inert transaction: {result:?}");
    assert!(clean);
    assert_eq!(
        fs::read(fixture.root.join("did-run")).unwrap(),
        b"xxxxxxxxx"
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(budget.count(), 0);
}

async fn one(
    fixture: &Fixture,
    index: usize,
    changed_args: bool,
    armed: bool,
    wrong_cap: bool,
    mutate: bool,
) -> (Result<Vec<u8>>, Result<Vec<u8>>, bool) {
    let budget = super::super::namespace::Budget::default();
    let mut output = files::Output::new(&fixture.output, &budget).unwrap();
    let tools = tools::Tools::open(
        &fixture.root,
        &fixture.environment,
        &request(fixture),
        &budget,
    )
    .unwrap();
    let proof = tools.invocation(index).unwrap();
    let maximum = if wrong_cap { 4095 } else { 4096 };
    let mut pending = output.capture(index, maximum).await.unwrap();
    let mut command = Command::capture(Program::OpsGh, &[]);
    command.arguments = tools.arguments(index).unwrap();
    if changed_args {
        command.arguments.push(OsString::from("--unreviewed"));
    }
    command.capture = None;
    command.timeout = plan::CAPS[index];
    command.grace = plan::GRACE;
    command.separate_logs = pending.logs.take();
    command.ops_acquire = Some(proof.clone());
    if armed {
        proof.schedule(&context()).unwrap();
    }
    let mut native = Native::new(&fixture.root, &fixture.environment, Signals::new().unwrap());
    let result = native.run(command).await;
    let captures = proof.captures();
    let clean = native.cleanup_passed();
    if mutate {
        let path = fixture.output.join("gh-0.stdout");
        let mut bytes = fs::read(&path).unwrap();
        bytes[0] ^= 1;
        fs::write(path, bytes).unwrap();
    }
    let read = captures
        .and_then(|seals| output.capture_read(pending, &seals, &hash::Checkpoint::fixture(), true));
    drop(proof);
    drop(native);
    let tools_closed = tools.finish();
    let output_closed = output.finish();
    assert!(tools_closed.is_ok());
    assert!(output_closed.is_ok());
    assert_eq!(budget.count(), 0);
    (result, read, clean)
}

#[tokio::test]
async fn actual_nonzero_cli_with_valid_output_remains_primary_and_all_peers_retire() {
    let fixture = Fixture::new(0);
    install(&fixture, "printf '{\"value\":1}\\n'\nexit 42");
    let (result, read, clean) = one(&fixture, 0, false, true, false, false).await;
    assert_eq!(result.unwrap_err().code, 42);
    assert_eq!(read.unwrap(), b"{\"value\":1}\n");
    assert!(clean);
    assert_eq!(fs::read(fixture.root.join("did-run")).unwrap(), b"x");
}

#[tokio::test]
async fn actual_finished_same_length_mutation_refuses_the_first_capture_read() {
    let fixture = Fixture::new(0);
    install(&fixture, "printf '{\"value\":1}\\n'");
    let (result, read, clean) = one(&fixture, 0, false, true, false, true).await;
    assert!(result.is_ok());
    assert_eq!(read.unwrap_err().code, 1);
    assert!(clean);
}

#[tokio::test]
async fn actual_unarmed_changed_argv_and_wrong_log_cap_refuse_before_any_execution() {
    for (changed, armed, cap) in [
        (false, false, false),
        (true, true, false),
        (false, true, true),
    ] {
        let fixture = Fixture::new(0);
        install(&fixture, "printf unexpected");
        let (result, _, clean) = one(&fixture, 0, changed, armed, cap, false).await;
        assert_eq!(result.unwrap_err().code, 1);
        assert!(clean);
        assert!(!fixture.root.join("did-run").exists());
    }
}

#[tokio::test]
async fn actual_bounded_pipe_overflow_cannot_produce_an_admitted_capture_seal() {
    let fixture = Fixture::new(0);
    install(
        &fixture,
        concat!(
            "trap '' PIPE\n",
            "while :; do printf '",
            "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
            "'; done"
        ),
    );
    let (result, read, clean) = one(&fixture, 0, false, true, false, false).await;
    assert_eq!(result.unwrap_err().code, 1);
    assert!(read.is_err());
    assert!(clean);
}
