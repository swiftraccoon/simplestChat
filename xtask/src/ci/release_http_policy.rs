//! Opt-in fixed owned-loopback release response checker.

mod ca;
mod config;
mod execute;
mod files;
#[cfg(test)]
mod fixture;
mod json;
mod native;
#[cfg(test)]
mod native_tests;
#[cfg(test)]
mod oracle_tests;
mod plan;
mod policy;
#[cfg(test)]
mod reference_pins;
#[cfg(test)]
mod tests;
pub(super) mod tools;

use crate::{
    ci::{
        browser_suite::process::Signals,
        process::{Executor, Native},
    },
    tooling,
};
use std::{path::Path, process::ExitCode, sync::Arc};
type Result<T> = tooling::Result<T>;
fn fail(status: u8, code: &'static str) -> tooling::Failure {
    tooling::Failure::new(status, code)
}
const HELP: &str = "Usage: xtask ci release-http-policy --fixture-token HEX32 --caddy-id HEX64 \
--app-id HEX64 --ca ABSOLUTE_FILE --ca-sha256 HEX64\n\
Requires root on a disposable Linux release fixture. Exactly five credential-free\n\
HTTPS GETs use 127.0.0.1:443, localhost TLS identity, and only the retained fixture CA.\n\
Four fixed Docker inspections prove the two original fixture identities pre/post.\n\
Scheduling: one 180s entry; one aggregate HTTP25s with 3s connect/read inactivity.\n\
No arbitrary origin, credentials, proxy, redirect, retry or installed Python fallback.\n";

async fn operation(
    config: &config::Config,
    temporary: &Path,
    time: &plan::Time,
    executor: &mut impl Executor,
) -> Result<()> {
    executor.ready().await?;
    time.admit(164)?;
    let (ca, bytes) = ca::Ca::open(config)?;
    let tools = match tools::Tools::open(Path::new("/usr/bin/docker")) {
        Ok(tools) => tools,
        Err(error) => {
            let _ = ca.finish();
            return Err(error);
        }
    };
    let scratch = match files::Scratch::open(temporary) {
        Ok(scratch) => scratch,
        Err(error) => {
            let _ = ca.finish();
            let _ = tools.finish();
            return Err(error);
        }
    };
    let http = match native::Native::new(&bytes) {
        Ok(http) => http,
        Err(error) => {
            let _ = ca.finish();
            let _ = tools.finish();
            let _ = scratch.finish();
            return Err(error);
        }
    };
    execute::complete(config, tools, scratch, ca, http, time, executor).await
}
pub(super) fn run(arguments: &[String]) -> anyhow::Result<ExitCode> {
    if arguments == ["--help"] || arguments == ["-h"] {
        print!("{HELP}");
        return Ok(ExitCode::SUCCESS);
    }
    let time = plan::Time::new(Arc::new(plan::RealClock));
    let captured = tooling::inherited_env();
    let config = config::Config::parse(arguments);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    let result = match runtime {
        Ok(runtime) => runtime.block_on(async {
            let signals = Signals::new().map_err(|_| fail(1, "http_policy_signals"))?;
            let environment = [
                ("PATH".into(), "/usr/sbin:/usr/bin:/sbin:/bin".into()),
                ("LC_ALL".into(), "C".into()),
            ]
            .into_iter()
            .collect();
            let mut executor = Native::new(Path::new("/"), &environment, signals);
            let primary = async {
                let config = config?;
                if rustix::system::uname().sysname() != c"Linux"
                    || !rustix::process::geteuid().is_root()
                {
                    return Err(fail(1, "fixture_host"));
                }
                let temporary = captured
                    .get(std::ffi::OsStr::new("TMPDIR"))
                    .filter(|s| !s.is_empty())
                    .map_or(Path::new("/tmp"), Path::new);
                operation(&config, temporary, &time, &mut executor).await
            }
            .await;
            let value = match &primary {
                Ok(()) => policy::report(),
                Err(error) => serde_json::json!({"passed":false,"code":error.message}),
            };
            let replay = match serde_json::to_vec(&value) {
                Ok(mut bytes) => {
                    bytes.push(b'\n');
                    match time.replay() {
                        Ok(deadline) => executor.replay(&bytes, false, deadline).await,
                        Err(e) => Err(e),
                    }
                }
                Err(_) => Err(fail(1, "http_policy_receipt")),
            };
            let pending = executor.ready().await;
            let final_clock = time.now();
            primary
                .and(replay)
                .and(pending)
                .and(final_clock.map(|_| ()))
        }),
        Err(_) => Err(fail(1, "http_policy_runtime")),
    };
    match result {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(error) => {
            eprintln!("Release HTTP policy failed.");
            Ok(ExitCode::from(error.code))
        }
    }
}
