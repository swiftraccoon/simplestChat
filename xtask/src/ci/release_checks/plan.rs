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

pub(super) const BUDGET: Duration = Duration::from_secs(1140);
// 850 body + 200 cooperative TERM + 20 Native settlement + 10 final fences.
const COORDINATOR_BODY: Duration = Duration::from_secs(850);
const GRACE: Duration = Duration::from_secs(200);
const FINAL: Duration = Duration::from_secs(30);
const COORDINATOR_TAIL: Duration = Duration::from_secs(1080);
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
    pub http_policy: PathBuf,
}
impl Context {
    pub(super) fn new(
        root: &Path,
        environment: Environment,
        system: &str,
        http_policy: &Path,
    ) -> Result<Self> {
        if !runner::authorized(&environment, system) {
            return Err(Failure::new(
                2,
                "Release checks require hosted or disposable Linux CI.",
            ));
        }
        let temporary = PathBuf::from(runner::text(&environment, "RUNNER_TEMP"));
        let image = runner::text(&environment, "PRODUCTION_IMAGE");
        if !http_policy.is_absolute()
            || http_policy.as_os_str().as_encoded_bytes().len() > 4096
            || !temporary.is_absolute()
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
            http_policy: http_policy.into(),
        })
    }
}
fn remaining(deadline: Instant, reserve: Duration) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .and_then(|left| left.checked_sub(reserve))
        .filter(|left| !left.is_zero())
        .ok_or_else(|| Failure::new(124, "Release-check budget exhausted."))
}
pub(super) fn fence(deadline: Instant) -> Result<()> {
    remaining(deadline, Duration::ZERO).map(|_| ())
}
pub(super) fn discover(node: Arc<Executable>, deadline: Instant) -> Result<Command> {
    let mut command = Command::capture(Program::Node, &["-e", EXPRESSION]);
    command.node = Some(node);
    let left = remaining(deadline, COORDINATOR_TAIL + Duration::from_secs(25))?;
    if left < Duration::from_secs(30) {
        return Err(Failure::new(124, "Release-check budget exhausted."));
    }
    command.timeout = Duration::from_secs(30);
    command.capture = Some(4096);
    Ok(command)
}
pub(super) fn coordinator(
    context: &Context,
    node: &Path,
    chromium: &Path,
    http_policy: &Path,
    python: Arc<Executable>,
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
        "--http-policy-binary".into(),
        http_policy.as_os_str().to_owned(),
        "--image".into(),
        context.image.clone().into(),
        "--fixture-output".into(),
        context.temporary.join("release-container").into_os_string(),
        "--output".into(),
        context.temporary.join("release-checks").into_os_string(),
    ]);
    command.capture = None;
    if remaining(deadline, GRACE + FINAL)? < COORDINATOR_BODY {
        return Err(Failure::new(124, "Release-check budget exhausted."));
    }
    command.timeout = COORDINATOR_BODY;
    command.grace = GRACE;
    command.release_checks = Some(super::invocation::Invocation::new(
        context,
        python,
        &command,
        deadline,
        COORDINATOR_TAIL,
    )?);
    Ok(command)
}
