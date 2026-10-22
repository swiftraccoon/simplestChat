//! One fixed discovery expression and the reference coordinator's exact inputs.

use super::files::Executable;
use crate::{
    ci::{
        command::{Command, Program},
        runner,
    },
    tooling::{Environment, Failure, Result},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

pub(super) const BUDGET: Duration = Duration::from_secs(11 * 60);
// The coordinator cancels both separately-sessioned tasks and gives their
// bounded_process owners up to 5s TERM + 5s reap, in parallel. Keep that grace.
const GRACE: Duration = Duration::from_secs(15);
const RESERVE: Duration = Duration::from_secs(20);
pub(super) const EXPRESSION: &str = concat!(
    "process.stdout.write(require(",
    "\"./web/e2e/node_modules/playwright\"",
    ").chromium.executablePath())",
);

pub(super) struct Context {
    pub root: PathBuf,
    pub environment: Environment,
    pub temporary: PathBuf,
    pub image: String,
}
impl Context {
    pub(super) fn new(root: &Path, environment: Environment, system: &str) -> Result<Self> {
        if !runner::authorized(&environment, system) {
            return Err(Failure::new(
                2,
                "Release checks require hosted or disposable Linux CI.",
            ));
        }
        let temporary = PathBuf::from(runner::text(&environment, "RUNNER_TEMP"));
        let image = runner::text(&environment, "PRODUCTION_IMAGE");
        if !temporary.is_absolute()
            || temporary.is_symlink()
            || !temporary.is_dir()
            || temporary.as_os_str().as_encoded_bytes().len() > 4096
            || !image.strip_prefix("sha256:").is_some_and(|digest| {
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
        {
            return Err(Failure::new(2, "Release-check inputs invalid."));
        }
        Ok(Self {
            root: root.into(),
            temporary,
            image: image.into(),
            environment,
        })
    }
}
fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .and_then(|left| left.checked_sub(RESERVE))
        .filter(|left| !left.is_zero())
        .ok_or_else(|| Failure::new(124, "Release-check budget exhausted."))
}
pub(super) fn discover(node: Arc<Executable>, deadline: Instant) -> Result<Command> {
    let mut command = Command::capture(Program::Node, &["-e", EXPRESSION]);
    command.node = Some(node);
    command.timeout = remaining(deadline)?.min(Duration::from_secs(30));
    command.capture = Some(4096);
    Ok(command)
}
pub(super) fn coordinator(
    context: &Context,
    node: &Path,
    chromium: &Path,
    deadline: Instant,
) -> Result<Command> {
    let mut command = Command::capture(
        Program::Python,
        &["-B", "build/ci_release_checks.py", "--controller"],
    );
    command.arguments.extend([
        context
            .temporary
            .join("release-container-controller/bin/python")
            .into_os_string(),
        "--node".into(),
        node.as_os_str().to_owned(),
        "--chromium".into(),
        chromium.as_os_str().to_owned(),
        "--image".into(),
        context.image.clone().into(),
        "--fixture-output".into(),
        context.temporary.join("release-container").into_os_string(),
        "--output".into(),
        context.temporary.join("release-checks").into_os_string(),
    ]);
    command.capture = None;
    command.timeout = remaining(deadline)?;
    command.grace = GRACE;
    Ok(command)
}
