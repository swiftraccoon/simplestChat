//! A guest-only server and one Node journey, with an optional live Linux phase lease.

#[path = "adaptive_control.rs"]
mod adaptive;
#[cfg(test)]
#[path = "adaptive_tests.rs"]
mod adaptive_tests;

#[cfg(test)]
#[path = "browser_oracle_tests.rs"]
mod oracle_tests;
#[path = "browser_plan.rs"]
mod plan;
#[cfg(test)]
#[path = "browser_tests.rs"]
mod tests;

use super::{
    controller,
    output::Output,
    state::{Authority, Kernel, Lease, NativeAuthority},
};
use crate::bench::{
    clock::{Clock, RealClock},
    process::{Child, Command, Control, Exit, Host, NativeHost, Sink},
    sampling::{NativePorts, Ports},
};
use anyhow::{Result, ensure};
use plan::{Mode, NODE_BUDGET, Plan};
use serde::{Deserialize, Serialize};
use std::{ffi::OsString, fmt, time::Duration};

const USAGE: &str = concat!(
    "Usage: load_test bench browser-mechanics --output DIR ",
    "[--server-bin BIN] [--node BIN]"
);
const RECEIPT_LIMIT: u64 = 16 * 1024;

#[derive(Debug)]
struct Failure(u8);
impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Browser check failed.")
    }
}
impl std::error::Error for Failure {}

fn code(error: &anyhow::Error) -> u8 {
    error
        .downcast_ref::<Failure>()
        .map_or_else(|| controller::exit_code(error), |failure| failure.0)
}
fn fail(primary: &mut u8, next: u8) {
    if *primary == 0 {
        *primary = next.max(1);
    }
}
async fn check(host: &dyn Host) -> Result<()> {
    tokio::select! {
        biased;
        code = host.cancelled() => Err(Failure(code).into()),
        _ = std::future::ready(()) => Ok(()),
    }
}

struct NativeReadiness(controller::NativeReadiness);
#[async_trait::async_trait]
impl controller::Readiness for NativeReadiness {
    async fn ready(&self) -> bool {
        self.0.request("http://127.0.0.1:3109/ready").await
    }
}

struct Services<'a> {
    host: &'a dyn Host,
    clock: &'a dyn Clock,
    ports: &'a dyn Ports,
    readiness: &'a dyn controller::Readiness,
    authority: &'a dyn Authority,
}

fn present<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BrowserRecord {
    browser_name: String,
    launch_mode: String,
    completed: bool,
    passed: bool,
    graceful_close_completed: bool,
    force_kill_requested: bool,
    exit_status_observed: bool,
    #[serde(deserialize_with = "present")]
    exit_code: Option<u8>,
    #[serde(deserialize_with = "present")]
    signal_code: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Receipt {
    schema_version: u8,
    run_id: String,
    completed: bool,
    phase_passed: bool,
    browser_cleanup_passed: bool,
    #[serde(deserialize_with = "present")]
    cancelled: Option<u8>,
    browser_count: u8,
    records: Vec<BrowserRecord>,
}
impl Receipt {
    fn read(bytes: &[u8], plan: &Plan) -> Result<Self> {
        ensure!(
            bytes.len() as u64 <= RECEIPT_LIMIT,
            "Mechanics receipt limit"
        );
        let value: Self = serde_json::from_slice(bytes)
            .map_err(|_| anyhow::anyhow!("Invalid mechanics receipt"))?;
        ensure!(
            value.schema_version == 1
                && value.run_id == plan.run_id
                && value.records.len() <= 1
                && usize::from(value.browser_count) == value.records.len()
                && value.cancelled.is_none_or(|code| matches!(code, 130 | 143))
                && value
                    .records
                    .iter()
                    .all(|record| record.browser_name == "chromium"
                        && record.launch_mode == "browser-server"
                        && record.signal_code.as_ref().is_none_or(|signal| matches!(
                            signal.as_str(),
                            "SIGINT" | "SIGTERM" | "SIGKILL"
                        ))),
            "Invalid mechanics receipt"
        );
        Ok(value)
    }
    fn passed(&self) -> bool {
        self.completed
            && self.phase_passed
            && self.browser_cleanup_passed
            && self.cancelled.is_none()
            && self.browser_count == 1
            && self.records.iter().all(|record| {
                record.completed
                    && record.passed
                    && record.graceful_close_completed
                    && !record.force_kill_requested
                    && record.exit_status_observed
                    && record.exit_code == Some(0)
                    && record.signal_code.is_none()
            })
    }
}

struct Owned {
    child: Box<dyn Child>,
    control: Option<Box<dyn Control>>,
}
fn spawn(
    plan: &Plan,
    services: &Services<'_>,
    output: &Output,
    node: bool,
    uncertain: &mut bool,
) -> Result<Owned> {
    plan.verify()?;
    let required = NODE_BUDGET + plan.mode.reserve();
    ensure!(
        plan.mode.budget().as_secs_f64() * 1000.0 - services.clock.elapsed_ms()
            >= required.as_secs_f64() * 1000.0,
        Failure(124)
    );
    let log = output.file(if node { "browser.log" } else { "server.log" })?;
    let (stdout, stderr) = if node {
        (Sink::Capture, Sink::File(log))
    } else {
        let errors = match log.try_clone() {
            Ok(value) => value,
            Err(error) => {
                let _ = nix::unistd::close(log);
                return Err(error.into());
            }
        };
        (Sink::File(log), Sink::File(errors))
    };
    *uncertain = true;
    let command = Command {
        executable: if node {
            &plan.node.path
        } else {
            &plan.server.path
        }
        .clone(),
        arguments: if node {
            vec![plan.entry.to_str().unwrap().into()]
        } else {
            Vec::new()
        },
        directory: plan.root.clone(),
        environment: if node {
            plan.node_environment()
        } else {
            plan.server_environment()
        },
        timeout: if node {
            NODE_BUDGET
        } else {
            plan.mode.budget()
        },
        stdout_limit: if node {
            RECEIPT_LIMIT
        } else {
            super::plan::LOG_LIMIT
        },
        stderr_limit: super::plan::LOG_LIMIT,
        input: Vec::new(),
        stdout,
        stderr,
        cleanup: false,
    };
    let (child, control) = if node && plan.mode.adaptive() {
        let (child, channel) = services.host.spawn_controlled(command)?;
        (child, Some(channel))
    } else {
        (services.host.spawn(command)?, None)
    };
    *uncertain = false;
    Ok(Owned { child, control })
}

async fn ready(services: &Services<'_>, server: &mut Owned) -> Result<()> {
    let limit = services.clock.elapsed_ms() + 20_000.0;
    loop {
        check(services.host).await?;
        if let Some(exit) = server.child.poll().await? {
            return Err(Failure(controller::status(&exit).max(1)).into());
        }
        let remaining = limit - services.clock.elapsed_ms();
        ensure!(remaining > 0.0, Failure(124));
        let ready = tokio::select! {
            biased;
            code = services.host.cancelled() => return Err(Failure(code).into()),
            ready = tokio::time::timeout(Duration::from_millis(remaining.min(2000.0) as u64),
                services.readiness.ready()) => ready.unwrap_or(false),
        };
        if ready {
            return Ok(());
        }
        services.clock.sleep(Duration::from_millis(200)).await;
    }
}

async fn journey(
    plan: &Plan,
    services: &Services<'_>,
    server: &mut Owned,
    node: &mut Owned,
    mut session: Option<&mut adaptive::Session>,
    mut lease: Option<&mut Lease>,
) -> Result<()> {
    let limit = services.clock.elapsed_ms() + NODE_BUDGET.as_secs_f64() * 1000.0;
    loop {
        check(services.host).await?;
        ensure!(services.clock.elapsed_ms() < limit, Failure(124));
        if let Some(exit) = server.child.poll().await? {
            return Err(Failure(controller::status(&exit).max(1)).into());
        }
        let result = tokio::select! {
            biased;
            code = services.host.cancelled() => return Err(Failure(code).into()),
            result = node.child.poll() => result,
        }?;
        if let Some(exit) = result {
            ensure!(exit.success(), Failure(controller::status(&exit).max(1)));
            return Ok(());
        }
        if let Some(session) = session.as_deref_mut() {
            let request = tokio::select! {
                biased;
                code = services.host.cancelled() => return Err(Failure(code).into()),
                request = tokio::time::timeout(Duration::from_millis(50),
                    session.poll(plan, services.clock)) => match request {
                        Ok(request) => request?,
                        Err(_) => continue,
                    },
            };
            if let Some(request) = request {
                let tools = plan.kernel.as_ref().unwrap();
                let kernel = Kernel {
                    root: &plan.root,
                    environment: plan.base(),
                    tc: &tools.tc,
                    sudo: &tools.sudo,
                };
                let services = adaptive::services(services);
                let lease = lease.as_deref_mut().unwrap();
                // Once a mutation is admitted, await all its owned commands.
                // Outer signal reception updates Host; it never drops this future.
                let operation: Result<()> = async {
                    lease.clear_kernel(&kernel, &services, limit).await?;
                    if matches!(request.phase.as_str(), "baseline" | "recovery" | "finish") {
                        lease.clean(&kernel, &services, false, limit).await
                    } else {
                        let profile = adaptive::profile(&request.phase)?;
                        lease
                            .apply_profile(&kernel, &profile, &services, limit)
                            .await
                    }
                }
                .await;
                let status = operation.as_ref().err().map_or(0, controller::exit_code);
                let replied = session.reply(plan, status).await;
                operation?;
                replied?;
            } else {
                services.clock.sleep(Duration::from_millis(10)).await;
            }
        } else {
            services.clock.sleep(Duration::from_millis(50)).await;
        }
    }
}

struct Retired {
    exit: Exit,
    stdout: Vec<u8>,
}
async fn retire(owned: &mut Option<Owned>, grace: Duration) -> Result<Option<Retired>> {
    match owned.take() {
        Some(mut owned) => {
            let exit = owned.child.stop(grace).await?;
            Ok(Some(Retired {
                exit,
                stdout: owned.child.output().to_owned(),
            }))
        }
        None => Ok(None),
    }
}

async fn execute(plan: &Plan, services: &Services<'_>) -> Result<()> {
    execute_owned(plan, services)
        .await
        .map_err(|error| Failure(code(&error)).into())
}

async fn execute_owned(plan: &Plan, services: &Services<'_>) -> Result<()> {
    check(services.host).await?;
    services.ports.available(plan::PORT, plan::MEDIA_PORT, 1)?;
    let output = Output::new(&plan.output)?;
    let result = execute_inner(plan, services, &output).await;
    let closed = output.finish();
    let pending = check(services.host).await;
    result?;
    closed?;
    pending?;
    ensure!(
        services.clock.elapsed_ms() < plan.mode.budget().as_secs_f64() * 1000.0,
        Failure(124)
    );
    Ok(())
}

async fn execute_inner(plan: &Plan, services: &Services<'_>, output: &Output) -> Result<()> {
    let (mut server, mut node) = (None, None);
    let mut primary = 0;
    let mut uncertain = false;
    let mut session = None;
    let kernel_services = adaptive::services(services);
    let mut lease = if plan.mode.adaptive() {
        Some(Lease::with_limit(&kernel_services, 64)?)
    } else {
        None
    };
    let kernel = plan.kernel.as_ref().map(|tools| Kernel {
        root: &plan.root,
        environment: plan.base(),
        tc: &tools.tc,
        sudo: &tools.sudo,
    });
    let body: Result<()> = async {
        if let Some(lease) = &mut lease {
            lease
                .clean(
                    kernel.as_ref().unwrap(),
                    &kernel_services,
                    false,
                    (plan.mode.budget() - plan.mode.reserve()).as_secs_f64() * 1000.0,
                )
                .await?;
        }
        server = Some(spawn(plan, services, output, false, &mut uncertain)?);
        ready(services, server.as_mut().unwrap()).await?;
        check(services.host).await?;
        node = Some(spawn(plan, services, output, true, &mut uncertain)?);
        if let Some(channel) = node.as_mut().unwrap().control.take() {
            session = Some(adaptive::Session::new(channel));
        }
        journey(
            plan,
            services,
            server.as_mut().unwrap(),
            node.as_mut().unwrap(),
            session.as_mut(),
            lease.as_mut(),
        )
        .await
    }
    .await;
    if let Err(error) = body {
        fail(&mut primary, code(&error));
    }
    if let Err(error) = check(services.host).await {
        fail(&mut primary, code(&error));
    }
    // BrowserServer's existing close owner permits10s graceful +5s forced
    // settlement. The Node CLI gets20s before our reserved-group escalation;
    // hanging SDK promises remain unproven, never successful cleanup.
    let stopped_node = retire(&mut node, Duration::from_secs(20)).await;
    let mut process_closed = stopped_node.is_ok() && !uncertain;
    if let Err(error) = &stopped_node {
        fail(&mut primary, code(error));
    }
    let control_passed = if let Some(session) = &mut session {
        match session.settle().await {
            Ok(()) => true,
            Err(error) => {
                fail(&mut primary, code(&error));
                false
            }
        }
    } else {
        !plan.mode.adaptive()
    };
    if !control_passed {
        fail(&mut primary, 1);
    }
    // The parent capability remains alive through Node group/sink retirement.
    if let Some(session) = session.take()
        && let Err(error) = session.finish()
    {
        process_closed = false;
        fail(&mut primary, code(&error));
    }
    let mut kernel_closed = true;
    if let Some(lease) = &mut lease {
        let deadline = plan.mode.budget().as_secs_f64() * 1000.0;
        let cleared = lease
            .clear_kernel(kernel.as_ref().unwrap(), &kernel_services, deadline)
            .await;
        let observed = lease
            .clean(kernel.as_ref().unwrap(), &kernel_services, true, deadline)
            .await;
        kernel_closed = cleared.is_ok() && observed.is_ok();
        for result in [cleared, observed] {
            if let Err(error) = result {
                fail(&mut primary, controller::exit_code(&error));
            }
        }
        process_closed &= lease.process_cleanup_passed;
    }
    let stopped_server = retire(&mut server, Duration::from_secs(5)).await;
    process_closed &= stopped_server.is_ok();
    if let Err(error) = &stopped_server {
        fail(&mut primary, code(error));
    }
    for result in [&stopped_node, &stopped_server] {
        if let Ok(Some(stopped)) = result
            && !stopped.exit.success()
        {
            fail(&mut primary, controller::status(&stopped.exit));
        }
    }
    let receipt = stopped_node
        .as_ref()
        .ok()
        .and_then(Option::as_ref)
        .map(|stopped| Receipt::read(&stopped.stdout, plan));
    if let Some(Ok(receipt)) = &receipt
        && output
            .json(
                if plan.mode.adaptive() {
                    "browser-adaptive-owner.json"
                } else {
                    "browser-mechanics-owner.json"
                },
                receipt,
            )
            .is_err()
    {
        fail(&mut primary, 1);
    }
    let browser_closed = receipt
        .as_ref()
        .is_some_and(|value| value.as_ref().is_ok_and(Receipt::passed));
    if !process_closed || !browser_closed || !kernel_closed || !control_passed {
        fail(&mut primary, 1);
    }
    if plan.verify().is_err() {
        fail(&mut primary, 1);
    }
    let evidence = serde_json::json!({"schemaVersion":1,"runId":plan.run_id,
        "serverBinarySha256":plan.server.hash,"nodeBinarySha256":plan.node.hash,
        "entrySha256":plan.entry_hash,"processCleanupPassed":process_closed,
        "browserReceiptPassed":browser_closed,"kernelCleanupPassed":kernel_closed,
        "controlSequencePassed":control_passed,
        "observations":lease.as_ref().map(|lease| &lease.observations),
        "statusBeforeFinalFence":primary,
        "nodeExit":stopped_node.as_ref().ok().and_then(Option::as_ref).map(|value| &value.exit),
        "serverExit":stopped_server.as_ref().ok().and_then(Option::as_ref)
            .map(|value| &value.exit)});
    if output
        .json(
            if plan.mode.adaptive() {
                "browser-adaptive-exit.json"
            } else {
                "browser-mechanics-exit.json"
            },
            &evidence,
        )
        .is_err()
    {
        fail(&mut primary, 1);
    }
    if let Err(error) = check(services.host).await {
        fail(&mut primary, code(&error));
    }
    if services.clock.elapsed_ms() >= plan.mode.budget().as_secs_f64() * 1000.0 {
        fail(&mut primary, 124);
    }
    ensure!(primary == 0, Failure(primary));
    Ok(())
}

pub(super) fn run(arguments: &[OsString]) -> std::process::ExitCode {
    run_mode(arguments, Mode::Mechanics)
}
pub(super) fn run_adaptive(arguments: &[OsString]) -> std::process::ExitCode {
    run_mode(arguments, Mode::Adaptive)
}
fn run_mode(arguments: &[OsString], mode: Mode) -> std::process::ExitCode {
    if arguments == [OsString::from("--help")] || arguments == [OsString::from("-h")] {
        if mode.adaptive() {
            println!(concat!(
                "Usage: load_test bench browser-adaptive --output DIR ",
                "[--server-bin BIN] [--node BIN]"
            ));
        } else {
            println!("{USAGE}");
        }
        return std::process::ExitCode::SUCCESS;
    }
    let result: Result<()> = (|| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            use tokio::signal::unix::{SignalKind, signal};
            let mut interrupt = signal(SignalKind::interrupt())?;
            let mut terminate = signal(SignalKind::terminate())?;
            let host = NativeHost::default();
            let clock = RealClock::default();
            let root = std::env::current_dir()?;
            let plan = if mode.adaptive() {
                Plan::parse_mode(
                    &root,
                    arguments,
                    std::env::vars_os().collect(),
                    mode,
                    std::env::consts::OS,
                )
            } else {
                Plan::parse(&root, arguments, std::env::vars_os().collect())
            }
            .map_err(|_| Failure(2))?;
            let readiness = NativeReadiness(controller::NativeReadiness::new()?);
            let services = Services {
                host: &host,
                clock: &clock,
                ports: &NativePorts,
                readiness: &readiness,
                authority: &NativeAuthority,
            };
            let operation = execute(&plan, &services);
            tokio::pin!(operation);
            let outcome = loop {
                tokio::select! {
                    biased;
                    _ = interrupt.recv() => host.cancel(130),
                    _ = terminate.recv() => host.cancel(143),
                    outcome = &mut operation => break outcome,
                }
            };
            outcome?;
            tokio::select! {
                biased;
                _ = interrupt.recv() => host.cancel(130),
                _ = terminate.recv() => host.cancel(143),
                _ = std::future::ready(()) => {},
            }
            check(&host).await
        })
    })();
    std::process::ExitCode::from(result.as_ref().err().map_or(0, code))
}
