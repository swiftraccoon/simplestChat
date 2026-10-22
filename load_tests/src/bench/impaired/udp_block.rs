//! Live disposable-interface UDP rule lease. Names/comments and snapshots fence
//! observed changes; they are not authenticated kernel IDs or an atomic CAS.

#[cfg(test)]
#[path = "udp_block_oracle_tests.rs"]
mod oracle_tests;
#[path = "udp_block_policy.rs"]
pub(super) mod policy;
#[cfg(test)]
#[path = "udp_block_tests.rs"]
mod tests;

use super::super::{
    controller::{self, Failure},
    output::Output,
    state::Identity,
};
use super::{Services, plan::Plan};
use crate::bench::process::{self, Command, Sink};
use anyhow::{Result, ensure};
use policy::{Rules, Table};
use serde::Serialize;
use std::{fs::File, os::unix::fs::MetadataExt, time::Duration};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Observation {
    action: &'static str,
    stage: u8,
    chains: usize,
    rules: usize,
}

pub(super) struct Lease {
    rules: Rules,
    identity: Identity,
    namespace: Option<File>,
    baseline: Option<Table>,
    backend: Option<&'static str>,
    tool_hash: String,
    allowed: (u8, u8),
    observed: u8,
    tainted: bool,
    attempted: bool,
    pub process_cleanup_passed: bool,
    observations: Vec<Observation>,
}
impl Lease {
    pub fn new(plan: &Plan, services: &Services<'_>) -> Result<Self> {
        let rules = Rules::new(&plan.run_id)?;
        let identity = services.authority.observe()?;
        let namespace = services.authority.pin()?;
        let admitted = namespace.as_ref().is_none_or(|file| {
            file.metadata()
                .is_ok_and(|stamp| (stamp.dev(), stamp.ino()) == identity.namespace)
        });
        if !admitted {
            if let Some(file) = namespace {
                let _ = nix::unistd::close(file);
            }
            anyhow::bail!("UDP namespace admission failed");
        }
        Ok(Self {
            rules,
            identity,
            namespace,
            baseline: None,
            backend: None,
            tool_hash: plan
                .kernel
                .as_ref()
                .unwrap()
                .iptables
                .as_ref()
                .unwrap()
                .hash
                .clone(),
            allowed: (0, 0),
            observed: 0,
            tainted: false,
            attempted: false,
            process_cleanup_passed: true,
            observations: Vec::new(),
        })
    }
    fn binding(&self, plan: &Plan, services: &Services<'_>) -> Result<()> {
        ensure!(
            services.authority.observe()? == self.identity,
            "UDP namespace changed"
        );
        if let Some(file) = &self.namespace {
            let stamp = file.metadata()?;
            ensure!(
                (stamp.dev(), stamp.ino()) == self.identity.namespace,
                "UDP namespace changed"
            );
        }
        let tools = plan.kernel.as_ref().unwrap();
        tools.sudo.verify()?;
        tools.iptables.as_ref().unwrap().verify()?;
        Ok(())
    }
    async fn call(
        &mut self,
        plan: &Plan,
        services: &Services<'_>,
        arguments: Vec<String>,
        cleanup: bool,
        deadline: f64,
    ) -> Result<Vec<u8>> {
        self.binding(plan, services)?;
        if !cleanup {
            super::check(services.host).await?;
        }
        let remaining = deadline - services.clock.elapsed_ms();
        ensure!(remaining > 26000.0, Failure::new(124));
        let tools = plan.kernel.as_ref().unwrap();
        let executable = tools.iptables.as_ref().unwrap();
        let mut argv = vec![
            "-n".into(),
            "--".into(),
            executable
                .path
                .to_str()
                .ok_or_else(|| Failure::new(2))?
                .into(),
        ];
        argv.extend(arguments);
        let prior = self.process_cleanup_passed;
        self.process_cleanup_passed = false;
        let result = process::run(
            services.host,
            Command {
                executable: tools.sudo.path.clone(),
                arguments: argv,
                directory: plan.root.clone(),
                environment: plan.base(),
                timeout: Duration::from_millis((remaining - 25000.0).min(5000.0) as u64),
                stdout_limit: policy::LIMIT,
                stderr_limit: 64 * 1024,
                input: Vec::new(),
                stdout: Sink::Capture,
                stderr: Sink::Capture,
                cleanup,
            },
        )
        .await;
        let output = result?;
        self.process_cleanup_passed = prior;
        ensure!(
            output.exit.success(),
            Failure::new(controller::status(&output.exit).max(1))
        );
        ensure!(
            output.stderr.is_empty(),
            "UDP filter diagnostic unavailable"
        );
        self.binding(plan, services)?;
        if !cleanup {
            super::check(services.host).await?;
        }
        Ok(output.stdout)
    }
    fn arguments(arguments: Vec<String>) -> Vec<String> {
        let mut values = ["-w", "2", "-t", "filter"].map(str::to_owned).to_vec();
        values.extend(arguments);
        values
    }
    async fn listing(
        &mut self,
        plan: &Plan,
        services: &Services<'_>,
        cleanup: bool,
        deadline: f64,
    ) -> Result<Table> {
        let bytes = self
            .call(
                plan,
                services,
                Self::arguments(vec!["-S".into()]),
                cleanup,
                deadline,
            )
            .await?;
        Table::parse(&bytes)
    }
    pub async fn prepare(
        &mut self,
        plan: &Plan,
        services: &Services<'_>,
        deadline: f64,
    ) -> Result<()> {
        let version = self
            .call(plan, services, vec!["--version".into()], false, deadline)
            .await?;
        self.backend = Some(policy::backend(&version)?);
        let table = self.listing(plan, services, false, deadline).await?;
        let (baseline, stage) = self.rules.project(table)?;
        ensure!(stage == 0, "Pre-existing UDP owner state");
        let counts = baseline.counts();
        self.baseline = Some(baseline);
        self.observations.push(Observation {
            action: "baseline",
            stage: 0,
            chains: counts.0,
            rules: counts.1,
        });
        Ok(())
    }
    async fn inspect(
        &mut self,
        plan: &Plan,
        services: &Services<'_>,
        cleanup: bool,
        action: &'static str,
        deadline: f64,
    ) -> Result<u8> {
        let table = self.listing(plan, services, cleanup, deadline).await?;
        let counts = table.counts();
        let admitted: Result<u8> = (|| {
            let (stripped, stage) = self.rules.project(table)?;
            ensure!(
                self.baseline
                    .as_ref()
                    .is_some_and(|baseline| *baseline == stripped),
                "UDP filter baseline changed"
            );
            ensure!(
                stage == 0 && cleanup || (self.allowed.0..=self.allowed.1).contains(&stage),
                "UDP filter state changed"
            );
            Ok(stage)
        })();
        let stage = match admitted {
            Ok(stage) => stage,
            Err(error) => {
                self.tainted = true;
                return Err(error);
            }
        };
        ensure!(self.observations.len() < 40, "UDP filter observation limit");
        self.observations.push(Observation {
            action,
            stage,
            chains: counts.0,
            rules: counts.1,
        });
        self.observed = stage;
        self.allowed = (stage, stage);
        Ok(stage)
    }
    pub async fn install(
        &mut self,
        plan: &Plan,
        services: &Services<'_>,
        output: &Output,
        deadline: f64,
    ) -> Result<()> {
        ensure!(
            !self.attempted && !self.tainted,
            "UDP filter installation already attempted"
        );
        ensure!(
            self.inspect(plan, services, false, "before-install", deadline)
                .await?
                == 0,
            "Pre-existing UDP owner state"
        );
        output.json(
            "browser-tcp-intent.json",
            &serde_json::json!({"schemaVersion":1,
            "runId":plan.run_id,"chain":self.rules.chain,
                "namespaceDevice":self.identity.namespace.0,
            "namespaceInode":self.identity.namespace.1,"loopbackIfindex":self.identity.interface,
            "baselineSha256":self.baseline.as_ref().unwrap().signature()}),
        )?;
        self.attempted = true;
        for stage in 1..=4 {
            ensure!(
                !self.tainted && self.observed + 1 == stage,
                "UDP filter state changed"
            );
            self.allowed = (stage - 1, stage);
            let mutation = self
                .call(
                    plan,
                    services,
                    Self::arguments(self.rules.mutation(stage, false)),
                    false,
                    deadline,
                )
                .await;
            let readback = self
                .inspect(plan, services, true, "install", deadline)
                .await;
            mutation?;
            ensure!(readback? == stage, "UDP filter installation unproven");
        }
        Ok(())
    }
    pub async fn held(
        &mut self,
        plan: &Plan,
        services: &Services<'_>,
        deadline: f64,
    ) -> Result<()> {
        ensure!(
            self.inspect(plan, services, false, "held", deadline)
                .await?
                == 4
                && !self.tainted,
            "UDP filter block unproven"
        );
        Ok(())
    }
    /// Inspect before every exact deletion. No flush or received rule text is
    /// executed. A still-present dependency or foreign state prevents later
    /// deletion; a lost response may still be reconciled by the next readback.
    pub async fn clear(
        &mut self,
        plan: &Plan,
        services: &Services<'_>,
        deadline: f64,
    ) -> Result<()> {
        if self.baseline.is_none() {
            ensure!(!self.attempted, "UDP cleanup unavailable");
            return Ok(());
        }
        let mut primary = None;
        for stage in (1..=4).rev() {
            let observed = match self
                .inspect(plan, services, true, "before-remove", deadline)
                .await
            {
                Ok(value) => value,
                Err(error) => {
                    if primary.is_none() {
                        primary = Some(error);
                    }
                    break;
                }
            };
            if observed == 0 {
                break;
            }
            if self.tainted || observed > stage {
                if primary.is_none() {
                    primary = Some(anyhow::anyhow!("UDP cleanup unproven"));
                }
                break;
            }
            if observed < stage {
                continue;
            }
            self.allowed = (stage - 1, stage);
            if let Err(error) = self
                .call(
                    plan,
                    services,
                    Self::arguments(self.rules.mutation(stage, true)),
                    true,
                    deadline,
                )
                .await
                && primary.is_none()
            {
                primary = Some(error);
            }
        }
        let final_state = self.absent(plan, services, deadline).await;
        if let Some(error) = primary {
            return Err(error);
        }
        final_state
    }
    pub async fn absent(
        &mut self,
        plan: &Plan,
        services: &Services<'_>,
        deadline: f64,
    ) -> Result<()> {
        ensure!(
            self.inspect(plan, services, true, "absent", deadline)
                .await?
                == 0,
            "UDP filter cleanup unproven"
        );
        Ok(())
    }
    pub fn close(&mut self, plan: &Plan, services: &Services<'_>) -> Result<()> {
        let bound = self.binding(plan, services);
        let closed = self.namespace.take().map(nix::unistd::close).transpose();
        bound?;
        closed?;
        Ok(())
    }
    pub fn evidence(&self) -> serde_json::Value {
        serde_json::json!({"backend":self.backend,"iptablesSha256":self.tool_hash,
            "baselineSha256":self.baseline.as_ref()
            .map(Table::signature),"baselineCounts":self.baseline.as_ref().map(Table::counts),
            "attempted":self.attempted,"processCleanupPassed":self.process_cleanup_passed,
            "observations":self.observations})
    }
}
