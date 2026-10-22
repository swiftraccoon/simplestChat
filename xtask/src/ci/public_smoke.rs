//! Fixed opt-in HTTPS/WSS public protocol smoke, with a private summary artifact.
mod config;
mod controller;
mod files;
#[cfg(test)]
mod files_tests;
#[cfg(test)]
mod fixture;
mod io;
mod native;
#[cfg(test)]
mod oracle_tests;
mod report;
#[cfg(test)]
mod tests;

use controller::{Cancellation, Clock, Services};
use std::{process::ExitCode, time::Instant};

const HELP: &str = "Usage: cargo run --locked -q -p xtask -- public-smoke \
--origin https://chat.example.com [--room lobby] [--output FRESH_ABSOLUTE_DIRECTORY]

Use only a deployment you own or are authorized to test. The HTTPS origin must
be explicit. This joins two guests and WRITES ONE PUBLIC TEST MESSAGE in the room.
It checks HTTPS, fixed static assets and WSS text delivery, then leaves and closes.
No retries, redirects, credentials, camera, microphone, accounts or media checks.
One private summary.json artifact is retained; CLI status is the final gate.
Scheduling: 35s work, one shared 5s socket cleanup, 45s total; OS stalls may exceed it.
";
struct NativeClock(Instant);
impl Clock for NativeClock {
    fn elapsed(&self) -> f64 {
        self.0.elapsed().as_secs_f64()
    }
    fn utc(&self) -> String {
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }
}
struct NativeSignals(crate::owned_process::Signals);
impl Cancellation for NativeSignals {
    async fn pending(&mut self) -> Option<u8> {
        self.0.pending().await
    }
    async fn next(&mut self) -> u8 {
        self.0.next().await
    }
}
pub(super) fn run(arguments: &[String]) -> anyhow::Result<ExitCode> {
    if arguments == ["--help"] || arguments == ["-h"] {
        print!("{HELP}");
        return Ok(ExitCode::SUCCESS);
    }
    let environment = crate::tooling::inherited_env();
    let result = (|| {
        let caller = std::env::current_dir().map_err(|_| io::fail(2, "invalid_output"))?;
        let id = files::uuid()?;
        let cfg = config::Config::read(
            arguments,
            &caller,
            environment
                .get(std::ffi::OsStr::new("NODE_TLS_REJECT_UNAUTHORIZED"))
                .is_some_and(|v| v == "0"),
            &id,
        )?;
        Ok((cfg, id))
    })();
    let (cfg, id) = match result {
        Ok(value) => value,
        Err(error) => return Ok(refusal(error)),
    };
    execute(&cfg, id)
}
fn execute(cfg: &config::Config, id: String) -> anyhow::Result<ExitCode> {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => return Ok(refusal(io::fail(1, "runtime_creation_failed"))),
    };
    let status = runtime.block_on(async {
        let clock = NativeClock(Instant::now());
        let signals = crate::owned_process::Signals::new()
            .map_err(|_| io::fail(1, "signal_registration_failed"))?;
        let mut signals = NativeSignals(signals);
        let wire = native::Native::new(&cfg.origin)?;
        let output = files::Output::new(&cfg.output, cfg.default_output)?;
        let mut services = Services {
            wire: &wire,
            clock: &clock,
            signals: &mut signals,
        };
        let outcome = controller::run(cfg, &mut services, output, id).await;
        drop(outcome.summary);
        Ok::<_, io::Failure>(outcome.status)
    });
    Ok(match status {
        Ok(0) => ExitCode::SUCCESS,
        Ok(status) => refusal(io::fail(status, "public_smoke_failed")),
        Err(error) => refusal(error),
    })
}
fn refusal(error: io::Failure) -> ExitCode {
    eprintln!(
        "public-smoke: {}; inspect the selected private summary directory",
        error.code
    );
    ExitCode::from(error.status)
}
