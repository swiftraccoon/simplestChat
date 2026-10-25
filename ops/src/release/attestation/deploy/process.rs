//! One shared safe owner, with fixed-role inherited input and retained bulk output.

use super::super::control::{Cause, Owner};
pub(super) use super::super::stage::process::Invocation;
#[cfg(test)]
pub(super) use super::super::stage::process::run;
use super::super::stage::process::{Io, Sink, run_io};
use super::{CommandOutput, Executor, Role};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub(super) struct Native;

impl Executor for Native {
    async fn run(&mut self, owner: &mut Owner, request: super::Invocation<'_>) -> CommandOutput {
        let entered = owner.now();
        let end = request.settlement_end;
        let inherited = request.input.is_some();
        let bulk = request.output.is_some();
        let allow_nonzero = matches!(request.role, Role::Ready | Role::Metrics);
        let completed = run_io(
            owner,
            Invocation {
                program: request.program,
                arguments: request.arguments,
                environment: BTreeMap::from([
                    ("PATH".into(), "/usr/sbin:/usr/bin:/sbin:/bin".into()),
                    ("LC_ALL".into(), "C".into()),
                ]),
                body_end: request.body_end,
                settlement_end: end,
                cleanup: request.cleanup,
            },
            Io {
                stderr: None,
                capture_output: false,
                input: request.input,
                output: request.output.map(|sink| Sink {
                    file: sink.file,
                    maximum: sink.maximum,
                }),
                allow_nonzero,
            },
        )
        .await;
        let proof = &completed.observation;
        let stdin = if inherited {
            // Stable Command consumes its duplicate. Its SDK-close status is
            // opaque; the operation still checks its retained original close.
            proof.stdin_inherited && proof.stdin_duplicate_sdk_owned && proof.reaped
        } else {
            proof.stdin_close_attempted && proof.stdin_closed
        };
        let stdout = if bulk {
            completed.stdout.is_empty()
                && proof.bulk_complete
                && proof.bulk_bytes == proof.stdout_bytes
                && proof.bulk_sha256 == proof.stdout_sha256
        } else {
            proof.stdout_bytes == completed.stdout.len() as u64
                && proof.stdout_sha256
                    == super::super::files::hex(&Sha256::digest(&completed.stdout))
        };
        let stderr = proof.stderr_bytes == completed.stderr.len() as u64
            && proof.stderr_sha256 == super::super::files::hex(&Sha256::digest(&completed.stderr));
        let settled = proof.pid.is_some()
            && stdin
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
            && !proof.timed_out
            && proof.cleanup_errors.is_empty()
            && proof.elapsed <= end.saturating_duration_since(entered)
            && proof.exit_status.is_some();
        let stdout_complete = proof.stdout_body_eof && !proof.stdout_limited && stdout;
        let mut error = completed.result.err();
        if !settled && error.is_none() {
            owner.record(Cause::Close);
            error = Some(Cause::Close);
        }
        CommandOutput {
            stdout: completed.stdout,
            stderr: completed.stderr,
            status: proof.exit_status,
            error,
            settled,
            stdout_complete,
            bulk_bytes: proof.bulk_bytes,
            bulk_sha256: proof.bulk_sha256.clone(),
            bulk_complete: proof.bulk_complete,
        }
    }
}
