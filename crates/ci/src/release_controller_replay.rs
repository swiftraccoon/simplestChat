//! The collector replays its retained log through the reserved toolkit child
//! of the controller executable, retained by its source check.

use super::{
    plan::{self, Context},
    source::Source,
};
use simplestchat_toolkit::{
    owned_process::Signals,
    replay::{CHILD, run},
    tooling::{self, Result},
};
use std::{process::Stdio, time::Instant};

pub(super) async fn replay(
    context: &Context,
    bytes: &[u8],
    signals: &mut Signals,
    deadline: Instant,
) -> Result<()> {
    let executable = std::env::current_exe().map_err(|_| plan::failure())?;
    let proof = Source::open(&executable, plan::MAX_EXECUTABLE_BYTES)?;
    let mut command = tooling::command(&context.root, &context.environment, executable, [CHILD]);
    command.stdout(Stdio::inherit()).stderr(Stdio::null());
    let result = run(command, bytes, signals, deadline).await;
    let closed = proof.finish();
    result.and(closed)
}
