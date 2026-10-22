//! Acquire, stop and close every owned resource before admitting generator evidence.

use super::{
    output::Output,
    plan::{self, Plan},
    state::{Authority, Lease},
};
use crate::bench::{
    clock::Clock,
    process::{Child, Command, Exit, Host, Sink},
    sampling::Ports,
    summary,
};
use anyhow::Result;
use serde::Serialize;
use serde_json::{Value, json};
use std::{fmt, time::Duration};

#[derive(Debug)]
pub(super) struct Failure {
    pub code: u8,
}
impl Failure {
    pub fn new(code: u8) -> Self {
        Self { code }
    }
}
impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Impaired generator check failed.")
    }
}
impl std::error::Error for Failure {}

#[async_trait::async_trait]
pub(super) trait Readiness: Send + Sync {
    async fn ready(&self) -> bool;
}
pub(super) struct NativeReadiness(reqwest::Client);
impl NativeReadiness {
    pub fn new() -> Result<Self> {
        Ok(Self(
            reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(2))
                .build()?,
        ))
    }
}
#[async_trait::async_trait]
impl Readiness for NativeReadiness {
    async fn ready(&self) -> bool {
        self.0
            .get("http://127.0.0.1:3129/ready")
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
    }
}
pub(super) struct Services<'a> {
    pub host: &'a dyn Host,
    pub clock: &'a dyn Clock,
    pub ports: &'a dyn Ports,
    pub readiness: &'a dyn Readiness,
    pub authority: &'a dyn Authority,
}

pub(super) async fn check(services: &Services<'_>) -> Result<()> {
    tokio::select! {
        biased;
        code = services.host.cancelled() => Err(Failure::new(code).into()),
        _ = std::future::ready(()) => Ok(()),
    }
}
pub(super) fn status(exit: &Exit) -> u8 {
    if let Some(code) = exit.code {
        return u8::try_from(code).unwrap_or(1);
    }
    match exit.signal.as_deref() {
        Some("SIGINT") => 130,
        Some("SIGTERM") => 143,
        _ => 1,
    }
}
fn fail(primary: &mut u8, code: u8) {
    if *primary == 0 {
        *primary = code.max(1);
    }
}
fn code(error: &anyhow::Error) -> u8 {
    error.downcast_ref::<Failure>().map_or_else(
        || crate::bench::process::failure_status(error).unwrap_or(1),
        |failure| failure.code,
    )
}

struct Owned {
    child: Box<dyn Child>,
    exit: Option<Exit>,
}
impl Owned {
    async fn poll(&mut self) -> Result<Option<Exit>> {
        if self.exit.is_none() {
            self.exit = self.child.poll().await?;
        }
        Ok(self.exit.clone())
    }
}

fn spawn(
    plan: &Plan,
    services: &Services<'_>,
    output: &Output,
    generator: bool,
    timeout: Duration,
    unproven_spawn: &mut bool,
) -> Result<Owned> {
    let selected = if generator {
        &plan.generator
    } else {
        &plan.server
    };
    selected.verify()?;
    let body_deadline = (plan.budget() - plan::RESERVE).as_secs_f64() * 1000.0;
    let required = if generator {
        timeout.as_secs_f64() * 1000.0
    } else {
        1000.0
    };
    if body_deadline - services.clock.elapsed_ms() < required {
        return Err(Failure::new(124).into());
    }
    let log = output.file(if generator {
        "generator.log"
    } else {
        "server.log"
    })?;
    let errors = match log.try_clone() {
        Ok(value) => value,
        Err(error) => {
            let _ = nix::unistd::close(log);
            return Err(error.into());
        }
    };
    *unproven_spawn = true;
    let child = services.host.spawn(Command {
        executable: selected.path.clone(),
        arguments: if generator {
            plan.generator_arguments()
        } else {
            Vec::new()
        },
        directory: plan.root.clone(),
        environment: if generator {
            plan.environment.clone()
        } else {
            plan.server_environment()
        },
        timeout,
        stdout_limit: plan::LOG_LIMIT,
        stderr_limit: plan::LOG_LIMIT,
        input: Vec::new(),
        stdout: Sink::File(log),
        stderr: Sink::File(errors),
        cleanup: false,
    })?;
    *unproven_spawn = false;
    Ok(Owned { child, exit: None })
}

async fn retire(owned: &mut Option<Owned>, grace: Duration) -> Result<Option<Exit>> {
    match owned.take() {
        Some(mut owned) => {
            let stopped = owned.child.stop(grace).await?;
            owned.exit = Some(stopped.clone());
            Ok(Some(stopped))
        }
        None => Ok(None),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Evidence<'a> {
    schema_version: u8,
    server_binary_sha256: &'a str,
    generator_binary_sha256: &'a str,
    generator_exit: Option<&'a Exit>,
    server_exit: Option<&'a Exit>,
    process_cleanup_passed: bool,
    kernel_cleanup_passed: bool,
    summary_admitted: bool,
    status_before_final_fence: u8,
    observations: &'a [super::state::Observation],
}

async fn ready(
    plan: &Plan,
    services: &Services<'_>,
    server: &mut Owned,
    deadline: f64,
) -> Result<()> {
    let limit = (services.clock.elapsed_ms() + 20_000.0).min(deadline);
    loop {
        check(services).await?;
        if let Some(exit) = server.poll().await? {
            return Err(Failure::new(status(&exit).max(1)).into());
        }
        let remaining = limit - services.clock.elapsed_ms();
        if remaining <= 0.0 {
            return Err(Failure::new(124).into());
        }
        let response = tokio::select! {
            biased;
            code = services.host.cancelled() => return Err(Failure::new(code).into()),
            value = tokio::time::timeout(Duration::from_millis(remaining.min(2000.0) as u64),
                services.readiness.ready()) => value.unwrap_or(false),
        };
        if response {
            return Ok(());
        }
        services.clock.sleep(Duration::from_millis(200)).await;
        plan.server.verify()?;
    }
}

async fn workload(
    plan: &Plan,
    services: &Services<'_>,
    server: &mut Owned,
    generator: &mut Owned,
    deadline: f64,
) -> Result<()> {
    let limit = (services.clock.elapsed_ms() + plan.generator_budget().as_secs_f64() * 1000.0)
        .min(deadline);
    loop {
        check(services).await?;
        if let Some(exit) = server.poll().await? {
            return Err(Failure::new(status(&exit).max(1)).into());
        }
        if services.clock.elapsed_ms() >= limit {
            return Err(Failure::new(124).into());
        }
        match generator.poll().await {
            Ok(Some(exit)) if exit.signal.is_none() && matches!(exit.code, Some(0 | 1)) => {
                return Ok(());
            }
            Ok(Some(exit)) => return Err(Failure::new(status(&exit).max(1)).into()),
            Ok(None) => services.clock.sleep(Duration::from_millis(20)).await,
            Err(error) => return Err(error),
        }
    }
}

fn admit(plan: &Plan, output: &Output, exit: Option<&Exit>) -> Result<Value> {
    plan.server.verify()?;
    plan.generator.verify()?;
    let value = output.read_summary()?;
    let summary = summary::parse(&value)?;
    anyhow::ensure!(
        summary.schema_version == 2 && summary.run.completed,
        "Incomplete impaired generator summary"
    );
    let exit = exit.ok_or_else(|| Failure::new(1))?;
    anyhow::ensure!(
        exit.signal.is_none()
            && ((exit.code == Some(0) && summary.run.passed)
                || (exit.code == Some(1) && !summary.run.passed)),
        "Unproven impaired completion"
    );
    let configuration = &summary.run.configuration;
    for (key, expected) in [
        ("numClients", json!(plan.clients)),
        ("numRooms", json!(4)),
        ("durationSecs", json!(plan.duration)),
        ("rampUpSecs", json!(65)),
        ("warmupSecs", json!(10)),
        ("publishRatio", json!(1.0)),
        ("subscriptionPlan", json!("ring-v1")),
        ("subscriptionSeed", json!(17)),
        ("serverUrl", json!("ws://127.0.0.1:3129/ws")),
        ("roomId", json!(plan.room)),
        ("maxAudioConsumers", json!(4)),
        ("maxVideoConsumers", json!(4)),
        ("runLabel", json!("impaired")),
        ("outputDir", json!(plan.output)),
    ] {
        anyhow::ensure!(
            configuration.get(key) == Some(&expected),
            "Foreign impaired summary"
        );
    }
    anyhow::ensure!(
        summary.run.provenance["generatorBinarySha256"] == plan.generator.hash,
        "Foreign impaired summary"
    );
    for (key, expected) in [
        ("audioEnabled", json!(true)),
        ("videoEnabled", json!(true)),
        ("videoWidth", json!(640)),
        ("videoHeight", json!(480)),
        ("videoFps", json!(30)),
    ] {
        anyhow::ensure!(
            configuration["mediaConfig"].get(key) == Some(&expected),
            "Foreign impaired summary"
        );
    }
    summary.impaired_projection(&value)
}

/// No resource-dependent reader runs until both owned groups and their sinks
/// retire. Files report observations, not final process/namespace-close proof.
pub(super) async fn execute(plan: &Plan, services: &Services<'_>) -> Result<Value> {
    let outcome: Result<Value> = async {
        check(services).await?;
        services.ports.available(plan::PORT, plan::MEDIA_PORT, 1)?;
        let output = Output::new(&plan.output)?;
        let result = execute_inner(plan, services, &output).await;
        let closed = output.finish();
        let pending = check(services).await;
        let value = result?;
        closed?;
        pending?;
        anyhow::ensure!(
            services.clock.elapsed_ms() < plan.budget().as_secs_f64() * 1000.0,
            Failure::new(124)
        );
        Ok(value)
    }
    .await;
    outcome.map_err(|error| Failure::new(code(&error)).into())
}

async fn execute_inner(plan: &Plan, services: &Services<'_>, output: &Output) -> Result<Value> {
    let deadline = plan.budget().as_secs_f64() * 1000.0;
    let body_deadline = deadline - plan::RESERVE.as_secs_f64() * 1000.0;
    let mut lease = Lease::new(services)?;
    let mut server = None;
    let mut generator = None;
    let mut primary = 0;
    let mut unproven_spawn = false;
    let body: Result<()> = async {
        check(services).await?;
        server = Some(spawn(
            plan,
            services,
            output,
            false,
            plan.budget(),
            &mut unproven_spawn,
        )?);
        ready(plan, services, server.as_mut().unwrap(), body_deadline).await?;
        lease.apply(plan, services, body_deadline).await?;
        check(services).await?;
        generator = Some(spawn(
            plan,
            services,
            output,
            true,
            plan.generator_budget(),
            &mut unproven_spawn,
        )?);
        workload(
            plan,
            services,
            server.as_mut().unwrap(),
            generator.as_mut().unwrap(),
            body_deadline,
        )
        .await
    }
    .await;
    if let Err(error) = body {
        fail(&mut primary, code(&error));
    }
    if let Err(error) = check(services).await {
        fail(&mut primary, code(&error));
    }
    let generator_result = retire(&mut generator, Duration::from_secs(5)).await;
    let mut closed = generator_result.is_ok() && !unproven_spawn;
    if !closed {
        fail(&mut primary, 1);
    }
    // Statistics are optional evidence, as in the reference. They remain private.
    match lease.statistics(plan, services, deadline).await {
        Ok(bytes) if output.statistics(&bytes).is_err() => fail(&mut primary, 1),
        Err(error) if !lease.process_cleanup_passed => fail(&mut primary, code(&error)),
        _ => {}
    }
    let cleared = lease.clear(plan, services, deadline).await;
    let kernel = cleared.is_ok();
    closed &= lease.process_cleanup_passed;
    if let Err(error) = cleared {
        fail(&mut primary, code(&error));
    }
    let server_result = retire(&mut server, Duration::from_secs(5)).await;
    closed &= server_result.is_ok();
    let server_exit = server_result.ok().flatten();
    if !closed || server_exit.as_ref().is_some_and(|exit| !exit.success()) {
        fail(
            &mut primary,
            server_exit.as_ref().map_or(1, |exit| status(exit).max(1)),
        );
    }
    let generator_exit = generator_result.ok().flatten();
    let projection = if primary == 0 {
        output
            .check()
            .and_then(|()| admit(plan, output, generator_exit.as_ref()))
            .and_then(|value| output.check().map(|()| value))
    } else {
        Err(Failure::new(primary).into())
    };
    if projection.is_err() {
        fail(&mut primary, 1);
    }
    if let Ok(value) = &projection
        && output.json("impaired-summary.json", value).is_err()
    {
        fail(&mut primary, 1);
    }
    let evidence = Evidence {
        schema_version: 1,
        server_binary_sha256: &plan.server.hash,
        generator_binary_sha256: &plan.generator.hash,
        generator_exit: generator_exit.as_ref(),
        server_exit: server_exit.as_ref(),
        process_cleanup_passed: closed,
        kernel_cleanup_passed: kernel,
        summary_admitted: projection.is_ok(),
        status_before_final_fence: primary,
        observations: &lease.observations,
    };
    if output.json("impaired-exit.json", &evidence).is_err() {
        fail(&mut primary, 1);
    }
    if let Err(error) = check(services).await {
        fail(&mut primary, code(&error));
    }
    if services.clock.elapsed_ms() >= deadline {
        fail(&mut primary, 124);
    }
    if primary != 0 {
        return Err(Failure::new(primary).into());
    }
    projection
}

pub(super) fn exit_code(error: &anyhow::Error) -> u8 {
    code(error)
}
