//! One owner retains all admitted children/sockets until their finalizers finish.

use super::{
    config::{self, Config, Mode, Ports},
    files::{Output, Selected},
    report::{self, Shutdown},
    wire::{Event, Socket, Wire},
};
use crate::bench::{
    clock::Clock,
    process::{self, Child, Command, Exit, Host, Retirement, Sink},
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[async_trait::async_trait]
pub(super) trait PortSource: Send + Sync {
    fn reserve(&self) -> Result<Ports>;
}
pub(super) struct Services<'a> {
    pub host: &'a dyn Host,
    pub clock: &'a dyn Clock,
    pub wire: &'a dyn Wire,
    pub ports: &'a dyn PortSource,
}
#[derive(Debug)]
pub(super) struct Failure(pub u8);
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("smoke_failed")
    }
}
impl std::error::Error for Failure {}
pub(super) fn status(error: &anyhow::Error) -> u8 {
    error
        .downcast_ref::<Failure>()
        .map(|v| v.0)
        .or_else(|| {
            process::failure_status(error).map(|v| if matches!(v, 124 | 130 | 143) { v } else { 1 })
        })
        .unwrap_or(1)
}
pub(super) async fn check(services: &Services<'_>, limit: f64) -> Result<()> {
    tokio::select! {biased;code=services.host.cancelled()=>return Err(Failure(
    if matches!(code,130|143){code}else{1}).into()),()=std::future::ready(())=>{}}
    let now = services.clock.elapsed_ms();
    ensure!(now.is_finite() && now >= 0.0, "invalid_smoke_clock");
    if now >= limit {
        return Err(Failure(124).into());
    }
    Ok(())
}
async fn bounded<T>(
    services: &Services<'_>,
    maximum: Duration,
    future: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    check(services, config::ACTIVE_MS).await?;
    let started = services.clock.elapsed_ms();
    let remaining = config::ACTIVE_MS - started;
    if !remaining.is_finite() || remaining <= 0.0 {
        return Err(Failure(124).into());
    }
    let bound = maximum.min(Duration::from_secs_f64(remaining / 1000.0));
    let value = tokio::select! {biased;
        code=services.host.cancelled()=>return Err(Failure(code).into()),
        result=tokio::time::timeout(bound,future)=>result.map_err(|_|Failure(124))??,
    };
    check(services, config::ACTIVE_MS).await?;
    if services.clock.elapsed_ms() - started >= maximum.as_secs_f64() * 1000.0 {
        return Err(Failure(124).into());
    }
    Ok(value)
}
async fn running(child: &mut dyn Child, exit: &mut Option<Exit>) -> Result<()> {
    if let Some(value) = child.poll().await? {
        *exit = Some(value);
        return Err(Failure(1).into());
    }
    Ok(())
}
fn command(config: &Config, ports: Ports) -> Command {
    Command {
        executable: config.binary.clone(),
        arguments: vec![],
        directory: config.directory.clone(),
        environment: config.server_environment(ports),
        timeout: Duration::from_secs(150),
        stdout_limit: config::LOG_BYTES,
        stderr_limit: config::LOG_BYTES,
        input: vec![],
        stdout: Sink::Capture,
        stderr: Sink::Capture,
        cleanup: false,
    }
}
struct State {
    child: Option<Box<dyn Child>>,
    sockets: Vec<Box<dyn Socket>>,
    exit: Option<Exit>,
    stopped: bool,
    stop_ok: bool,
    retirement: Option<Retirement>,
}
impl State {
    fn new() -> Self {
        Self {
            child: None,
            sockets: vec![],
            exit: None,
            stopped: false,
            stop_ok: false,
            retirement: None,
        }
    }
    fn retain_exit(&mut self, observed: Option<Exit>) {
        if self.exit.is_none() {
            self.exit = observed.or_else(|| {
                self.retirement.as_ref().and_then(|facts| {
                    (facts.wait_code.is_some() || facts.wait_signal.is_some()).then(|| Exit {
                        code: facts.wait_code,
                        signal: facts.wait_signal.map(|signal| {
                            nix::sys::signal::Signal::try_from(signal)
                                .map(|value| format!("{value:?}"))
                                .unwrap_or_else(|_| "UNKNOWN".into())
                        }),
                    })
                })
            });
        }
    }
    async fn retire(&mut self) -> Result<()> {
        let Some(child) = self.child.as_mut() else {
            return Err(Failure(1).into());
        };
        if !self.stopped {
            let result = child.stop(config::DRAIN).await;
            self.stopped = true;
            self.stop_ok = result.is_ok();
            self.retirement = child.retirement();
            self.retain_exit(result.as_ref().ok().cloned());
            result?;
        }
        ensure!(
            self.stop_ok
                && self
                    .retirement
                    .as_ref()
                    .is_some_and(|v| v.group_cleanup_passed && v.streams_closed),
            "smoke_cleanup_unconfirmed"
        );
        Ok(())
    }
}
async fn events(
    services: &Services<'_>,
    socket: &mut dyn Socket,
    wanted: &str,
    maximum: Duration,
) -> Result<(u16, bool)> {
    let start = services.clock.elapsed_ms();
    let mut restarting = false;
    loop {
        if services.clock.elapsed_ms() - start >= maximum.as_secs_f64() * 1000.0 {
            return Err(Failure(124).into());
        }
        let left = maximum.saturating_sub(Duration::from_secs_f64(
            ((services.clock.elapsed_ms() - start) / 1000.0).max(0.0),
        ));
        match bounded(services, left, socket.next()).await? {
            Event::Joined if wanted == "join" => return Ok((0, false)),
            Event::Restarting => restarting = true,
            Event::Closed { code, clean } if wanted == "peer" => {
                ensure!(code == 1000 && clean, "smoke_peer_close_failed");
                return Ok((code, false));
            }
            Event::Closed { code, clean } if wanted == "drain" => {
                ensure!(code == 1001 && clean && restarting, "smoke_drain_failed");
                return Ok((code, true));
            }
            Event::Closed { .. } => anyhow::bail!("smoke_early_close"),
            _ => {}
        }
    }
}
async fn connect(services: &Services<'_>, state: &mut State, ports: Ports) -> Result<()> {
    bounded(services, config::START, async {
        let socket = services.wire.connect(ports).await?;
        // Retain every completed acquisition before any post-await fence.
        state.sockets.push(socket);
        Ok(())
    })
    .await
}
async fn work(
    config: &Config,
    services: &Services<'_>,
    output: &mut Output,
    selected: &Selected,
    state: &mut State,
) -> Result<Shutdown> {
    check(services, config::ACTIVE_MS).await?;
    output.check()?;
    selected.check()?;
    let ports = services.ports.reserve()?.validate()?;
    state.child = Some(services.host.spawn(command(config, ports))?);
    let start = services.clock.elapsed_ms();
    loop {
        running(state.child.as_mut().unwrap().as_mut(), &mut state.exit).await?;
        if services.clock.elapsed_ms() - start >= 20_000.0 {
            return Err(Failure(124).into());
        }
        let ready = bounded(services, config::START, services.wire.ready(ports)).await?;
        running(state.child.as_mut().unwrap().as_mut(), &mut state.exit).await?;
        if services.clock.elapsed_ms() - start >= 20_000.0 {
            return Err(Failure(124).into());
        }
        if ready {
            break;
        }
        bounded(services, config::START, async {
            services.clock.sleep(Duration::from_millis(25)).await;
            Ok(())
        })
        .await?;
    }
    if config.mode == Mode::Diagnostics {
        let _ = output.observe_recorder();
    }
    connect(services, state, ports).await?;
    let (peer, _) = bounded(services, Duration::from_secs(5), async {
        state.sockets[0].peer_close().await?;
        events(
            services,
            state.sockets[0].as_mut(),
            "peer",
            Duration::from_secs(5),
        )
        .await
    })
    .await?;
    let peer = state.sockets.pop().unwrap().finish().await.map(|()| peer)?;
    running(state.child.as_mut().unwrap().as_mut(), &mut state.exit).await?;
    connect(services, state, ports).await?;
    bounded(services, config::START, async {
        state.sockets[0].join(&config.room).await?;
        events(services, state.sockets[0].as_mut(), "join", config::START).await
    })
    .await?;
    running(state.child.as_mut().unwrap().as_mut(), &mut state.exit).await?;
    check(services, config::ACTIVE_MS).await?;
    let started = services.clock.elapsed_ms();
    // Never select-drop the stop future when wire I/O or cancellation fails.
    let (wire, stopped) = tokio::join!(
        events(services, state.sockets[0].as_mut(), "drain", config::DRAIN),
        state.child.as_mut().unwrap().stop(config::DRAIN)
    );
    state.stopped = true;
    state.stop_ok = stopped.is_ok();
    state.retirement = state.child.as_ref().unwrap().retirement();
    state.retain_exit(stopped.as_ref().ok().cloned());
    let exit = match stopped {
        Ok(exit) => exit,
        Err(error) => {
            if let Err(wire_error) = wire
                && matches!(status(&wire_error), 124 | 130 | 143)
            {
                return Err(wire_error);
            }
            return Err(error);
        }
    };
    ensure!(exit.success(), "smoke_server_exit_failed");
    let (close, restarting) = wire?;
    let elapsed = services.clock.elapsed_ms() - started;
    ensure!(elapsed.is_finite() && elapsed >= 0.0, "invalid_smoke_clock");
    if elapsed >= 20_000.0 {
        return Err(Failure(124).into());
    }
    check(services, config::ACTIVE_MS).await?;
    Ok(Shutdown {
        ready: true,
        peer_close_code: peer,
        joined: true,
        server_restarting: restarting,
        close_code: close,
        exit_code: 0,
        shutdown_ms: elapsed,
        ports,
    })
}

async fn read_report(services: &Services<'_>, output: &mut Output) -> Result<Value> {
    check(services, config::ACTIVE_MS).await?;
    let recorder = output.take_recorder()?;
    let left = (config::ACTIVE_MS - services.clock.elapsed_ms()).min(30_000.0);
    if left <= 0.0 {
        let _ = recorder.finish();
        return Err(Failure(124).into());
    }
    let deadline = Instant::now() + Duration::from_secs_f64(left / 1000.0);
    let abort = Arc::new(AtomicBool::new(false));
    let worker_abort = abort.clone();
    let mut worker = tokio::task::spawn_blocking(move || {
        recorder.report(|| !worker_abort.load(Ordering::SeqCst) && Instant::now() < deadline)
    });
    let mut primary: Result<()> = Ok(());
    let joined = loop {
        tokio::select! {
            result=&mut worker=>break result.map_err(|_|anyhow::anyhow!("smoke_reader_worker")),
            _=tokio::time::sleep(Duration::from_millis(10))=>{
                let pending=check(services,config::ACTIVE_MS).await;
                let timely=if Instant::now()>=deadline{Err(Failure(124).into())}else{Ok(())};
                primary=primary.and(pending).and(timely);
                if primary.is_err(){abort.store(true,Ordering::SeqCst);}
            }
        }
    };
    let pending = check(services, config::ACTIVE_MS).await;
    let checked = output.check();
    primary?;
    let report = joined??;
    pending?;
    checked?;
    if Instant::now() >= deadline {
        return Err(Failure(124).into());
    }
    Ok(report)
}
#[derive(serde::Serialize)]
pub(super) struct Outcome {
    pub code: u8,
    pub shutdown: Option<Shutdown>,
    pub diagnostic: Option<Value>,
}
pub(super) async fn execute(config: &Config, services: &Services<'_>) -> Result<Outcome> {
    check(services, config::ACTIVE_MS).await?;
    let selected = Selected::open(&config.binary)?;
    let mut output = match Output::new(config) {
        Ok(v) => v,
        Err(error) => {
            let _ = selected.finish();
            return Err(error);
        }
    };
    let mut state = State::new();
    let primary = work(config, services, &mut output, &selected, &mut state).await;
    let retired = state.retire().await;
    let mut socket_result = Ok(());
    for socket in state.sockets.drain(..) {
        let closed = socket.finish().await;
        socket_result = socket_result.and(closed);
    }
    let mut logs = Vec::new();
    if let Some(child) = state.child.take() {
        logs.extend_from_slice(child.output());
        logs.extend_from_slice(child.errors());
        drop(child);
    }
    let selected_closed = selected.finish();
    let mut code = primary.as_ref().err().map(status).unwrap_or(0);
    let shutdown = primary.ok();
    let cleanup = retired.is_ok() && socket_result.is_ok() && selected_closed.is_ok();
    if code == 0
        && let Err(error) = &retired
    {
        code = status(error);
    }
    if code == 0 && !cleanup {
        code = 1
    }
    let pending = check(services, config::ACTIVE_MS).await;
    if code == 0
        && let Err(error) = &pending
    {
        code = status(error)
    }
    let mut issues = Vec::new();
    let mut diagnostic = None;
    if config.mode == Mode::Diagnostics {
        if !cleanup {
            issues.push("server_not_stopped")
        }
        if matches!(code, 130 | 143) {
            issues.push("smoke_cancelled")
        }
        if cleanup && pending.is_ok() {
            match read_report(services, &mut output).await {
                Ok(value) => {
                    if output
                        .json("server-diagnostics-report.json", &value)
                        .is_err()
                    {
                        issues.push("diagnostic_report_write_failed")
                    };
                    diagnostic = Some(value);
                }
                Err(error) => {
                    if code == 0 && matches!(status(&error), 124 | 130 | 143) {
                        code = status(&error)
                    }
                    issues.push("diagnostic_read_failed");
                }
            }
        }
    }
    if code != 0 && !logs.is_empty() && output.log(&logs).is_err() {
        issues.push("server_log_write_failed")
    }
    let summary = if config.mode == Mode::Diagnostics {
        report::diagnostic(
            shutdown.as_ref(),
            state.exit.as_ref(),
            diagnostic.as_ref(),
            &issues,
            &config.output,
            code == 0,
        )
    } else {
        shutdown
            .as_ref()
            .and_then(|v| serde_json::to_value(v).ok())
            .unwrap_or(Value::Null)
    };
    let mut summary = summary;
    if config.mode == Mode::Diagnostics && summary["passed"] != true && code == 0 {
        code = 1
    }
    let evidence = json!({"schemaVersion":1,"status":code,"groupAndStreamsRetired":cleanup,
        "retirement":state.retirement,"serverExit":state.exit,
        "scope":"reserved_group_signals_leader_reap_and_owned_streams_not_escaped_descendants"});
    let stored = output
        .json("shutdown-retirement.json", &evidence)
        .and(output.json(
            if config.mode == Mode::Diagnostics {
                "diagnostics-smoke.json"
            } else {
                "shutdown-smoke.json"
            },
            &summary,
        ));
    if code == 0 && stored.is_err() {
        code = 1
    }
    if let Err(error) = check(services, config::FINAL_MS).await {
        if code == 0 {
            code = status(&error)
        }
        if config.mode == Mode::Diagnostics && summary["passed"] == true {
            summary["passed"] = false.into();
            if matches!(status(&error), 130 | 143) {
                summary["artifactIssues"]
                    .as_array_mut()
                    .unwrap()
                    .push("smoke_cancelled".into());
            }
            let _ = output.withdraw(&summary);
        }
    }
    let closed = output.finish();
    if code == 0 && closed.is_err() {
        code = 1
    }
    if code == 0
        && let Err(error) = check(services, config::FINAL_MS).await
    {
        code = status(&error)
    }
    Ok(Outcome {
        code,
        shutdown,
        diagnostic: if config.mode == Mode::Diagnostics {
            Some(summary)
        } else {
            None
        },
    })
}
