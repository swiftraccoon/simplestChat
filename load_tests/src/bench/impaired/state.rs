//! One exclusive disposable-interface lease; signatures are observations, not capabilities.

use super::{
    controller::{Failure, Services},
    netem,
    plan::{self, Plan},
};
use crate::bench::process::{self, Command, Sink};
use anyhow::{Result, ensure};
use serde::Serialize;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Identity {
    pub namespace: (u64, u64),
    pub interface: u32,
}

pub(super) trait Authority: Send + Sync {
    fn observe(&self) -> Result<Identity>;
}

pub(super) struct NativeAuthority;
impl Authority for NativeAuthority {
    fn observe(&self) -> Result<Identity> {
        let namespace = File::open("/proc/self/ns/net")?;
        let stamp = namespace.metadata();
        let closed = nix::unistd::close(namespace);
        let stamp = stamp?;
        closed?;
        let mut interface = File::open("/sys/class/net/lo/ifindex")?;
        let mut bytes = Vec::new();
        let read = Read::by_ref(&mut interface)
            .take(65)
            .read_to_end(&mut bytes);
        let closed = nix::unistd::close(interface);
        read?;
        closed?;
        ensure!(bytes.len() <= 64, "Invalid loopback identity");
        let interface = std::str::from_utf8(&bytes)?.trim().parse::<u32>()?;
        ensure!(interface != 0, "Invalid loopback identity");
        Ok(Identity {
            namespace: (stamp.dev(), stamp.ino()),
            interface,
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Observation {
    action: &'static str,
    state: serde_json::Value,
}

pub(super) struct Lease {
    identity: Identity,
    attempted: bool,
    installed: bool,
    signature: Option<String>,
    pub observations: Vec<Observation>,
    pub process_cleanup_passed: bool,
}

impl Lease {
    pub fn new(services: &Services<'_>) -> Result<Self> {
        Ok(Self {
            identity: services.authority.observe()?,
            attempted: false,
            installed: false,
            signature: None,
            observations: Vec::new(),
            process_cleanup_passed: true,
        })
    }

    fn binding(&self, services: &Services<'_>) -> Result<()> {
        ensure!(
            services.authority.observe()? == self.identity,
            "Loopback identity changed"
        );
        Ok(())
    }

    async fn call(
        &mut self,
        plan: &Plan,
        services: &Services<'_>,
        arguments: Vec<String>,
        mutation: bool,
        cleanup: bool,
        deadline: f64,
    ) -> Result<Vec<u8>> {
        self.binding(services)?;
        plan.tc.verify()?;
        if mutation {
            plan.sudo.verify()?;
        }
        if !cleanup {
            super::controller::check(services).await?;
        }
        let remaining = deadline - services.clock.elapsed_ms();
        // process::run retires/reaps/drains after observing the body. Reserve
        // its5s grace plus20s conservative terminal/stream settlement first.
        if remaining <= 26_000.0 {
            return Err(Failure::new(124).into());
        }
        let timeout = Duration::from_millis((remaining - 25_000.0).min(5000.0) as u64);
        let (executable, arguments) = if mutation {
            let mut args = vec![
                "-n".into(),
                "--".into(),
                plan.tc.path.to_str().ok_or_else(|| Failure::new(2))?.into(),
            ];
            args.extend(arguments);
            (plan.sudo.path.clone(), args)
        } else {
            (plan.tc.path.clone(), arguments)
        };
        let prior = self.process_cleanup_passed;
        self.process_cleanup_passed = false;
        let outcome = process::run(
            services.host,
            Command {
                executable,
                arguments,
                directory: plan.root.clone(),
                environment: plan.base(),
                timeout,
                stdout_limit: plan::CLI_LIMIT,
                stderr_limit: plan::CLI_LIMIT,
                input: Vec::new(),
                stdout: Sink::Capture,
                stderr: Sink::Capture,
                cleanup,
            },
        )
        .await;
        let output = match outcome {
            Ok(output) => output,
            Err(error) => {
                // run has already attempted retirement. Only its observed
                // typed outcome takes precedence over a later cleanup error.
                if let Some(code) = process::failure_status(&error) {
                    return Err(Failure::new(code).into());
                }
                return Err(error);
            }
        };
        self.process_cleanup_passed = prior;
        if !output.exit.success() {
            return Err(Failure::new(super::controller::status(&output.exit).max(1)).into());
        }
        self.binding(services)?;
        if !cleanup {
            super::controller::check(services).await?;
        }
        Ok(output.stdout)
    }

    async fn inspect(
        &mut self,
        plan: &Plan,
        services: &Services<'_>,
        action: &'static str,
        cleanup: bool,
        deadline: f64,
    ) -> Result<netem::State> {
        let mut values = Vec::new();
        for kind in ["qdisc", "filter"] {
            values.push(
                self.call(
                    plan,
                    services,
                    ["-j", kind, "show", "dev", "lo"]
                        .map(str::to_owned)
                        .to_vec(),
                    false,
                    cleanup,
                    deadline,
                )
                .await?,
            );
        }
        let state = netem::inspect(&values[0], &values[1], &plan.profile)?;
        ensure!(self.observations.len() < 8, "Impairment observation limit");
        self.observations.push(Observation {
            action,
            state: state.evidence.clone(),
        });
        Ok(state)
    }

    pub async fn apply(
        &mut self,
        plan: &Plan,
        services: &Services<'_>,
        deadline: f64,
    ) -> Result<()> {
        let before = self
            .inspect(plan, services, "before-apply", false, deadline)
            .await?;
        ensure!(
            !self.attempted && before.clean,
            "Pre-existing impairment state"
        );
        self.attempted = true;
        let mut failure = None;
        for arguments in plan.profile.additions() {
            if let Err(error) = self
                .call(plan, services, arguments, true, false, deadline)
                .await
            {
                failure = Some(error);
                break;
            }
        }
        self.installed = failure.is_none();
        let after = self
            .inspect(plan, services, "after-apply", false, deadline)
            .await;
        if let Ok(after) = &after {
            if after.owned_shape {
                self.signature = Some(after.signature.clone());
            }
            if after.clean {
                self.attempted = false;
            }
        }
        if let Some(failure) = failure {
            return Err(failure);
        }
        ensure!(after?.complete, "Impairment installation unproven");
        Ok(())
    }

    /// A clean readback is distinct from CLI-group retirement. Under the
    /// exclusive-owner assumption, only our observed unchanged shape is removed.
    pub async fn clear(
        &mut self,
        plan: &Plan,
        services: &Services<'_>,
        deadline: f64,
    ) -> Result<()> {
        if !self.attempted {
            return Ok(());
        }
        let before = self
            .inspect(plan, services, "before-clear", true, deadline)
            .await?;
        if before.clean {
            self.attempted = false;
            return Ok(());
        }
        ensure!(
            before.owned_shape
                && (!self.installed || before.complete)
                && self
                    .signature
                    .as_ref()
                    .is_none_or(|value| value == &before.signature),
            "Observed impairment state changed"
        );
        let removed = self
            .call(
                plan,
                services,
                ["qdisc", "del", "dev", "lo", "root", "handle", "1:"]
                    .map(str::to_owned)
                    .to_vec(),
                true,
                true,
                deadline,
            )
            .await;
        let after = self
            .inspect(plan, services, "after-clear", true, deadline)
            .await;
        if after.as_ref().is_ok_and(|state| state.clean) {
            self.attempted = false;
        }
        removed?;
        ensure!(after?.clean, "Impairment cleanup unproven");
        Ok(())
    }

    pub async fn statistics(
        &mut self,
        plan: &Plan,
        services: &Services<'_>,
        deadline: f64,
    ) -> Result<Vec<u8>> {
        self.call(
            plan,
            services,
            ["-s", "qdisc", "show", "dev", "lo"]
                .map(str::to_owned)
                .to_vec(),
            false,
            true,
            deadline,
        )
        .await
    }
}
