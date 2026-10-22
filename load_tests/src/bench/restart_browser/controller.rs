//! One inline transition future owns any admitted server. Failure starts Node
//! retirement concurrently with that future's settlement; neither is detached.
use super::{
    config::{self, Config},
    files::Directory,
    protocol::{Request, Session},
    report::Receipt,
};
use crate::bench::{
    clock::Clock,
    process::{self, Child, Command, Exit, Host, Retirement, Sink},
    smoke::config::Ports,
};
use anyhow::{Result, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

#[derive(Debug)]
pub(super) struct Failure(pub u8);
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("restart_browser_failed")
    }
}
impl std::error::Error for Failure {}
pub(super) fn status(error: &anyhow::Error) -> u8 {
    error
        .downcast_ref::<Failure>()
        .map(|v| v.0)
        .or_else(|| process::failure_status(error))
        .map(|v| {
            if matches!(v, 2 | 124 | 130 | 143) {
                v
            } else {
                1
            }
        })
        .unwrap_or(1)
}
fn node_failure(exit: &Exit) -> Failure {
    Failure(match exit.code {
        Some(code @ (124 | 130 | 143)) => code as u8,
        None => match exit.signal.as_deref() {
            Some("SIGINT") => 130,
            Some("SIGTERM") => 143,
            _ => 1,
        },
        _ => 1,
    })
}
fn fail(code: &mut u8, next: u8) {
    if *code == 0 {
        *code = next.max(1);
    }
}
#[async_trait::async_trait]
pub(super) trait Readiness: Send + Sync {
    async fn ready(&self, ports: Ports) -> Result<bool>;
}
pub(super) trait PortSource: Send + Sync {
    fn reserve(&self) -> Result<Ports>;
}
pub(super) struct Services<'a> {
    pub host: &'a dyn Host,
    pub clock: &'a dyn Clock,
    pub readiness: &'a dyn Readiness,
    pub ports: &'a dyn PortSource,
}
async fn check(s: &Services<'_>, limit: f64) -> Result<()> {
    tokio::select! {biased;code=s.host.cancelled()=>return Err(Failure(code).into()),
    ()=std::future::ready(())=>{}}
    let elapsed = s.clock.elapsed_ms();
    ensure!(
        elapsed.is_finite() && elapsed >= 0.0,
        "invalid_restart_clock"
    );
    if elapsed >= limit {
        return Err(Failure(124).into());
    }
    Ok(())
}
fn command(cfg: &Config, ports: Ports, node: bool) -> Command {
    Command {
        executable: if node {
            cfg.node_path.clone()
        } else {
            cfg.guest.binary.clone()
        },
        arguments: if node {
            vec![
                cfg.guest
                    .directory
                    .join("web/e2e/restart-browser-phase.cjs")
                    .to_str()
                    .unwrap()
                    .into(),
            ]
        } else {
            vec![]
        },
        directory: cfg.guest.directory.clone(),
        environment: if node {
            cfg.node_environment(ports)
        } else {
            cfg.guest.server_environment(ports)
        },
        timeout: Duration::from_secs(175),
        stdout_limit: if node { config::RECEIPT } else { config::LOG },
        stderr_limit: config::LOG,
        input: vec![],
        stdout: Sink::Capture,
        stderr: Sink::Capture,
        cleanup: false,
    }
}
struct Owned {
    child: Box<dyn Child>,
    number: u8,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct End {
    number: u8,
    pid: u32,
    exit: Option<Exit>,
    retirement: Option<Retirement>,
    owned_streams_closed: bool,
    stop_succeeded: bool,
    #[serde(skip)]
    result: Result<()>,
    #[serde(skip)]
    output: Vec<u8>,
    #[serde(skip)]
    errors: Vec<u8>,
}
impl End {
    fn closed(&self) -> bool {
        self.stop_succeeded
            && self.owned_streams_closed
            && self
                .retirement
                .as_ref()
                .is_some_and(|v| v.group_cleanup_passed)
    }
    fn passed(&self) -> bool {
        self.closed() && self.exit.as_ref().is_some_and(Exit::success)
    }
}
fn observed(facts: &Option<Retirement>) -> Option<Exit> {
    facts.as_ref().and_then(|f| {
        (f.wait_code.is_some() || f.wait_signal.is_some()).then(|| Exit {
            code: f.wait_code,
            signal: f.wait_signal.map(|signal| {
                nix::sys::signal::Signal::try_from(signal)
                    .map_or_else(|_| format!("SIG{signal}"), |v| v.to_string())
            }),
        })
    })
}
async fn retire(mut owner: Owned, grace: Duration) -> End {
    let stopped = owner.child.stop(grace).await;
    let facts = owner.child.retirement();
    let exit = stopped.as_ref().ok().cloned().or_else(|| observed(&facts));
    let streams = owner.child.owned_streams_closed();
    let result = stopped
        .as_ref()
        .map(|_| ())
        .map_err(|_| anyhow::anyhow!("restart_retirement_failed"));
    let mut end = End {
        number: owner.number,
        pid: owner.child.pid(),
        exit,
        retirement: facts,
        owned_streams_closed: streams,
        stop_succeeded: stopped.is_ok(),
        result,
        output: owner.child.output().to_vec(),
        errors: owner.child.errors().to_vec(),
    };
    if let Err(error) = stopped {
        end.result = Err(error);
    }
    // Drop is before this future resolves, including the reserved-group fallback.
    drop(owner);
    end
}
async fn ready(cfg: &Config, s: &Services<'_>, ports: Ports, server: &mut Owned) -> Result<()> {
    let start = s.clock.elapsed_ms();
    loop {
        check(s, config::ACTIVE).await?;
        ensure!(s.clock.elapsed_ms() - start < 20000.0, Failure(124));
        if server.child.poll().await?.is_some() {
            return Err(Failure(1).into());
        }
        let ready = tokio::select! {biased;
        code=s.host.cancelled()=>return Err(Failure(code).into()),
        result=tokio::time::timeout(Duration::from_millis(750),s.readiness.ready(ports))=>
            result.unwrap_or(Ok(false)).unwrap_or(false)};
        check(s, config::ACTIVE).await?;
        ensure!(s.clock.elapsed_ms() - start < 20000.0, Failure(124));
        if server.child.poll().await?.is_some() {
            return Err(Failure(1).into());
        }
        if ready {
            cfg.check()?;
            return Ok(());
        }
        s.clock.sleep(Duration::from_millis(25)).await;
    }
}
struct Transition {
    server: Option<Owned>,
    end: Option<End>,
    result: Result<()>,
}
async fn transition(
    cfg: &Config,
    s: &Services<'_>,
    ports: Ports,
    request: Request,
    mut server: Option<Owned>,
    session: &mut Session,
    known: &AtomicU8,
) -> Transition {
    let mut end = None;
    let operation: Result<()> = async {
        check(s, config::ACTIVE).await?;
        cfg.check()?;
        if request.phase == "start-second" {
            ensure!(server.is_none(), "restart_second_server_overlap");
            server = Some(Owned {
                child: s.host.spawn(command(cfg, ports, false))?,
                number: 2,
            });
            ready(cfg, s, ports, server.as_mut().unwrap()).await?;
        } else {
            let owned = server
                .take()
                .ok_or_else(|| anyhow::anyhow!("restart_server_missing"))?;
            let start = s.clock.elapsed_ms();
            let ended = retire(owned, Duration::from_secs(20)).await;
            let okay = ended.passed();
            let failure = ended.result.as_ref().err().map(status);
            end = Some(ended);
            if let Some(code) = failure {
                return Err(Failure(code).into());
            }
            ensure!(okay, "restart_server_shutdown_failed");
            ensure!(s.clock.elapsed_ms() - start < 20000.0, Failure(124));
        }
        check(s, config::ACTIVE).await
    }
    .await;
    let code = operation.as_ref().err().map_or(0, status);
    if code != 0 {
        let _ = known.compare_exchange(0, code, Ordering::SeqCst, Ordering::SeqCst);
    }
    // The exact write future settles even when the outer loop observes control
    // cancellation. Native readiness timeout does not drop this operation.
    let replied = session.reply(&cfg.run_id, code).await;
    if let Err(error) = &replied {
        let _ = known.compare_exchange(0, status(error), Ordering::SeqCst, Ordering::SeqCst);
    }
    Transition {
        server,
        end,
        result: operation.and(replied),
    }
}
async fn wait_transition<F: std::future::Future>(
    future: &mut Pin<Box<F>>,
    s: &Services<'_>,
    node: &mut Owned,
    known: &AtomicU8,
) -> Result<F::Output> {
    loop {
        tokio::select! {biased;
            value=future.as_mut()=>return Ok(value),
            code=s.host.cancelled()=>return Err(Failure(match known.load(Ordering::SeqCst){
                0=>code,known=>known}).into()),
            ()=s.clock.sleep(Duration::from_millis(10))=>{
                let observed=node.child.poll().await;
                let failure=match observed{Ok(None)=>check(s,config::ACTIVE).await,
                    Ok(Some(exit))=>Err(node_failure(&exit).into()),Err(error)=>Err(error)};
                if let Err(error)=failure{return Err(Failure(match known.load(Ordering::SeqCst){
                    0=>status(&error),known=>known}).into());}
            }
        }
    }
}
async fn finalize_server(transition: Transition) -> (Vec<End>, Result<()>) {
    let mut ends = transition.end.into_iter().collect::<Vec<_>>();
    if let Some(server) = transition.server {
        ends.push(retire(server, Duration::from_secs(20)).await);
    }
    (ends, transition.result)
}
pub(super) struct Outcome {
    pub code: u8,
    pub report: Value,
}
pub(super) async fn execute(cfg: Config, s: &Services<'_>) -> Result<Outcome> {
    let admitted = check(s, config::ACTIVE).await;
    if let Err(error) = admitted {
        let _ = cfg.finish();
        return Err(error);
    }
    let output = match Directory::new(&cfg.guest.output) {
        Ok(v) => v,
        Err(error) => {
            let _ = cfg.finish();
            return Err(error);
        }
    };
    let started = s.clock.utc();
    let hashes = cfg.sources.as_ref().unwrap().hashes();
    let mut server = None;
    let mut node = None;
    let mut session = None;
    let mut ends = vec![];
    let mut node_end = None;
    let mut primary = 0;
    let ports = s.ports.reserve().and_then(Ports::validate);
    let body: Result<()> = async {
        let ports = ports
            .as_ref()
            .map_err(|_| anyhow::anyhow!("restart_ports_failed"))?;
        cfg.check()?;
        check(s, config::ACTIVE).await?;
        server = Some(Owned {
            child: s.host.spawn(command(&cfg, *ports, false))?,
            number: 1,
        });
        ready(&cfg, s, *ports, server.as_mut().unwrap()).await?;
        check(s, config::ACTIVE).await?;
        cfg.check()?;
        let (child, control) = s.host.spawn_controlled(command(&cfg, *ports, true))?;
        node = Some(Owned { child, number: 0 });
        session = Some(Session::new(control));
        loop {
            check(s, config::ACTIVE).await?;
            let request = tokio::select! {biased;
                code=s.host.cancelled()=>return Err(Failure(code).into()),
                result=session.as_mut().unwrap().poll(&cfg.run_id,s.clock)=>result?,
                ()=s.clock.sleep(Duration::from_millis(10))=>{
                    if let Some(exit)=node.as_mut().unwrap().child.poll().await? {
                        if !exit.success(){return Err(node_failure(&exit).into());}
                        ensure!(session.as_ref().unwrap().complete(),
                            "restart_node_incomplete");break;
                    }
                    if let Some(server)=&mut server {ensure!(server.child.poll().await?.is_none(),
                        "restart_server_exited");}
                    None
                }
            };
            let Some(request) = request else { continue };
            check(s, config::ACTIVE).await?;
            let known = Arc::new(AtomicU8::new(0));
            let mut operation = Box::pin(transition(
                &cfg,
                s,
                *ports,
                request,
                server.take(),
                session.as_mut().unwrap(),
                known.as_ref(),
            ));
            match wait_transition(&mut operation, s, node.as_mut().unwrap(), known.as_ref()).await {
                Ok(done) => {
                    server = done.server;
                    if let Some(end) = done.end {
                        ends.push(end);
                    }
                    done.result?;
                }
                Err(error) => {
                    fail(&mut primary, status(&error));
                    // Both futures start now. A returned second-server handle
                    // is retired in this branch before waiting for Node.
                    let (servers, ended) = tokio::join!(
                        async {
                            let done = operation.await;
                            finalize_server(done).await
                        },
                        retire(node.take().unwrap(), Duration::from_secs(35))
                    );
                    ends.extend(servers.0);
                    node_end = Some(ended);
                    return Err(error);
                }
            }
        }
        Ok(())
    }
    .await;
    if let Err(error) = body {
        fail(&mut primary, status(&error));
    }
    // A cancellation already observed when work returns is primary over a
    // subsequent cleanup error, while a known operation failure stays first.
    tokio::select! {biased;code=s.host.cancelled()=>fail(&mut primary,code),
    ()=std::future::ready(())=>{}}
    let (last_server, last_node) = tokio::join!(
        async {
            match server.take() {
                Some(v) => Some(retire(v, Duration::from_secs(20)).await),
                None => None,
            }
        },
        async {
            match node.take() {
                Some(v) => Some(retire(v, Duration::from_secs(35)).await),
                None => None,
            }
        }
    );
    ends.extend(last_server);
    if let Some(value) = last_node {
        node_end = Some(value);
    }
    let mut control_ok = false;
    if let Some(mut session) = session {
        let settled = session.settle(s.clock).await;
        let closed = session.finish();
        control_ok = settled.is_ok() && closed.is_ok();
        if let Err(error) = settled.and(closed) {
            fail(&mut primary, status(&error));
        }
    }
    for end in ends.iter().chain(node_end.iter()) {
        if let Err(error) = &end.result {
            fail(&mut primary, status(error));
        }
        if !end.passed() {
            fail(&mut primary, 1);
        }
    }
    let receipt = node_end
        .as_ref()
        .and_then(|v| Receipt::read(&v.output, &cfg.run_id, &cfg.browser).ok());
    let native_closed = control_ok
        && ends.len() == 2
        && ends.iter().all(End::passed)
        && node_end.as_ref().is_some_and(End::passed);
    if !native_closed || !receipt.as_ref().is_some_and(Receipt::passed) {
        fail(&mut primary, 1);
    }
    let mut report = receipt.as_ref().map(Receipt::observation).unwrap_or(json!({
        "browser":cfg.browser,"phase":"setup","pages":[],"recoveryPages":null,"captureCalls":[],
        "pageErrors":0,"blockedExternalRequests":0,"browserVersion":null,
        "rejoinMilliseconds":null,"draftPreserved":null,"messageDelivery":null,
        "navigationCountsBeforeRestart":null}));
    report["schemaVersion"] = 1.into();
    report["startedAt"] = started.into();
    report["finishedAt"] = s.clock.utc().into();
    report["servers"] = serde_json::to_value(&ends).unwrap_or_else(|_| {
        fail(&mut primary, 1);
        Value::Array(vec![])
    });
    report["browserCleanupPassed"] = receipt
        .as_ref()
        .is_some_and(Receipt::browser_cleanup)
        .into();
    report["elapsedMilliseconds"] = s.clock.elapsed_ms().into();
    report["controllerSourceSha256"] = json!(hashes);
    if let Ok(ports) = ports {
        report["ports"] = json!(ports);
    }
    for end in &ends {
        let name = format!("server-{}.log", end.number);
        let stored = store_bytes(&output, &name, &[&end.output, &end.errors], 2 * config::LOG);
        if stored.is_err() {
            fail(&mut primary, 1);
        }
    }
    if let Some(node) = &node_end {
        if store_bytes(&output, "browser.log", &[&node.errors], config::LOG).is_err() {
            fail(&mut primary, 1);
        }
        if receipt.is_some()
            && store_bytes(
                &output,
                "browser-receipt.json",
                &[&node.output],
                config::RECEIPT,
            )
            .is_err()
        {
            fail(&mut primary, 1);
        }
    }
    let proofs = cfg.finish();
    if proofs.is_err() {
        fail(&mut primary, 1);
    }
    if let Err(error) = check(s, config::ACCEPT).await {
        fail(&mut primary, status(&error));
    }
    report["passed"] = (primary == 0).into();
    report["status"] = primary.into();
    let stored = output.json("outcome.json", &report);
    if stored.is_err() {
        fail(&mut primary, 1);
    }
    let closed = output.finish();
    if closed.is_err() {
        fail(&mut primary, 1);
    }
    if let Err(error) = check(
        s,
        if primary == 0 {
            config::ACCEPT
        } else {
            config::FINAL
        },
    )
    .await
    {
        fail(&mut primary, status(&error));
    }
    Ok(Outcome {
        code: primary,
        report,
    })
}
fn store_bytes(directory: &Directory, name: &str, parts: &[&[u8]], limit: u64) -> Result<()> {
    let mut record = directory.create(name, limit)?;
    let mut result = Ok(());
    for part in parts {
        if result.is_ok() {
            result = record.append(part);
        }
    }
    let closed = record.finish(directory);
    result.and(closed.map(|_| ()))
}
