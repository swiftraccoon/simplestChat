//! Restore-only fixed roles with both original live evidence sinks retained.
use super::super::{
    control::{Cause, Owner, Window},
    stage::process::{self, Io, Sink},
};
use super::{CommandOutput, Executor, Invocation};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
pub(super) struct Native;
impl Executor for Native {
    async fn run(&mut self, owner: &mut Owner, request: Invocation<'_>) -> CommandOutput {
        let before = owner.error();
        let cleanup = request.role.cleanup();
        let inherited = request.input.is_some();
        let end = request.settlement_end;
        let completed = process::run_io(
            owner,
            process::Invocation {
                program: request.program,
                arguments: request.arguments,
                environment: BTreeMap::from([
                    ("PATH".into(), "/usr/sbin:/usr/bin:/sbin:/bin".into()),
                    ("LC_ALL".into(), "C".into()),
                ]),
                body_end: request.body_end,
                settlement_end: end,
                cleanup,
            },
            Io {
                input: request.input,
                output: Some(Sink {
                    file: request.stdout,
                    maximum: 2 * 1024 * 1024,
                }),
                stderr: Some(Sink {
                    file: request.stderr,
                    maximum: 2 * 1024 * 1024,
                }),
                capture_output: true,
                allow_nonzero: true,
            },
        )
        .await;
        let proof = &completed.observation;
        let input = if inherited {
            proof.stdin_inherited && proof.stdin_duplicate_sdk_owned
        } else {
            proof.stdin_close_attempted && proof.stdin_closed
        };
        let stdout = proof.stdout_bytes == completed.stdout.len() as u64
            && proof.stdout_sha256 == super::super::files::hex(&Sha256::digest(&completed.stdout));
        let stderr = proof.stderr_bytes == completed.stderr.len() as u64
            && proof.stderr_sha256 == super::super::files::hex(&Sha256::digest(&completed.stderr));
        let active = if cleanup {
            owner.final_work()
        } else {
            owner.check(Window::Body)
        };
        let settled = proof.pid.is_some()
            && input
            && stdout
            && stderr
            && proof.stdout_close_attempted
            && proof.stderr_close_attempted
            && proof.stdout_closed
            && proof.stderr_closed
            && proof.stdout_body_eof
            && proof.stderr_body_eof
            && proof.stdout_eof
            && proof.stderr_eof
            && proof.group_retired
            && proof.reaped
            && !proof.stdout_limited
            && !proof.stderr_limited
            && !proof.role_timed_out
            && proof.cleanup_errors.is_empty()
            && proof.bulk_complete
            && proof.stderr_sink_complete
            && proof.exit_status.is_some()
            && active.is_ok()
            && owner.now() < end;
        let mut error = completed.result.err();
        if cleanup && error == before {
            error = None;
        }
        if !settled && error.is_none() {
            error = Some(Cause::Close);
            owner.record(Cause::Close);
        }
        if let Err(cause) = active {
            error.get_or_insert(cause);
        }
        CommandOutput {
            stdout: completed.stdout,
            status: proof.exit_status,
            settled,
            stdout_complete: proof.stdout_body_eof && !proof.stdout_limited && stdout,
            stdout_bytes: proof.stdout_bytes,
            stderr_bytes: proof.stderr_bytes,
            error,
        }
    }
}
