//! Frozen local Docker transport, daemon observations and exact resource retirement.
//! These are optimistic caller-trusted engine observations, not authentication.

use crate::{
    ci::{
        command::{Command, Program},
        process::Executor,
    },
    tooling::{Environment, Failure, Result},
};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    path::{Component, Path},
    time::{Duration, Instant},
};

/// Only maintained owners select a policy; callers cannot provide error text.
#[derive(Clone, Copy)]
pub(in crate::ci) enum Scope {
    Smoke,
    Validation,
}
impl Scope {
    pub fn failure(self) -> Failure {
        Failure::new(
            1,
            match self {
                Self::Smoke => "Container smoke evidence rejected.",
                Self::Validation => "Configuration validation evidence rejected.",
            },
        )
    }
    pub fn expired(self) -> Failure {
        Failure::new(
            124,
            match self {
                Self::Smoke => "Container smoke watchdog expired.",
                Self::Validation => "Configuration validation watchdog expired.",
            },
        )
    }
    fn endpoint_failure(self) -> Failure {
        Failure::new(
            2,
            match self {
                Self::Smoke => "Container smoke requires a local Unix Docker endpoint.",
                Self::Validation => {
                    "Configuration validation requires a local Unix Docker endpoint."
                }
            },
        )
    }
    fn context_failure(self) -> Failure {
        Failure::new(
            2,
            match self {
                Self::Smoke => "Container smoke context selection invalid.",
                Self::Validation => "Configuration validation context selection invalid.",
            },
        )
    }
    fn log_failure(self) -> &'static str {
        match self {
            Self::Smoke => "Container smoke command and log settlement failed.",
            Self::Validation => "Configuration command and log settlement failed.",
        }
    }
}

/// Fixed internal scheduling profiles. Neither is an OS/reap/filesystem wall bound.
#[derive(Clone, Copy)]
pub(in crate::ci) struct Policy {
    scope: Scope,
    watchdog: Duration,
    reserve: Duration,
    settlement: Duration,
    metadata_timeout: Duration,
    metadata_grace: Duration,
}
impl Policy {
    pub fn smoke() -> Self {
        Self {
            scope: Scope::Smoke,
            watchdog: Duration::from_secs(300),
            reserve: Duration::from_secs(60),
            settlement: Duration::from_secs(20),
            metadata_timeout: Duration::from_secs(30),
            metadata_grace: Duration::from_secs(5),
        }
    }
    pub fn validation() -> Self {
        Self {
            scope: Scope::Validation,
            watchdog: Duration::from_secs(570),
            reserve: Duration::from_secs(450),
            settlement: Duration::from_secs(20),
            metadata_timeout: Duration::from_secs(5),
            metadata_grace: Duration::from_secs(1),
        }
    }
    fn validate(self) -> Result<()> {
        if self.reserve >= self.watchdog
            || self.metadata_timeout.is_zero()
            || self.metadata_grace.is_zero()
            || self.settlement.is_zero()
            || self.metadata_timeout + self.metadata_grace + self.settlement >= self.reserve
        {
            return Err(self.scope.failure());
        }
        Ok(())
    }
    pub fn watchdog(self) -> Duration {
        self.watchdog
    }
    pub fn reserve(self) -> Duration {
        self.reserve
    }
    pub fn command(self, endpoint: Option<&str>, args: &[&str], cleanup: bool) -> Command {
        let mut command = Command::capture(Program::Docker, &[]);
        if let Some(endpoint) = endpoint {
            command.arguments.extend(["--host".into(), endpoint.into()]);
        }
        command
            .arguments
            .extend(args.iter().map(std::ffi::OsString::from));
        command.capture = Some(64 * 1024);
        command.timeout = self.metadata_timeout;
        command.grace = self.metadata_grace;
        command.cleanup = cleanup;
        command
    }
}
pub(in crate::ci) fn line(bytes: &[u8], scope: Scope) -> Result<&str> {
    let value = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    if value.is_empty() || value.iter().any(|byte| matches!(byte, 0..=31 | 127)) {
        return Err(scope.failure());
    }
    std::str::from_utf8(value).map_err(|_| scope.failure())
}
pub(in crate::ci) fn container_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(in crate::ci) fn image_id(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(container_id)
}
pub(in crate::ci) fn safe_path(path: &Path) -> bool {
    path.is_absolute()
        && path.as_os_str().as_encoded_bytes().len() <= 4096
        && !path
            .as_os_str()
            .as_encoded_bytes()
            .iter()
            .any(|b| matches!(b, 0..=31 | 127))
        && path
            .components()
            .all(|p| matches!(p, Component::RootDir | Component::Normal(_)))
}
pub(in crate::ci) fn endpoint(value: &str, scope: Scope) -> Result<String> {
    let path = value
        .strip_prefix("unix://")
        .ok_or_else(|| scope.endpoint_failure())?;
    if !safe_path(Path::new(path)) || path.bytes().any(|b| b <= 32 || b == 127) {
        return Err(scope.endpoint_failure());
    }
    Ok(value.into())
}
pub(in crate::ci) async fn discover(
    environment: &Environment,
    runner: &mut impl Executor,
    policy: Policy,
) -> Result<String> {
    runner.ready().await?;
    let selected = environment
        .get(OsStr::new("DOCKER_HOST"))
        .filter(|v| !v.is_empty());
    let context = environment
        .get(OsStr::new("DOCKER_CONTEXT"))
        .filter(|v| !v.is_empty());
    if let Some(value) = context {
        let text = value
            .to_str()
            .ok_or_else(|| policy.scope.context_failure())?;
        if text.len() > 4096 || text.bytes().any(|b| matches!(b, 0..=31 | 127)) {
            return Err(policy.scope.context_failure());
        }
    }
    if context.is_none()
        && let Some(selected) = selected
    {
        return endpoint(
            selected.to_str().ok_or_else(|| policy.scope.failure())?,
            policy.scope,
        );
    }
    let mut command = policy.command(
        None,
        &[
            "context",
            "inspect",
            "--format",
            "{{.Endpoints.docker.Host}}",
        ],
        false,
    );
    command.capture = Some(4096);
    let output = runner.run(command).await?;
    runner.ready().await?;
    endpoint(line(&output, policy.scope)?, policy.scope)
}
pub(in crate::ci) fn frozen_environment(environment: &Environment) -> Environment {
    let mut value = environment.clone();
    for key in [
        "DOCKER_CONTEXT",
        "DOCKER_HOST",
        "DOCKER_TLS_VERIFY",
        "DOCKER_CERT_PATH",
        "DOCKER_API_VERSION",
        "BUILDX_BUILDER",
        "BUILDKIT_HOST",
    ] {
        value.remove(OsStr::new(key));
    }
    value
}
pub(in crate::ci) trait Clock {
    fn elapsed(&self) -> Duration;
    async fn sleep(&mut self, duration: Duration);
}
pub(in crate::ci) struct NativeClock(Instant);
impl NativeClock {
    pub fn new(started: Instant) -> Self {
        Self(started)
    }
}
impl Clock for NativeClock {
    fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }
    async fn sleep(&mut self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

pub(in crate::ci) struct Core<'a, E, C> {
    pub endpoint: &'a str,
    pub runner: &'a mut E,
    pub clock: &'a mut C,
    identity: String,
    policy: Policy,
}
impl<'a, E: Executor, C: Clock> Core<'a, E, C> {
    async fn rejected(mut command: Command, error: Failure, scope: Scope) -> Result<Vec<u8>> {
        // Admission can fail before Native receives the sink. Retire its one
        // owner here too; Drop alone would abort the log task without proving
        // EOF and explicit descriptor closes. Keep the primary status fixed.
        let settled = match command.combined_log.take() {
            Some(log) => match std::sync::Arc::try_unwrap(log) {
                Ok(log) => log.finish().await.and_then(|value| {
                    if value.complete {
                        Ok(())
                    } else {
                        Err(scope.failure())
                    }
                }),
                Err(_) => Err(scope.failure()),
            },
            None => Ok(()),
        };
        match settled {
            Ok(()) => Err(error),
            Err(_) => Err(Failure::new(error.code, scope.log_failure())),
        }
    }
    pub fn identity_sha256(&self) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(self.identity.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
    pub fn endpoint_sha256(&self) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(self.endpoint.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
    pub async fn bind(
        endpoint: &'a str,
        runner: &'a mut E,
        clock: &'a mut C,
        policy: Policy,
    ) -> Result<Self> {
        policy.validate()?;
        self::endpoint(endpoint, policy.scope)?;
        let mut value = Self {
            endpoint,
            runner,
            clock,
            identity: String::new(),
            policy,
        };
        let bytes = value
            .raw(value.command(&["info", "--format", "{{.ID}}"], false))
            .await?;
        let identity = line(&bytes, value.policy.scope)?;
        // Docker's ID is opaque (UUID and colon-group forms both exist). This
        // equality observation is not authentication or a global uniqueness proof.
        if identity.len() > 256 {
            return Err(value.policy.scope.failure());
        }
        value.identity = identity.into();
        Ok(value)
    }
    pub fn policy(&self) -> Policy {
        self.policy
    }
    pub async fn raw(&mut self, mut command: Command) -> Result<Vec<u8>> {
        let limit = if command.cleanup {
            self.policy.watchdog
        } else {
            self.policy.watchdog - self.policy.reserve
        };
        let admitted = async {
            if !command.cleanup {
                self.runner.ready().await?;
            }
            limit
                .checked_sub(self.clock.elapsed())
                .and_then(|duration| duration.checked_sub(command.grace + self.policy.settlement))
                .filter(|duration| !duration.is_zero())
                .ok_or_else(|| self.policy.scope.expired())
        }
        .await;
        let available = match admitted {
            Ok(available) => available,
            Err(error) => return Self::rejected(command, error, self.policy.scope).await,
        };
        command.timeout = command.timeout.min(available);
        let output = self.runner.run(command).await?;
        if self.clock.elapsed() >= limit {
            return Err(self.policy.scope.expired());
        }
        Ok(output)
    }
    pub async fn check(&mut self, cleanup: bool) -> Result<()> {
        let output = self
            .raw(self.command(&["info", "--format", "{{.ID}}"], cleanup))
            .await?;
        if line(&output, self.policy.scope)? != self.identity {
            return Err(self.policy.scope.failure());
        }
        Ok(())
    }
    pub async fn run(&mut self, command: Command) -> Result<Vec<u8>> {
        if let Err(error) = self.check(command.cleanup).await {
            return Self::rejected(command, error, self.policy.scope).await;
        }
        self.raw(command).await
    }
    pub async fn image(&mut self, selected: &str, cleanup: bool) -> Result<String> {
        let output = self
            .run(self.command(
                &["image", "inspect", "--format", "{{.Id}}", selected],
                cleanup,
            ))
            .await?;
        let image = line(&output, self.policy.scope)?;
        if !image_id(image) || (image_id(selected) && image != selected) {
            return Err(self.policy.scope.failure());
        }
        Ok(image.into())
    }
    pub fn command(&self, args: &[&str], cleanup: bool) -> Command {
        self.policy.command(Some(self.endpoint), args, cleanup)
    }
    pub fn scope(&self) -> Scope {
        self.policy.scope
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(in crate::ci) struct Inspection {
    pub id: String,
    pub name: String,
    pub image: String,
    pub config: Config,
    pub state: State,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(in crate::ci) struct Config {
    pub image: String,
    pub labels: BTreeMap<String, String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(in crate::ci) struct State {
    pub running: bool,
    pub status: Option<String>,
    pub exit_code: Option<i64>,
}

pub(in crate::ci) struct OwnedContainer {
    scope: Scope,
    pub name: String,
    label_key: String,
    label_value: String,
    pub image: Option<String>,
    pub id: Option<String>,
    pub attempted: bool,
    pub creation_completed: bool,
}
impl OwnedContainer {
    pub fn new(
        scope: Scope,
        name: String,
        label_key: String,
        label_value: String,
        image: Option<String>,
    ) -> Self {
        Self {
            scope,
            name,
            label_key,
            label_value,
            image,
            id: None,
            attempted: false,
            creation_completed: false,
        }
    }
    pub fn id(&self) -> Result<&str> {
        self.id.as_deref().ok_or_else(|| self.scope.failure())
    }
    pub async fn inspect<E: Executor, C: Clock>(
        &mut self,
        engine: &mut Core<'_, E, C>,
        id: &str,
        cleanup: bool,
    ) -> Result<Inspection> {
        if !container_id(id) || self.id.as_deref().is_some_and(|old| old != id) {
            return Err(engine.scope().failure());
        }
        let bytes = engine
            .run(engine.command(
                &["container", "inspect", "--format", "{{json .}}", id],
                cleanup,
            ))
            .await?;
        let observed: Inspection =
            serde_json::from_slice(&bytes).map_err(|_| engine.scope().failure())?;
        let image = self
            .image
            .as_ref()
            .ok_or_else(|| engine.scope().failure())?;
        if observed.id != id
            || observed.name != format!("/{}", self.name)
            || &observed.image != image
            || observed.config.labels.get(&self.label_key) != Some(&self.label_value)
            || &observed.config.image != image
        {
            return Err(engine.scope().failure());
        }
        self.id = Some(id.into());
        Ok(observed)
    }
    pub async fn locate<E: Executor, C: Clock>(
        &mut self,
        engine: &mut Core<'_, E, C>,
    ) -> Result<Option<String>> {
        if !self.attempted {
            return Ok(None);
        }
        let filter = self
            .id
            .as_ref()
            .map(|id| format!("id={id}"))
            .unwrap_or_else(|| format!("name=^/{}$", self.name));
        let query = [
            "container",
            "ls",
            "--all",
            "--no-trunc",
            "--filter",
            &filter,
            "--format",
            "{{.ID}}",
        ];
        let bytes = engine.run(engine.command(&query, true)).await?;
        if bytes.is_empty() {
            // A killed/disconnected create client may have an in-flight daemon
            // request. Current absence alone cannot prove that it will not commit.
            return if self.creation_completed || self.id.is_some() {
                Ok(None)
            } else {
                Err(engine.scope().failure())
            };
        }
        let id = line(&bytes, engine.scope())?.to_owned();
        self.inspect(engine, &id, true).await?;
        Ok(Some(id))
    }
    pub async fn retire<E: Executor, C: Clock>(
        &mut self,
        engine: &mut Core<'_, E, C>,
    ) -> Result<()> {
        let Some(id) = self.locate(engine).await? else {
            return Ok(());
        };
        engine
            .run(engine.command(&["container", "rm", "--force", "--volumes", &id], true))
            .await?;
        let filter = format!("id={id}");
        let query = [
            "container",
            "ls",
            "--all",
            "--no-trunc",
            "--filter",
            &filter,
            "--format",
            "{{.ID}}",
        ];
        let remaining = engine.run(engine.command(&query, true)).await?;
        if !remaining.is_empty() {
            return Err(engine.scope().failure());
        }
        Ok(())
    }
}
