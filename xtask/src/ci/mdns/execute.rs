use super::{
    Clock,
    config::Config,
    failure,
    files::{Output, Source, Temporary},
    policy::{self, Observation},
    tools::{Paths, Role, Tools},
};
use crate::{
    ci::{
        command::Command,
        process::{Executor, settle_unforwarded},
    },
    tooling::{Failure, Result},
};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    sync::Arc,
    time::{Duration, Instant},
};

fn fixed(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}
fn keep(primary: &mut Option<Failure>, error: Failure) {
    if primary.is_none() {
        *primary = Some(error);
    }
}
struct Context<'a, E> {
    config: &'a Config,
    executor: &'a mut E,
    clock: &'a dyn Clock,
    temporary: &'a mut Temporary,
    tools: &'a mut Tools,
    paths: &'a Paths,
    index: usize,
    primary: &'a mut Option<Failure>,
}
impl<E: Executor> Context<'_, E> {
    fn elapsed(&self) -> Result<f64> {
        let elapsed = self.clock.elapsed();
        if !elapsed.is_finite() || elapsed < 0.0 {
            return Err(failure(1));
        }
        Ok(elapsed)
    }
    fn body(&self, cap: f64) -> Result<Duration> {
        let replay = if self.config.artifact_only { 0.0 } else { 25.0 };
        let left = self.config.limit() - self.elapsed()? - 30.0 - replay;
        if left <= 0.0 {
            return Err(failure(124));
        }
        Ok(Duration::from_secs_f64(cap.min(left)))
    }
    async fn call(&mut self, role: Role, args: Vec<OsString>, cap: f64) -> Result<Observation> {
        self.executor.ready().await?;
        let timeout = self.body(cap)?;
        let proof = self.tools.invocation(role, self.paths)?;
        if role == Role::Probe {
            self.temporary.verify_probe()?;
        }
        let index = self.index;
        self.index += 1;
        let logs = self.temporary.logs(index).await?;
        let mut command = Command::capture(role.program(), &[]);
        command.arguments = args;
        command.timeout = timeout;
        command.grace = Duration::from_secs(1);
        command.capture = None;
        command.separate_logs = Some(Arc::new(logs));
        command.mdns = Some(proof.clone());
        let admitted = self.body(cap);
        if let Err(error) = admitted {
            let _ = settle_unforwarded(command).await;
            return Err(error);
        }
        command.timeout = admitted.unwrap();
        if role == Role::Probe
            && let Err(error) = self.temporary.verify_probe()
        {
            let _ = settle_unforwarded(command).await;
            return Err(error);
        }
        let result = self.executor.run(command).await;
        let evidence = proof.observed()?;
        let selected = proof.validate(role.program()).map(|_| ());
        drop(proof);
        let output = self.temporary.read(index);
        if let Some(code) = evidence.interrupted {
            keep(self.primary, failure(code));
        } else if evidence.timed_out {
            keep(self.primary, failure(124));
        } else if evidence.settled && evidence.exit != Some(0) {
            keep(self.primary, failure(1));
        }
        if !evidence.settled
            || !evidence.group_closed
            || !evidence.io_closed
            || !self.executor.cleanup_passed()
        {
            return Err(failure(1));
        }
        selected?;
        let observed = Observation {
            stdout: output?,
            exit: evidence.exit.and_then(|v| u8::try_from(v).ok()),
            timed_out: evidence.timed_out,
        };
        if let Some(code) = evidence.interrupted {
            keep(self.primary, failure(code));
            return Err(failure(code));
        }
        if evidence.timed_out {
            keep(self.primary, failure(124));
        } else if !observed.passed() {
            keep(self.primary, failure(1));
        }
        // IO/readback failure may follow a known status without replacing it.
        if let Err(error) = result
            && observed.passed()
        {
            return Err(error);
        }
        let post = self.body(cap).map(|_| ());
        if let Err(error) = post {
            let code = error.code;
            keep(self.primary, error);
            return Err(failure(code));
        }
        self.executor.ready().await?;
        if let Err(error) = self.body(cap) {
            let code = error.code;
            keep(self.primary, error);
            return Err(failure(code));
        }
        Ok(observed)
    }
}

pub(super) async fn run(
    config: &Config,
    executor: &mut impl Executor,
    clock: &dyn Clock,
    paths: &Paths,
) -> Result<()> {
    run_after(config, executor, clock, paths, |_| {}).await
}
pub(super) async fn run_after(
    config: &Config,
    executor: &mut impl Executor,
    clock: &dyn Clock,
    paths: &Paths,
    after_probe: impl FnOnce(&std::path::Path),
) -> Result<()> {
    let mut after_probe = Some(after_probe);
    // Signal registration is already active before any filesystem admission.
    executor.ready().await?;
    let output = Output::new(&config.output)?;
    let mut temporary = match Temporary::new(config) {
        Ok(v) => v,
        Err(e) => {
            let _ = output.finish();
            return Err(e);
        }
    };
    let mut source = if config.phase == "preflight" {
        match Source::new(&config.root) {
            Ok(v) => Some(v),
            Err(e) => {
                let _ = temporary.finish();
                let _ = output.finish();
                return Err(e);
            }
        }
    } else {
        None
    };
    let mut tools = Tools::default();
    let mut primary = None;
    let mut report = json!({"schemaVersion":1,"phase":config.phase,"startedAt":clock.utc(),
        "registrationDeadlineMs":5000});
    let body = async {
        let mut context = Context {
            config,
            executor,
            clock,
            temporary: &mut temporary,
            tools: &mut tools,
            paths,
            index: 0,
            primary: &mut primary,
        };
        let interface = context.call(Role::Interfaces, fixed(&["-a"]), 5.0).await?;
        if !interface.passed() {
            return Err(failure(1));
        }
        report["interfaces"] = policy::interfaces(&config.address, &interface.stdout)?;
        let plist = context
            .call(
                Role::Plist,
                fixed(&[
                    "-extract",
                    "Label",
                    "raw",
                    "-o",
                    "-",
                    "/System/Library/LaunchDaemons/com.apple.mDNSResponder.plist",
                ]),
                5.0,
            )
            .await?;
        let label = policy::text(&plist.stdout)?.trim();
        let launch = if plist.exit == Some(0)
            && [
                "com.apple.mDNSResponder",
                "com.apple.mDNSResponder.reloaded",
            ]
            .contains(&label)
        {
            context
                .call(
                    Role::Launch,
                    fixed(&["print", &format!("system/{label}")]),
                    5.0,
                )
                .await?
        } else {
            Observation {
                stdout: vec![],
                exit: plist.exit,
                timed_out: plist.timed_out,
            }
        };
        let logs = context
            .call(
                Role::Logs,
                fixed(&[
                    "show",
                    "--last",
                    "2m",
                    "--style",
                    "ndjson",
                    "--info",
                    "--debug",
                    "--predicate",
                    policy::PREDICATE,
                ]),
                5.0,
            )
            .await?;
        report["system"] = policy::system(&plist, &launch, &logs)?;
        if report.pointer("/system/responder/readable") != Some(&Value::Bool(true))
            || report.pointer("/system/logs/readable") != Some(&Value::Bool(true))
        {
            keep(context.primary, failure(1));
        }
        if let Some(source) = &mut source {
            source.verify()?;
            let args = vec![
                "-std=c11".into(),
                "-fblocks".into(),
                "-Wall".into(),
                "-Wextra".into(),
                "-Werror".into(),
                source.path.as_os_str().to_owned(),
                "-o".into(),
                context.temporary.path.join("probe").into_os_string(),
            ];
            let compiled = context.call(Role::Compiler, args, 15.0).await;
            let verified = source.verify();
            // A settled compiler owns its one fixed generated filename. Admit
            // partial regular output for exact cleanup, never for execution.
            let probe = if let Ok(compiled) = &compiled {
                context.temporary.probe(compiled.exit == Some(0))
            } else {
                Ok(None)
            };
            let compiled = compiled?;
            verified?;
            if compiled.exit != Some(0) {
                report["native"] = json!({"passed":false,"stage":"compile",
                "exitCode":compiled.exit,"timedOut":compiled.timed_out});
            } else {
                let probe = probe?.ok_or_else(|| failure(1))?;
                after_probe.take().unwrap()(&probe);
                // The private path is selected once after successful compilation;
                // before/after execution both descriptor stamps are revalidated.
                context.temporary.verify_probe()?;
                let invocation = context.tools.admit(Role::Probe, &probe)?;
                drop(invocation);
                context.temporary.verify_probe()?;
                let identifier = super::files::uuid()?;
                let result = context
                    .call(
                        Role::Probe,
                        vec![config.address.clone().into(), identifier.into()],
                        8.0,
                    )
                    .await?;
                report["native"] = policy::probe(&result);
                if report.pointer("/native/passed") != Some(&Value::Bool(true)) {
                    keep(context.primary, failure(1));
                }
            }
        }
        Ok::<_, Failure>(())
    }
    .await;
    if let Err(e) = body {
        keep(&mut primary, e);
        report["stage"] = "diagnostic_failed".into();
    }
    let tools = tools.finish();
    let source = source.map(Source::finish).transpose();
    let temporary = temporary.finish();
    for result in [tools, source.map(|_| ()), temporary, executor.ready().await] {
        if let Err(e) = result {
            keep(&mut primary, e);
        }
    }
    let elapsed = clock.elapsed();
    if !elapsed.is_finite() || elapsed < 0.0 {
        keep(&mut primary, failure(1));
    } else if elapsed >= config.limit() {
        keep(&mut primary, failure(124));
    }
    let passed = primary.is_none()
        && report.pointer("/system/responder/readable") == Some(&Value::Bool(true))
        && report.pointer("/system/logs/readable") == Some(&Value::Bool(true))
        && (config.phase == "snapshot"
            || report.pointer("/native/passed") == Some(&Value::Bool(true)));
    if !passed {
        keep(&mut primary, failure(1));
    }
    report["passed"] = passed.into();
    report["finishedAt"] = clock.utc().into();
    let bytes = serde_json::to_vec_pretty(&report).map_err(|_| failure(1));
    let published = match &bytes {
        Ok(bytes) => {
            let mut bytes = bytes.clone();
            bytes.push(b'\n');
            output.publish(&config.phase, &bytes)
        }
        Err(_) => Err(failure(1)),
    };
    let closed = output.finish();
    let replay_allowed = published.is_ok() && closed.is_ok();
    for result in [published, closed, executor.ready().await] {
        if let Err(e) = result {
            keep(&mut primary, e);
        }
    }
    if !config.artifact_only && replay_allowed {
        let mut bytes = serde_json::to_vec(&report).map_err(|_| failure(1))?;
        bytes.push(b'\n');
        let left = config.limit() - clock.elapsed();
        if !left.is_finite() || left <= 0.0 {
            keep(&mut primary, failure(124));
        } else if let Err(e) = executor
            .replay(
                &bytes,
                false,
                Instant::now() + Duration::from_secs_f64(left.min(25.0)),
            )
            .await
        {
            keep(&mut primary, e);
        }
    }
    if let Err(e) = executor.ready().await {
        keep(&mut primary, e);
    }
    let elapsed = clock.elapsed();
    if !elapsed.is_finite() || elapsed < 0.0 {
        keep(&mut primary, failure(1));
    } else if elapsed >= config.limit() {
        keep(&mut primary, failure(124));
    }
    primary.map_or(Ok(()), Err)
}
