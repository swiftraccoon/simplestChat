//! Fixed manual image recipe; all identities occupy literal argument slots.

use crate::{
    ci::{
        command::{Command, Program},
        container_engine::{Policy, Scope},
        runner,
    },
    tooling::{Environment, Failure, Result},
};
use std::{
    cell::Cell,
    ffi::OsStr,
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

pub(super) const TAG: &str = "simplestchat-loadtest:ci";
pub(super) const OWNER: &str = "com.simplestchat.load-image";
pub(super) const MARKER: &str = "SIMPLESTCHAT_LOAD_IMAGE_OWNER";
pub(super) const ENTRY: Duration = Duration::from_secs(2700);
pub(super) const TAIL: Duration = Duration::from_secs(1500);
pub(super) const SETTLEMENT: Duration = Duration::from_secs(20);
pub(super) const RAW_LIMIT: u64 = 8 * 1024 * 1024 * 1024;
pub(super) const COMPRESSED_LIMIT: u64 = 9 * 1024 * 1024 * 1024;
pub(super) const LOG_LIMIT: u64 = 2 * 1024 * 1024;
pub(super) const BUILD_LOG_LIMIT: u64 = 64 * 1024 * 1024;
pub(super) fn failure() -> Failure {
    Scope::LoadImage.failure()
}
pub(super) fn expired() -> Failure {
    Scope::LoadImage.expired()
}
pub(super) fn revision(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
struct TimeInner {
    started: Instant,
    #[cfg(test)]
    virtual_now: Cell<Option<Duration>>,
}
#[derive(Clone)]
pub(super) struct Time(Rc<TimeInner>);
impl Time {
    fn new(started: Instant) -> Self {
        Self(Rc::new(TimeInner {
            started,
            #[cfg(test)]
            virtual_now: Cell::new(None),
        }))
    }
    pub fn elapsed(&self) -> Duration {
        #[cfg(test)]
        if let Some(value) = self.0.virtual_now.get() {
            return value;
        }
        self.0.started.elapsed()
    }
    #[cfg(test)]
    pub fn set(&self, value: Duration) {
        self.0.virtual_now.set(Some(value));
    }
}
impl super::engine::Clock for Time {
    fn elapsed(&self) -> Duration {
        self.elapsed()
    }
    async fn sleep(&mut self, duration: Duration) {
        #[cfg(test)]
        if self.0.virtual_now.get().is_some() {
            self.set(self.elapsed() + duration);
            return;
        }
        tokio::time::sleep(duration).await;
    }
}

pub(super) struct Context {
    pub root: PathBuf,
    pub temporary: PathBuf,
    pub environment: Environment,
    pub revision: String,
    pub time: Time,
    pub hash_used: Cell<Duration>,
}
impl Context {
    pub fn new(
        root: &Path,
        environment: Environment,
        system: &str,
        started: Instant,
    ) -> Result<Self> {
        if !runner::authorized(&environment, system) {
            return Err(Failure::new(
                2,
                "Load image requires hosted or disposable Linux CI.",
            ));
        }
        let revision = runner::text(&environment, "GITHUB_SHA");
        let temporary = environment
            .get(OsStr::new("RUNNER_TEMP"))
            .map(PathBuf::from)
            .ok_or_else(failure)?;
        if !self::revision(revision)
            || !super::engine::safe_path(root)
            || !super::engine::safe_path(&temporary)
            || temporary.to_str().is_none()
            || temporary.as_os_str().as_encoded_bytes().contains(&b',')
            || root.to_str().is_none()
            || root.as_os_str().as_encoded_bytes().contains(&b',')
        {
            return Err(Failure::new(2, "Load image inputs invalid."));
        }
        let revision = revision.to_owned();
        Ok(Self {
            root: root.into(),
            temporary,
            environment,
            revision,
            time: Time::new(started),
            hash_used: Cell::new(Duration::ZERO),
        })
    }
    pub fn archive_name(&self) -> String {
        format!("simplestchat-loadtest-{}.tar.zst", self.revision)
    }
    pub fn build(&self, endpoint: &str, builder: &str, cache: bool) -> Command {
        let mut command = Policy::load_image().command(
            Some(endpoint),
            &[
                "buildx",
                "build",
                "--builder",
                builder,
                "--pull",
                "--load",
                "--target",
                "loadtest",
                "--platform",
                "linux/amd64",
                "--provenance=false",
                "--sbom=false",
            ],
            false,
        );
        if cache {
            command.arguments.extend([
                "--cache-from".into(),
                format!(
                    "type=local,src={}",
                    self.temporary.join("buildx-cache").display()
                )
                .into(),
            ]);
        }
        command.arguments.extend([
            "--build-arg".into(),
            format!("SOURCE_REVISION={}", self.revision).into(),
            "--label".into(),
            format!("org.opencontainers.image.revision={}", self.revision).into(),
            "--tag".into(),
            TAG.into(),
            ".".into(),
        ]);
        command.timeout = Duration::from_secs(2400);
        command.grace = Duration::from_secs(120);
        command.capture = Some(BUILD_LOG_LIMIT);
        command
    }
}
pub(super) fn build_command(command: &Command) -> bool {
    command.program == Program::Docker && command.arguments.iter().any(|v| v == "--target")
}
pub(super) fn available(context: &Context, command: &Command) -> Result<Duration> {
    let elapsed = context.time.elapsed();
    let hash = Duration::from_secs(120)
        .checked_sub(context.hash_used.get())
        .ok_or_else(expired)?;
    let tail = if build_command(command) {
        TAIL
    } else if command.program == Program::Docker && command.arguments.iter().any(|v| v == "save") {
        Duration::from_secs(441) + hash
    } else if command.program == Program::Zstd {
        if command.arguments.iter().any(|v| v == "--decompress") {
            Duration::from_secs(107) + hash
        } else {
            Duration::from_secs(248) + hash
        }
    } else {
        Duration::from_secs(30)
    };
    ENTRY
        .checked_sub(elapsed)
        .and_then(|left| left.checked_sub(tail + command.grace + SETTLEMENT))
        .filter(|left| !left.is_zero())
        .ok_or_else(expired)
}
