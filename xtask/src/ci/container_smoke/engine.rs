//! Smoke-specific names, defaults and readiness on the shared exact engine owner.

use super::{
    options::{self, Context},
    plan,
};
use crate::{
    ci::{container_engine as shared, process::Executor},
    tooling::{Failure, Result},
};
pub(super) use shared::{Clock, NativeClock};
use std::ops::{Deref, DerefMut};
pub(super) fn failure() -> Failure {
    shared::Scope::Smoke.failure()
}
pub(super) fn line(bytes: &[u8]) -> Result<&str> {
    shared::line(bytes, shared::Scope::Smoke)
}
#[cfg(test)]
pub(super) fn endpoint(value: &str) -> Result<String> {
    shared::endpoint(value, shared::Scope::Smoke)
}
pub(super) async fn discover(context: &Context, runner: &mut impl Executor) -> Result<String> {
    shared::discover(&context.environment, runner, shared::Policy::smoke()).await
}
pub(super) struct Engine<'a, E, C>(shared::Core<'a, E, C>);
impl<'a, E, C> Deref for Engine<'a, E, C> {
    type Target = shared::Core<'a, E, C>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl<E, C> DerefMut for Engine<'_, E, C> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl<'a, E: Executor, C: Clock> Engine<'a, E, C> {
    pub async fn run(&mut self, command: crate::ci::command::Command) -> Result<Vec<u8>> {
        self.0.run(command).await
    }

    pub async fn bind(endpoint: &'a str, runner: &'a mut E, clock: &'a mut C) -> Result<Self> {
        shared::Core::bind(endpoint, runner, clock, shared::Policy::smoke())
            .await
            .map(Self)
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
pub(super) struct Resource {
    pub role: plan::Role,
    owned: shared::OwnedContainer,
}
impl Deref for Resource {
    type Target = shared::OwnedContainer;
    fn deref(&self) -> &Self::Target {
        &self.owned
    }
}
impl DerefMut for Resource {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.owned
    }
}
impl Resource {
    pub fn new(role: plan::Role, token: &str, image: Option<String>) -> Self {
        Self {
            role,
            owned: shared::OwnedContainer::new(
                shared::Scope::Smoke,
                format!("simplestchat-smoke-{}-{token}", role.label()),
                plan::OWNER.into(),
                format!("{token}:{}", role.label()),
                image,
            ),
        }
    }
}
