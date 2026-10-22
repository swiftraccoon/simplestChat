//! One workload owns its children, credentials, sampler and final evidence.

use super::Manifest;
use crate::bench::{
    artifacts,
    clock::Clock,
    commands,
    media_sampler::{Media, Sampler},
    metrics::Metrics,
    options::{Options, Purpose, Scenario},
    process::{Child, Command, Exit, Host, Sink},
    provenance,
    reports::Reports,
    resources::{self, Role, Sample},
    sampling::{Ports, Resources},
    subscription, summary,
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct Services<'a> {
    pub host: &'a dyn Host,
    pub clock: &'a dyn Clock,
    pub metrics: &'a dyn Metrics,
    pub resources: &'a dyn Resources,
    pub ports: &'a dyn Ports,
    pub media: &'a dyn Media,
    pub reports: &'a dyn Reports,
    pub environment: &'a BTreeMap<OsString, OsString>,
}

pub struct Context<'a> {
    pub options: &'a Options,
    pub scenario: &'a Scenario,
    pub variant: &'a str,
    pub repetition: u32,
    pub manifest: &'a Manifest,
    pub services: &'a Services<'a>,
}

pub(super) struct Owned {
    pub(super) child: Box<dyn Child>,
    pub(super) exit: Option<Exit>,
    pub(super) closed: bool,
}

pub(super) struct State<'a> {
    pub(super) context: Context<'a>,
    pub(super) directory: PathBuf,
    pub(super) name: String,
    pub(super) started_at: String,
    pub(super) origin: String,
    pub(super) token: String,
    pub(super) server: Option<Owned>,
    pub(super) generator: Option<Owned>,
    pub(super) capture: Option<Owned>,
    pub(super) timeline: Option<Value>,
    pub(super) timeline_start: f64,
    pub(super) timeline_saved: bool,
    pub(super) sampler: Option<Sampler>,
    pub(super) pending_media: Option<Value>,
    pub(super) samples: Vec<Sample>,
    pub(super) start: f64,
    pub(super) row: Option<Value>,
}

impl State<'_> {
    pub(super) fn mark(&mut self, event: &str) {
        if let Some(timeline) = &mut self.timeline {
            timeline["events"]
                .as_array_mut()
                .unwrap()
                .push(json!({"event":event,
                "at":self.context.services.clock.utc(),
                "elapsedMs":self.context.services.clock.elapsed_ms()-self.timeline_start}));
        }
    }

    pub(super) fn alive(&self) -> Value {
        json!({"server":self.server.as_ref().is_some_and(|owned|owned.exit.is_none()),
            "generator":self.generator.as_ref().is_some_and(|owned|owned.exit.is_none())})
    }

    pub(super) async fn poll(&mut self) -> Result<()> {
        for (role, event) in [
            ("server", "server_exited"),
            ("generator", "generator_exited"),
            ("capture", ""),
        ] {
            let slot = match role {
                "server" => &mut self.server,
                "generator" => &mut self.generator,
                _ => &mut self.capture,
            };
            let mut marked = false;
            if let Some(owned) = slot
                && owned.exit.is_none()
            {
                owned.exit = owned.child.poll().await?;
                marked = owned.exit.is_some();
            }
            if marked && !event.is_empty() {
                self.mark(event);
            }
        }
        Ok(())
    }

    pub(super) fn spawn(
        &self,
        binary: &Path,
        args: Vec<String>,
        directory: &Path,
        environment: BTreeMap<OsString, OsString>,
        log: &str,
    ) -> Result<Owned> {
        let file = artifacts::private_file(&self.directory.join(log))?;
        let error_file = file.try_clone()?;
        // Both descriptors share one file description, preserving combined log
        // offsets instead of overwriting each other's output.
        let child = self.context.services.host.spawn(Command {
            executable: binary.to_owned(),
            arguments: args,
            directory: directory.to_owned(),
            environment,
            timeout: Duration::from_secs(u64::from(
                self.context.options.ramp_up
                    + self.context.options.warmup
                    + self.context.options.duration
                    + 250,
            )),
            stdout_limit: 64 * 1024 * 1024,
            stderr_limit: 64 * 1024 * 1024,
            input: Vec::new(),
            stdout: Sink::File(file),
            stderr: Sink::File(error_file),
            cleanup: false,
        })?;
        Ok(Owned {
            child,
            exit: None,
            closed: false,
        })
    }

    async fn wait(&mut self, duration: Duration) -> Result<()> {
        let end = self.context.services.clock.elapsed_ms() + duration.as_secs_f64() * 1000.0;
        // Observe process termination independently of the500ms sample cadence.
        // Polling never reaps, so the exact process-group identity stays owned.
        loop {
            self.poll().await?;
            let remaining = end - self.context.services.clock.elapsed_ms();
            if remaining <= 0.0 {
                return Ok(());
            }
            self.context
                .services
                .clock
                .sleep(Duration::from_secs_f64((remaining / 1000.0).min(0.01)))
                .await;
        }
    }

    async fn capture_start(&mut self) -> Result<()> {
        let options = self.context.options;
        if options.capture_interface.is_none() {
            return Ok(());
        }
        let args = commands::capture(options, &self.directory)?;
        artifacts::json(
            &self.directory.join("capture-invocation.json"),
            &json!({
            "command":"tcpdump","args":args,
            "limitations":concat!("Bounded first matching packet headers only;64-byte snapshots, ",
                "not full payloads. Check capture.log for kernel drops.")}),
        )?;
        self.capture = Some(self.spawn(
            Path::new("tcpdump"),
            args,
            &options.candidate_root,
            commands::clean(self.context.services.environment),
            "capture.log",
        )?);
        let mut listening = false;
        for _ in 0..100 {
            self.poll().await?;
            if self
                .capture
                .as_ref()
                .is_some_and(|owned| owned.exit.is_some())
            {
                break;
            }
            let bytes = artifacts::live_log_prefix(&self.directory.join("capture.log"))?;
            listening = String::from_utf8_lossy(&bytes).contains("listening on ");
            if listening {
                break;
            }
            self.wait(Duration::from_millis(20)).await?;
        }
        ensure!(listening, "Loopback capture unavailable");
        Ok(())
    }

    async fn finish_capture(&mut self) -> Result<()> {
        if self.capture.is_none() {
            return Ok(());
        }
        if self.capture.as_ref().unwrap().exit.is_none() {
            self.wait(Duration::from_secs(2)).await?;
        }
        let owned = self.capture.as_mut().unwrap();
        owned.exit = Some(owned.child.stop(Duration::from_secs(5)).await?);
        owned.closed = true;
        ensure!(
            owned.exit.as_ref().unwrap().success(),
            "Packet capture did not finish cleanly"
        );
        Ok(())
    }

    async fn load(&mut self) -> Result<()> {
        let options = self.context.options;
        let clock = self.context.services.clock;
        let deadline =
            f64::from(options.ramp_up + options.warmup + options.duration + 135) * 1000.0;
        loop {
            self.poll().await?;
            if self.generator.as_ref().unwrap().exit.is_some() {
                break;
            }
            ensure!(
                self.server.as_ref().unwrap().exit.is_none(),
                "Server exited during load"
            );
            ensure!(
                self.capture
                    .as_ref()
                    .is_none_or(|owned| owned.exit.as_ref().is_none_or(Exit::success)),
                "Packet capture failed"
            );
            if options.purpose == Purpose::Performance {
                let resources = self.context.services.resources;
                let (server, generator) = tokio::join!(
                    resources.sample(self.server.as_ref().unwrap().child.pid()),
                    resources.sample(self.generator.as_ref().unwrap().child.pid()),
                );
                self.samples.push(Sample {
                    elapsed_ms: clock.elapsed_ms() - self.start,
                    server,
                    generator,
                });
            }
            let now = clock.elapsed_ms() - self.start;
            let alive = self.alive();
            self.pending_media = self
                .sampler
                .as_mut()
                .and_then(|sampler| sampler.prepare(now, alive));
            if self
                .pending_media
                .as_ref()
                .is_some_and(|sample| sample["status"].is_null())
            {
                let result = self
                    .context
                    .services
                    .media
                    .fetch(&self.origin, &self.token)
                    .await;
                self.poll().await?;
                let alive = self.alive();
                self.sampler.as_ref().unwrap().complete(
                    self.pending_media.as_mut().unwrap(),
                    result,
                    clock.elapsed_ms() - self.start,
                    alive,
                );
            }
            if let Some(sample) = self.pending_media.take() {
                self.sampler.as_mut().unwrap().save(sample);
            }
            ensure!(
                clock.elapsed_ms() - self.start <= deadline,
                "Outer generator deadline exceeded"
            );
            self.wait(Duration::from_millis(500)).await?;
        }
        self.finish_capture().await?;
        ensure!(
            self.generator
                .as_ref()
                .unwrap()
                .exit
                .as_ref()
                .unwrap()
                .success(),
            "Generator failed"
        );
        // A NOWAIT leader exit reserves its PGID, but does not establish that
        // descendants and output sinks are closed. Retire that ownership before
        // admitting any summary or results written by the generator.
        let generator = self.generator.as_mut().unwrap();
        generator.exit = Some(generator.child.stop(Duration::from_secs(5)).await?);
        generator.closed = true;
        Ok(())
    }

    async fn workload(&mut self) -> Result<()> {
        let options = self.context.options;
        let services = self.context.services;
        let identity = self.context.manifest.server(self.context.variant)?;
        provenance::verify_executable(
            &options.generator,
            &self.context.manifest.generator_sha256,
            "Generator",
        )?;
        provenance::verify_executable(&identity.binary, &identity.binary_sha256, "Server")?;
        services
            .ports
            .available(options.port, options.udp_port, options.workers)?;
        let environment = commands::server_environment(
            options,
            &self.directory,
            &self.token,
            services.environment,
        );
        self.server = Some(self.spawn(
            &identity.binary,
            Vec::new(),
            &identity.root,
            environment.clone(),
            "server.log",
        )?);
        let mut ready = false;
        for _ in 0..100 {
            self.poll().await?;
            if self.server.as_ref().unwrap().exit.is_some() {
                break;
            }
            ready = services
                .metrics
                .healthy(&self.origin)
                .await
                .unwrap_or(false);
            self.poll().await?;
            if ready {
                break;
            }
            self.wait(Duration::from_millis(100)).await?;
        }
        ensure!(
            ready && self.server.as_ref().unwrap().exit.is_none(),
            "Owned server did not become ready"
        );
        self.mark("server_ready");
        let before = services.metrics.fetch(&self.origin, &self.token).await?;
        artifacts::new_file(
            &self.directory.join("metrics-before.txt"),
            before.raw.as_bytes(),
        )?;
        self.capture_start().await?;
        let args = commands::generator(
            options,
            commands::Invocation {
                scenario: self.context.scenario,
                directory: &self.directory,
                name: &self.name,
                room: &format!(
                    "benchmark-{}",
                    &uuid::Uuid::new_v4().simple().to_string()[..12]
                ),
                server_revision: &provenance::server_revision_label(identity),
                generator_revision: &self.context.manifest.generator_source_identity,
            },
        )?;
        artifacts::json(
            &self.directory.join("invocation.json"),
            &json!({
            "startedAt":self.started_at,"variant":self.context.variant,
            "scenario":self.context.scenario,"repetition":self.context.repetition,"args":args,
            "serverConfiguration":commands::public_configuration(&environment)}),
        )?;
        self.start = services.clock.elapsed_ms();
        self.mark("generator_started");
        let mut environment = commands::clean(services.environment);
        environment.insert("RUST_LOG".into(), commands::generator_log(options).into());
        self.generator = Some(self.spawn(
            &options.generator,
            args,
            &options.candidate_root,
            environment,
            "generator.log",
        )?);
        if options.full_diagnostics() {
            self.sampler = Some(Sampler::new(options, self.directory.clone()));
        }
        self.load().await?;
        let (summary, raw) = summary::read(&self.directory.join("load_test_summary.json"))?;
        if let Some(timeline) = &mut self.timeline {
            timeline["workload"] =
                summary::lifecycle_workload(&raw, options, &self.context.manifest.generator_sha256)
                    .unwrap_or(Value::Null);
        }
        ensure!(
            !timeout_marker(&self.directory)? && summary.workload_passed(),
            "Incomplete or failing generator report"
        );
        if options.full_diagnostics() {
            ensure!(
                raw["run"]["configuration"]["diagnostics"] == true
                    && summary.diagnostic_failures == Some(0)
                    && raw["run"]["configuration"]["departure"]
                        == commands::departure(options.departure),
                "Missing or failing diagnostics"
            );
        }
        ensure!(
            raw["run"]["provenance"]["generatorBinarySha256"]
                == self.context.manifest.generator_sha256,
            "Foreign generator report"
        );
        let results = if options.subscription_plan == super::super::options::Plan::Fifo {
            Value::Null
        } else {
            artifacts::read_json(
                &self.directory.join("load_test_results.json"),
                32 * 1024 * 1024,
            )?
        };
        let graph = subscription::verify(&raw, &results, &options.into(), self.context.scenario)?;
        let finish = services.metrics.fetch(&self.origin, &self.token).await?;
        artifacts::new_file(
            &self.directory.join("metrics-finish.txt"),
            finish.raw.as_bytes(),
        )?;
        let cleanup_start = services.clock.elapsed_ms();
        let mut cleaned = false;
        for _ in 0..80 {
            self.poll().await?;
            if self.server.as_ref().unwrap().exit.is_some() {
                break;
            }
            let last = services.metrics.fetch(&self.origin, &self.token).await?;
            if last.cleaned() {
                cleaned = true;
                artifacts::new_file(
                    &self.directory.join("metrics-cleanup.txt"),
                    last.raw.as_bytes(),
                )?;
                break;
            }
            self.wait(Duration::from_millis(500)).await?;
        }
        self.poll().await?;
        ensure!(
            cleaned && self.server.as_ref().unwrap().exit.is_none(),
            "Server room/session cleanup did not complete within40 seconds"
        );
        self.mark("cleanup_observed");
        let mut row = json!({"purpose":options.purpose,"scenario":self.context.scenario.name,
            "variant":self.context.variant,"repetition":self.context.repetition,
            "startedAt":self.started_at,"workloadPassed":true,
            "cleanupMs":services.clock.elapsed_ms()-cleanup_start,"media":{
                "validatedConsumers":summary.validated_consumers,
                "failedConsumers":summary.failed_consumers,
                "skippedShortLivedConsumers":summary.skipped_short_lived_consumers}});
        row.as_object_mut()
            .unwrap()
            .extend(serde_json::to_value(graph)?.as_object().unwrap().clone());
        if options.purpose == Purpose::Diagnostic {
            row["diagnosticDetail"] = serde_json::to_value(options.diagnostic_detail)?;
            row["departure"] = serde_json::to_value(options.departure)?;
            row["completedAt"] = services.clock.utc().into();
        } else {
            let start = f64::from(options.ramp_up + options.warmup) * 1000.0;
            let end = start + f64::from(options.duration) * 1000.0;
            let server = resources::summarize(&self.samples, Role::Server, start, end)?;
            let generator = resources::summarize(&self.samples, Role::Generator, start, end)?;
            let values = json!({"joinP99Ms":summary.p99_connection_time_ms,
                "sendReadyP99Ms":summary.send_media_ready.p99_ms,
                "receiveReadyP99Ms":summary.receive_media_ready.p99_ms,
                "receivedPacketsPerSecond":summary.measurement.packets_received as f64
                    /(summary.measurement.duration_ms as f64/1000.0),
                "serverCpuPercent":server.cpu_percent_of_one_core,
                "serverPeakRssMiB":server.peak_rss_mi_b,
                "generatorCpuPercent":generator.cpu_percent_of_one_core,
                "generatorPeakRssMiB":generator.peak_rss_mi_b,
                "serverResources":server,"generatorResources":generator});
            row.as_object_mut()
                .unwrap()
                .extend(values.as_object().unwrap().clone());
        }
        self.row = Some(row);
        Ok(())
    }
}

pub(super) fn timeout_marker(directory: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(directory.join("load_test_timeout.json")) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => anyhow::bail!("Generator timeout marker unavailable"),
    }
}

/// Finish every owned process and artifact attempt before returning. A workload
/// failure wins over finalizer failures; no partially finalized row can pass.
pub async fn run_one(context: Context<'_>) -> Result<Value> {
    ensure!(
        ["baseline", "candidate"].contains(&context.variant),
        "Invalid benchmark variant"
    );
    let name = format!(
        "{}-{}-{}",
        context.scenario.name, context.repetition, context.variant
    );
    let directory = context.options.output.join(&name);
    artifacts::directory(&directory)?;
    let timeline_start = context.services.clock.elapsed_ms();
    let timeline = context.options.full_diagnostics().then(|| {
        json!({"schemaVersion":1,
        "departure":commands::departure(context.options.departure),"events":[],"workload":null})
    });
    let mut state = State {
        started_at: context.services.clock.utc(),
        origin: format!("http://127.0.0.1:{}", context.options.port),
        token: format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        ),
        context,
        directory,
        name,
        server: None,
        generator: None,
        capture: None,
        timeline,
        timeline_start,
        timeline_saved: false,
        sampler: None,
        pending_media: None,
        samples: Vec::new(),
        start: 0.0,
        row: None,
    };
    state.mark("run_started");
    let host = state.context.services.host;
    let primary = tokio::select! {
        result = state.workload() => result,
        _ = host.cancelled() => Err(anyhow::anyhow!("Benchmark interrupted")),
    };
    if primary.is_err() {
        let _ = artifacts::json(
            &state.directory.join("failure.json"),
            &json!({
            "startedAt":state.started_at,"error":"local_benchmark_failed",
            "server":state.server.as_ref().and_then(|owned|owned.exit.as_ref()),
            "generator":state.generator.as_ref().and_then(|owned|owned.exit.as_ref())}),
        );
    }
    let finalized = super::finalize::run(&mut state).await;
    primary?;
    finalized?;
    state
        .row
        .ok_or_else(|| anyhow::anyhow!("Benchmark row unavailable"))
}
