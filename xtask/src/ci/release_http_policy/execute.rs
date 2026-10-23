//! Four owned inspections surround exactly five credential-free requests.

use super::{
    Result, ca::Ca, config::Config, fail, files::Scratch, native::Http, plan::Time, policy,
    tools::Tools,
};
use crate::ci::process::{Executor, settle_unforwarded};
use std::{future::Future, time::Duration};

async fn inspection(
    config: &Config,
    tools: &Tools,
    scratch: &Scratch,
    time: &Time,
    executor: &mut impl Executor,
    index: usize,
) -> Result<()> {
    executor.ready().await?;
    let id = if index.is_multiple_of(2) {
        &config.caddy
    } else {
        &config.app
    };
    let mut capture = scratch.capture().await?;
    let command = tools.command(id, time, index);
    let (mut command, proof) = match command {
        Ok(command) => command,
        Err(e) => {
            // Forward no CLI, but settle the actual admitted peers.
            let mut empty =
                crate::ci::command::Command::capture(crate::ci::command::Program::Docker, &[]);
            capture.attach(&mut empty)?;
            let _ = settle_unforwarded(empty).await;
            let _ = capture.finish(Err(fail(1, "fixture_container_inspection")));
            return Err(e);
        }
    };
    capture.attach(&mut command)?;
    let admitted = executor
        .ready()
        .await
        .and_then(|_| time.docker(index).map(|_| ()));
    if let Err(e) = admitted {
        let _ = settle_unforwarded(command).await;
        let _ = capture.finish(Err(fail(1, "fixture_container_inspection")));
        return Err(e);
    }
    let result = executor.run(command).await;
    let observed = capture.finish(proof.seals());
    let pending = executor.ready().await;
    let fence = time.now();
    result.map_err(|e| fail(e.code, "fixture_container_inspection"))?;
    let bytes = observed?;
    pending?;
    fence?;
    policy::owned(config, index, &bytes)
}
async fn http_wait<T>(
    future: impl Future<Output = Result<T>>,
    deadline: std::time::Instant,
    time: &Time,
    executor: &mut impl Executor,
) -> Result<T> {
    tokio::pin!(future);
    let result = loop {
        tokio::select! {
            biased;
            result = &mut future => break result,
            _ = tokio::time::sleep_until(deadline.into()) =>
                break Err(fail(124, "http_policy_deadline")),
            _ = tokio::time::sleep(Duration::from_millis(10)) => {
                if let Err(error) = executor.ready().await { break Err(error); }
                if let Err(error) = time.now() { break Err(error); }
            }
        }
    };
    let pending = executor.ready().await;
    let fence = time.now().and_then(|now| {
        if now >= deadline {
            Err(fail(124, "http_policy_deadline"))
        } else {
            Ok(())
        }
    });
    result.and_then(|v| pending.and(fence).map(|_| v))
}
pub(super) async fn body(
    config: &Config,
    tools: &Tools,
    scratch: &Scratch,
    ca: &mut Ca,
    http: &impl Http,
    time: &Time,
    executor: &mut impl Executor,
) -> Result<()> {
    inspection(config, tools, scratch, time, executor, 0).await?;
    inspection(config, tools, scratch, time, executor, 1).await?;
    ca.bytes()?;
    let deadline = time.http()?;
    for (index, endpoint) in policy::ENDPOINTS.into_iter().enumerate() {
        executor.ready().await?;
        ca.check()?;
        if time.now()? >= deadline {
            return Err(fail(124, "http_policy_deadline"));
        }
        let response = http_wait(http.request(endpoint), deadline, time, executor).await?;
        policy::validate(index, response.status, &response.headers, &response.body)?;
    }
    ca.bytes()?;
    inspection(config, tools, scratch, time, executor, 2).await?;
    inspection(config, tools, scratch, time, executor, 3).await
}
pub(super) async fn complete(
    config: &Config,
    tools: Tools,
    scratch: Scratch,
    mut ca: Ca,
    http: impl Http,
    time: &Time,
    executor: &mut impl Executor,
) -> Result<()> {
    let primary = body(config, &tools, &scratch, &mut ca, &http, time, executor).await;
    drop(http); // Standard client Drop; no claimed checked internal TLS/socket close.
    let pending = executor.ready().await;
    let ca = ca.finish();
    let tools = tools.finish();
    let scratch = scratch.finish();
    let groups = if executor.cleanup_passed() {
        Ok(())
    } else {
        Err(fail(1, "http_policy_cleanup"))
    };
    let final_clock = time.now();
    primary
        .and(pending)
        .and(ca)
        .and(tools)
        .and(scratch)
        .and(groups)
        .and(final_clock.map(|_| ()))
}
