//! One frozen Unix transport and engine identity. Inspection failure is not absence.

use super::{
    options::{self, Context},
    plan,
};
use crate::{
    ci::{
        command::{Command, Program},
        process::Executor,
    },
    tooling::{Failure, Result},
};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

pub(super) fn failure() -> Failure {
    Failure::new(1, "Container smoke evidence rejected.")
}

pub(super) fn line(bytes: &[u8]) -> Result<&str> {
    let value = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    if value.is_empty() || value.iter().any(|byte| matches!(byte, 0..=31 | 127)) {
        return Err(failure());
    }
    std::str::from_utf8(value).map_err(|_| failure())
}

pub(super) fn endpoint(value: &str) -> Result<String> {
    let path = value
        .strip_prefix("unix://")
        .ok_or_else(|| Failure::new(2, "Container smoke requires a local Unix Docker endpoint."))?;
    if !options::safe_path(std::path::Path::new(path))
        || path.bytes().any(|byte| byte <= 32 || byte == 127)
    {
        return Err(Failure::new(
            2,
            "Container smoke requires a local Unix Docker endpoint.",
        ));
    }
    Ok(value.into())
}

pub(super) async fn discover(context: &Context, runner: &mut impl Executor) -> Result<String> {
    runner.ready().await?;
    let selected = context
        .environment
        .get(std::ffi::OsStr::new("DOCKER_HOST"))
        .filter(|value| !value.is_empty());
    let context_name = context
        .environment
        .get(std::ffi::OsStr::new("DOCKER_CONTEXT"))
        .filter(|value| !value.is_empty());
    if let Some(value) = context_name {
        let text = value
            .to_str()
            .ok_or_else(|| Failure::new(2, "Container smoke context selection invalid."))?;
        if text.len() > 4096 || text.bytes().any(|byte| matches!(byte, 0..=31 | 127)) {
            return Err(Failure::new(
                2,
                "Container smoke context selection invalid.",
            ));
        }
    }
    let contextual = context_name.is_some();
    if !contextual && let Some(selected) = selected {
        return endpoint(selected.to_str().ok_or_else(failure)?);
    }
    let mut command = Command::capture(
        Program::Docker,
        &[
            "context",
            "inspect",
            "--format",
            "{{.Endpoints.docker.Host}}",
        ],
    );
    command.capture = Some(4096);
    let output = runner.run(command).await?;
    runner.ready().await?;
    endpoint(line(&output)?)
}

pub(super) trait Clock {
    fn elapsed(&self) -> Duration;
    async fn sleep(&mut self, duration: Duration);
}
pub(super) struct NativeClock(Instant);
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

pub(super) struct Engine<'a, E, C> {
    pub endpoint: &'a str,
    pub runner: &'a mut E,
    pub clock: &'a mut C,
    identity: String,
}
impl<'a, E: Executor, C: Clock> Engine<'a, E, C> {
    async fn rejected(mut command: Command, error: Failure) -> Result<Vec<u8>> {
        // Admission can fail before Native receives the sink. Retire its one
        // owner here too; Drop alone would abort the log task without proving
        // EOF and explicit descriptor closes. Keep the primary status fixed.
        let settled = match command.combined_log.take() {
            Some(log) => match std::sync::Arc::try_unwrap(log) {
                Ok(log) => log.finish().await.and_then(|value| {
                    if value.complete {
                        Ok(())
                    } else {
                        Err(failure())
                    }
                }),
                Err(_) => Err(failure()),
            },
            None => Ok(()),
        };
        match settled {
            Ok(()) => Err(error),
            Err(_) => Err(Failure::new(
                error.code,
                "Container smoke command and log settlement failed.",
            )),
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
    pub async fn bind(endpoint: &'a str, runner: &'a mut E, clock: &'a mut C) -> Result<Self> {
        self::endpoint(endpoint)?;
        let mut value = Self {
            endpoint,
            runner,
            clock,
            identity: String::new(),
        };
        let bytes = value
            .raw(plan::docker(
                endpoint,
                &["info", "--format", "{{.ID}}"],
                false,
            ))
            .await?;
        let identity = line(&bytes)?;
        // Docker's ID is opaque (UUID and colon-group forms both exist). This
        // equality observation is not authentication or a global uniqueness proof.
        if identity.len() > 256 {
            return Err(failure());
        }
        value.identity = identity.into();
        Ok(value)
    }
    async fn raw(&mut self, mut command: Command) -> Result<Vec<u8>> {
        let limit = if command.cleanup {
            plan::WATCHDOG
        } else {
            plan::WATCHDOG - plan::RESERVE
        };
        let admitted = async {
            if !command.cleanup {
                self.runner.ready().await?;
            }
            limit
                .checked_sub(self.clock.elapsed())
                .and_then(|duration| {
                    duration.checked_sub(command.grace + plan::SETTLEMENT_ALLOWANCE)
                })
                .filter(|duration| !duration.is_zero())
                .ok_or_else(|| Failure::new(124, "Container smoke watchdog expired."))
        }
        .await;
        let available = match admitted {
            Ok(available) => available,
            Err(error) => return Self::rejected(command, error).await,
        };
        command.timeout = command.timeout.min(available);
        let output = self.runner.run(command).await?;
        if self.clock.elapsed() >= limit {
            return Err(Failure::new(124, "Container smoke watchdog expired."));
        }
        Ok(output)
    }
    pub async fn check(&mut self, cleanup: bool) -> Result<()> {
        let output = self
            .raw(plan::docker(
                self.endpoint,
                &["info", "--format", "{{.ID}}"],
                cleanup,
            ))
            .await?;
        if line(&output)? != self.identity {
            return Err(failure());
        }
        Ok(())
    }
    pub async fn run(&mut self, command: Command) -> Result<Vec<u8>> {
        if let Err(error) = self.check(command.cleanup).await {
            return Self::rejected(command, error).await;
        }
        self.raw(command).await
    }
    pub async fn image(&mut self, selected: &str, cleanup: bool) -> Result<String> {
        let output = self
            .run(plan::docker(
                self.endpoint,
                &["image", "inspect", "--format", "{{.Id}}", selected],
                cleanup,
            ))
            .await?;
        let image = line(&output)?;
        if !options::image_id(image) || (options::image_id(selected) && image != selected) {
            return Err(failure());
        }
        Ok(image.into())
    }
    pub async fn database_image(&mut self) -> Result<String> {
        // A settled inspect failure is not absence evidence. It only permits
        // retrying the reference's implicit pull of this one fixed digest.
        let command = plan::docker(
            self.endpoint,
            &[
                "image",
                "inspect",
                "--format",
                "{{.Id}}",
                plan::POSTGRES_IMAGE,
            ],
            false,
        );
        self.check(false).await?;
        match self.raw(command).await {
            Ok(output) => {
                let image = line(&output)?;
                if !options::image_id(image) {
                    return Err(failure());
                }
                return Ok(image.into());
            }
            Err(error) if error.code == 1 && self.runner.cleanup_passed() => {
                self.runner.ready().await?;
            }
            Err(error) => return Err(error),
        }
        let mut pull = plan::docker(
            self.endpoint,
            &["pull", "--quiet", plan::POSTGRES_IMAGE],
            false,
        );
        // A cold pull shares the complete remaining active budget. The native
        // runner and raw() retain grace/terminal time plus the cleanup reserve.
        pull.timeout = plan::WATCHDOG;
        pull.capture = Some(4096);
        self.run(pull).await?;
        self.image(plan::POSTGRES_IMAGE, false).await
    }
    pub async fn delay(&mut self) -> Result<()> {
        self.runner.ready().await?;
        if self.clock.elapsed() + plan::POLL >= plan::WATCHDOG - plan::RESERVE {
            return Err(Failure::new(124, "Container smoke readiness timed out."));
        }
        self.clock.sleep(plan::POLL).await;
        self.runner.ready().await
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct Inspection {
    pub id: String,
    pub name: String,
    pub image: String,
    pub config: Config,
    pub state: State,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct Config {
    pub image: String,
    pub labels: BTreeMap<String, String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct State {
    pub running: bool,
}

pub(super) struct Resource {
    pub role: plan::Role,
    pub name: String,
    pub token: String,
    pub image: Option<String>,
    pub id: Option<String>,
    pub attempted: bool,
    pub creation_completed: bool,
}
impl Resource {
    pub fn new(role: plan::Role, token: &str, image: Option<String>) -> Self {
        Self {
            role,
            name: format!("simplestchat-smoke-{}-{token}", role.label()),
            token: token.into(),
            image,
            id: None,
            attempted: false,
            creation_completed: false,
        }
    }
    pub fn id(&self) -> Result<&str> {
        self.id.as_deref().ok_or_else(failure)
    }
    pub async fn inspect<E: Executor, C: Clock>(
        &mut self,
        engine: &mut Engine<'_, E, C>,
        id: &str,
        cleanup: bool,
    ) -> Result<Inspection> {
        if !options::container_id(id) || self.id.as_deref().is_some_and(|old| old != id) {
            return Err(failure());
        }
        let bytes = engine
            .run(plan::docker(
                engine.endpoint,
                &["container", "inspect", "--format", "{{json .}}", id],
                cleanup,
            ))
            .await?;
        let observed: Inspection = serde_json::from_slice(&bytes).map_err(|_| failure())?;
        let image = self.image.as_ref().ok_or_else(failure)?;
        if observed.id != id
            || observed.name != format!("/{}", self.name)
            || &observed.image != image
            || observed.config.labels.get(plan::OWNER)
                != Some(&format!("{}:{}", self.token, self.role.label()))
            || &observed.config.image != image
        {
            return Err(failure());
        }
        self.id = Some(id.into());
        Ok(observed)
    }
    pub async fn locate<E: Executor, C: Clock>(
        &mut self,
        engine: &mut Engine<'_, E, C>,
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
        let bytes = engine
            .run(plan::docker(engine.endpoint, &query, true))
            .await?;
        if bytes.is_empty() {
            // A killed/disconnected create client may have an in-flight daemon
            // request. Current absence alone cannot prove that it will not commit.
            return if self.creation_completed || self.id.is_some() {
                Ok(None)
            } else {
                Err(failure())
            };
        }
        let id = line(&bytes)?.to_owned();
        self.inspect(engine, &id, true).await?;
        Ok(Some(id))
    }
    pub async fn retire<E: Executor, C: Clock>(
        &mut self,
        engine: &mut Engine<'_, E, C>,
    ) -> Result<()> {
        let Some(id) = self.locate(engine).await? else {
            return Ok(());
        };
        engine
            .run(plan::docker(
                engine.endpoint,
                &["container", "rm", "--force", "--volumes", &id],
                true,
            ))
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
        let remaining = engine
            .run(plan::docker(engine.endpoint, &query, true))
            .await?;
        if !remaining.is_empty() {
            return Err(failure());
        }
        Ok(())
    }
}
