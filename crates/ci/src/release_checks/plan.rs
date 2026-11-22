//! One fixed discovery expression, the root release fixture and the scanner
//! download that overlaps it.

use crate::{
    command::{Command, Program},
    runner,
};
use simplestchat_toolkit::{
    executable::Executable,
    tooling::{Environment, Failure, Result},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

pub(super) const BUDGET: Duration = Duration::from_secs(1140);
// 850 body + 200 cooperative TERM + 20 Native settlement + 10 final fences.
const FIXTURE_BODY: Duration = Duration::from_secs(850);
const GRACE: Duration = Duration::from_secs(200);
const FINAL: Duration = Duration::from_secs(30);
const FIXTURE_TAIL: Duration = Duration::from_secs(1080);
/// The scanner download's own bound, inside the fixture's.
const SCANNER_BODY: Duration = Duration::from_secs(300);
/// Each overlapped command's retained stdout and stderr.
pub(super) const MAX_LOG: u64 = 1024 * 1024;
pub(super) const EXPRESSION: &str = concat!(
    "process.stdout.write(require(",
    "\"./web/e2e/node_modules/playwright\"",
    ").chromium.executablePath())",
);
/// The three image scanners the later image check reads from `tools`.
const SCANNERS: [&str; 3] = ["syft", "grype", "gitleaks"];

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
    /// The private directory of the overlapped commands' logs and summary.
    pub(super) fn output(&self) -> PathBuf {
        self.temporary.join("release-checks")
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
    let left = remaining(deadline, FIXTURE_TAIL + Duration::from_secs(25))?;
    if left < Duration::from_secs(30) {
        return Err(Failure::new(124, "Release-check budget exhausted."));
    }
    command.timeout = Duration::from_secs(30);
    command.capture = Some(4096);
    Ok(command)
}
/// The fixture as root through the selected `sudo`: this same feature
/// binary, the explicit browser tools, the image under test, its new output
/// directory and the controller environment's Ansible.
pub(super) fn fixture(
    context: &Context,
    tools: [&Path; 3],
    sudo: Arc<Executable>,
    deadline: Instant,
) -> Result<Command> {
    let [node, chromium, http_policy] = tools;
    let mut command = Command::capture(Program::Sudo, &[]);
    command.arguments = vec![
        http_policy.as_os_str().to_owned(),
        "release-fixture".into(),
        "--disposable-host".into(),
        "--image".into(),
        context.image.clone().into(),
        "--output".into(),
        context.temporary.join("release-container").into_os_string(),
        "--node".into(),
        node.as_os_str().to_owned(),
        "--chromium".into(),
        chromium.as_os_str().to_owned(),
        "--http-policy-binary".into(),
        http_policy.as_os_str().to_owned(),
        "--ansible".into(),
        context
            .temporary
            .join("release-container-controller/bin/ansible-playbook")
            .into_os_string(),
    ];
    command.capture = None;
    if remaining(deadline, GRACE + FINAL)? < FIXTURE_BODY {
        return Err(Failure::new(124, "Release-check budget exhausted."));
    }
    command.timeout = FIXTURE_BODY;
    command.grace = GRACE;
    command.release_checks = Some(super::invocation::Invocation::new(
        context,
        sudo,
        &command,
        deadline,
        FIXTURE_TAIL,
    )?);
    Ok(command)
}
/// The download-only scanner installer for the later image check: the
/// checksum-pinned Linux AMD64 tools into `release-checks/tools`.
pub(super) fn scanner(context: &Context, deadline: Instant) -> Result<Command> {
    let mut arguments = vec![
        "run",
        "--locked",
        "-q",
        "-p",
        "simplestchat-security",
        "--",
        "tools",
        "install",
        "--tools",
    ];
    arguments.extend(SCANNERS);
    arguments.extend(["--platform", "linux-x86_64", "--directory"]);
    let mut command = Command::capture(Program::Cargo, &arguments);
    command
        .arguments
        .push(context.output().join("tools").into_os_string());
    command.capture = None;
    if remaining(deadline, GRACE + FINAL)? < SCANNER_BODY {
        return Err(Failure::new(124, "Release-check budget exhausted."));
    }
    command.timeout = SCANNER_BODY;
    command.grace = Duration::from_secs(5);
    Ok(command)
}
