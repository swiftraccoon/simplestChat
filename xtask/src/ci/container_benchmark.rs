//! Standalone, opt-in root/Linux benchmark owner. Installed scripts stay unchanged.
mod cleanup;
mod config;
mod controller;
mod files;
mod io;
mod journal;
mod namespace;
mod native;
#[cfg(test)]
mod oracle_tests;
pub(super) mod plan;
mod policy;
mod sampling;
#[cfg(test)]
mod tests;
pub(super) mod tools;

pub(super) fn failure(code: u8) -> crate::tooling::Failure {
    crate::tooling::Failure::new(
        code,
        "Cannot complete the owned private container benchmark.",
    )
}

use crate::{
    ci::process::{Executor, Native},
    tooling::{self, Result},
};
use std::{process::ExitCode, time::Instant};
const HELP: &str = concat!(
    "Usage: xtask container-benchmark run --server-image sha256:HEX64 ",
    "--generator-image sha256:HEX64 --output FRESH_ABSOLUTE_DIRECTORY ",
    "[--workers N --clients N --rooms N --duration N --ramp-up N --warmup N]\n",
    "       xtask container-benchmark cleanup\n",
    "Root/Linux, rootful same-host Unix Docker daemon and cgroup v2 only. Creates exactly\n",
    "its two private containers; no public service is stopped or changed. Recovery\n",
    "always obtains the same fixed workload lock. No normal-mode bypass is exposed.\n",
    "Standalone scheduling: 2400s run entry with 1260s retirement reserve; cleanup 1260s\n",
    "including lock wait. This does not fit the unchanged installed 900s/90s service.\n",
    "Only explicit authorized workloads may be run; no daemon or kernel test is implied.\n"
);
pub(super) fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    if args == ["--help"] || args == ["-h"] {
        print!("{HELP}");
        return Ok(ExitCode::SUCCESS);
    }
    let config = match config::Config::parse(args) {
        Ok(c) => c,
        Err(e) => return Ok(refusal(e)),
    };
    if rustix::process::geteuid().as_raw() != 0 || !cfg!(target_os = "linux") {
        return Ok(refusal(failure(2)));
    }
    let caller = match std::env::current_dir() {
        Ok(p) => p,
        Err(_) => return Ok(refusal(failure(2))),
    };
    let env = tooling::inherited_env();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(_) => return Ok(refusal(failure(1))),
    };
    let result = runtime.block_on(execute(config, &caller, &env));
    Ok(match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => refusal(e),
    })
}
fn refusal(e: tooling::Failure) -> ExitCode {
    eprintln!("container-benchmark: {}", failure(e.code));
    ExitCode::from(e.code)
}
async fn execute(
    config: config::Config,
    caller: &std::path::Path,
    env: &tooling::Environment,
) -> Result<()> {
    let start = Instant::now();
    let recovery = matches!(config, config::Config::Cleanup);
    let time = if recovery {
        plan::Time::recovery(start)
    } else {
        plan::Time::new(start)
    };
    let budget = io::Budget::default();
    let paths = files::Paths::fixed();
    let token = files::hex(&files::random::<32>(&budget)?);
    let signals = crate::owned_process::Signals::new().map_err(|_| failure(1))?;
    let mut native = Native::new(caller, env, signals);
    if recovery && !files::Guard::exists(&paths)? {
        native.ready().await?;
        time.fence(true)?;
        return Ok(());
    }
    let guard = files::Guard::acquire(paths.clone(), &time, &budget, recovery).await?;
    if recovery {
        let no_work = guard
            .current()
            .and_then(|bytes| bytes.map(|b| journal::Journal::decode(&b)).transpose())
            .map(|j| j.is_none_or(|j| j.finalized));
        if !matches!(no_work, Ok(false)) {
            let mut primary = None;
            cleanup::first(&mut primary, no_work.map(|_| ()));
            cleanup::first(&mut primary, guard.finish());
            cleanup::first(&mut primary, native.ready().await);
            cleanup::first(&mut primary, time.fence(true));
            return primary.map_or(Ok(()), Err);
        }
    }
    let tools = tools::Tools::new(
        caller,
        env,
        time.clone(),
        &token,
        &guard.config,
        budget.clone(),
    );
    let mut primary = None;
    match tools {
        Ok(tools) => {
            let host = native::Host::new(&tools);
            let mut runner = native::Adapter::new(&mut native, &tools);
            let result = match config {
                config::Config::Run(settings) => {
                    controller::run(
                        &settings,
                        env,
                        &mut runner,
                        &controller::Context {
                            obs: &host,
                            guard: &guard,
                            paths: &paths,
                            budget: &budget,
                            time: &time,
                            token: &token,
                        },
                    )
                    .await
                }
                config::Config::Cleanup => {
                    controller::recover(
                        &mut runner,
                        &controller::Context {
                            obs: &host,
                            guard: &guard,
                            paths: &paths,
                            budget: &budget,
                            time: &time,
                            token: "",
                        },
                    )
                    .await
                }
            };
            cleanup::first(&mut primary, result);
            cleanup::first(&mut primary, tools.finish());
        }
        Err(e) => cleanup::first(&mut primary, Err(e)),
    }
    cleanup::first(&mut primary, guard.finish());
    cleanup::first(&mut primary, native.ready().await);
    cleanup::first(&mut primary, time.fence(true));
    if !native.cleanup_passed() {
        cleanup::first(&mut primary, Err(failure(1)));
    }
    primary.map_or(Ok(()), Err)
}
