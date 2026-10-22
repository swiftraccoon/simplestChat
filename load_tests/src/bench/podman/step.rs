//! One owned comparison: admission, sampling and workload evidence precede shutdown gates.

use super::{
    artifacts as transfer, commands,
    engine::{Engine, State, valid_id},
    netem,
    options::{Options, Workload},
    ownership::Ownership,
    parsers, report,
    sampler::{self, Data, Kind, Sampler},
};
use crate::bench::{artifacts, metrics::Metrics, options::Plan, subscription, summary};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub(super) struct Context<'a> {
    pub(super) options: &'a Options,
    pub(super) scenario: &'a Workload,
    pub(super) repetition: u32,
    pub(super) manifest: &'a Value,
    pub(super) engine: &'a Engine,
    pub(super) metrics: &'a dyn Metrics,
}

pub(super) struct Run<'a> {
    pub(super) context: Context<'a>,
    pub(super) directory: PathBuf,
    pub(super) ownership: Ownership,
    pub(super) id: String,
    pub(super) server: String,
    pub(super) generator: String,
    pub(super) capture: String,
    pub(super) started_at: String,
    pub(super) generator_started: bool,
    pub(super) generator_exit: Option<State>,
    pub(super) server_exit: Option<State>,
    pub(super) result: Option<Value>,
    pub(super) collected: bool,
    pub(super) capture_collected: bool,
    pub(super) samplers: Vec<Sampler>,
    pub(super) data: Arc<Mutex<Data>>,
}

pub(super) async fn run(context: Context<'_>) -> Result<Value> {
    let name = format!(
        "{}-{}{}",
        context.scenario.name,
        context.repetition,
        if context.options.netem.is_some() {
            "-netem"
        } else {
            ""
        }
    );
    let directory = context.options.output.join(&name);
    artifacts::directory(&directory)?;
    let id = hex::encode(rand::random::<[u8; 4]>());
    let mut run = Run {
        ownership: Ownership::new(&directory, &id),
        directory,
        server: format!("bench-sfu-{id}"),
        generator: format!("bench-gen-{id}"),
        capture: format!("bench-cap-{id}"),
        id,
        started_at: context.engine.clock.utc(),
        context,
        generator_started: false,
        generator_exit: None,
        server_exit: None,
        result: None,
        collected: false,
        capture_collected: false,
        samplers: Vec::new(),
        data: Arc::new(Mutex::new(Data::default())),
    };
    let primary = run.execute(&name).await.err();
    let cleanup = super::finalize::run(&mut run).await;
    if primary.is_some() || !cleanup.is_empty() {
        let message = primary
            .as_ref()
            .map_or_else(|| cleanup.join("; "), |error| format!("{error:#}"));
        let evidence = json!({"startedAt":run.started_at, "error":message,
            "generatorExit":run.generator_exit, "generatorStarted":run.generator_started,
            "cleanupErrors":cleanup});
        let saved = artifacts::json(&run.directory.join("failure.json"), &evidence);
        if let Err(error) = saved {
            return Err(anyhow::anyhow!("{message}; failure evidence: {error}"));
        }
        anyhow::bail!("{message}");
    }
    run.result
        .ok_or_else(|| anyhow::anyhow!("Owned benchmark completed without admitted evidence"))
}

impl Run<'_> {
    pub(super) async fn create(&self, name: &str, image: &str, args: &[String]) -> Result<String> {
        self.ownership.remember(name, image, "")?;
        let id = self
            .context
            .engine
            .run(args, 120, false)
            .await?
            .trim()
            .to_owned();
        ensure!(valid_id(&id), "Engine returned an invalid container ID");
        self.ownership.remember(name, image, &id)?;
        Ok(id)
    }
    async fn sidecar(&self, name: &str, image: &str, args: &[String]) -> Result<String> {
        self.ownership.remember(name, image, "")?;
        self.context.engine.run(args, 60, false).await
    }
    pub(super) async fn pause(&self, duration: Duration) -> Result<()> {
        tokio::select! {
            _=self.context.engine.host.cancelled()=>anyhow::bail!("Benchmark interrupted"),
            _=self.context.engine.clock.sleep(duration)=>Ok(()),
        }
    }
    pub(super) async fn freeze(&self, name: &str, grace: u64) -> Result<Option<(String, State)>> {
        let engine = self.context.engine;
        let Some(id) = self.ownership.resolve(engine, name).await? else {
            return Ok(None);
        };
        let state = engine.state(&id, true).await?;
        if state.as_ref().is_none_or(|state| !state.terminal()) {
            self.ownership.stop(engine, name, grace).await?;
        }
        let state = engine
            .state(&id, true)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Stopped owned container state is unavailable"))?;
        ensure!(
            state.terminal(),
            "Owned container did not enter a terminal state"
        );
        Ok(Some((id, state)))
    }
    pub(super) async fn collect_generator(&mut self) -> Result<()> {
        if self.collected {
            return Ok(());
        }
        let Some((id, state)) = self.freeze(&self.generator, 5).await? else {
            return Ok(());
        };
        self.generator_exit = Some(state);
        // A failed transfer is never retried onto partially written evidence.
        self.collected = true;
        transfer::collect_generator(self.context.engine, &id, &self.directory).await?;
        Ok(())
    }
    pub(super) async fn collect_capture(&mut self) -> Result<()> {
        if self.capture_collected {
            return Ok(());
        }
        let Some((id, _)) = self.freeze(&self.capture, 5).await? else {
            return Ok(());
        };
        self.capture_collected = true;
        transfer::collect_capture(self.context.engine, &id, &self.directory).await?;
        Ok(())
    }
    async fn execute(&mut self, name: &str) -> Result<()> {
        let options = self.context.options;
        let scenario = self.context.scenario;
        let engine = self.context.engine;
        let metrics = self.context.metrics;
        let manifest = self.context.manifest;
        let repetition = self.context.repetition;
        let token = hex::encode(rand::random::<[u8; 32]>());
        let origin = format!("http://127.0.0.1:{}", options.port);
        let env = commands::environment(options, scenario, &token)?;
        let server = self
            .create(
                &self.server,
                &options.server_image,
                &commands::server(options, &self.server, &self.id, &env),
            )
            .await?;
        let ticks = number_or(engine.exec(&server, "getconf CLK_TCK").await?.trim(), 100.0);
        let page = number_or(
            engine.exec(&server, "getconf PAGESIZE").await?.trim(),
            4096.0,
        );
        let mut ready = false;
        for _ in 0..200 {
            if metrics.healthy(&origin).await.unwrap_or(false) {
                ready = true;
                break;
            }
            self.pause(Duration::from_millis(250)).await?;
        }
        ensure!(ready, "Owned server container did not become ready");
        if let Some(parameters) = &options.netem {
            let script = netem::script(
                parameters,
                &options.netem_scope,
                options.udp_port,
                options.workers,
            )?;
            let sidecar = format!("bench-netem-{}", self.id);
            let shown = self
                .sidecar(
                    &sidecar,
                    &options.netem_image,
                    &commands::netem(options, &sidecar, &self.id, &server, &script),
                )
                .await?;
            artifacts::new_file(
                &self.directory.join("netem.txt"),
                format!("{script}\n---\n{shown}").as_bytes(),
            )?;
        }
        let before = metrics.fetch(&origin, &token).await?;
        artifacts::new_file(
            &self.directory.join("metrics-before.txt"),
            before.raw.as_bytes(),
        )?;
        let room = format!("benchmark-{}", hex::encode(rand::random::<[u8; 6]>()));
        let args = commands::generator_args(
            options,
            scenario,
            name,
            &room,
            (
                manifest["server"]["id"].as_str().unwrap_or(""),
                manifest["generator"]["id"].as_str().unwrap_or(""),
            ),
        )?;
        if options.capture_handshakes {
            self.create(
                &self.capture,
                &options.capture_image,
                &commands::capture(options, &self.capture, &self.id, &server),
            )
            .await?;
        }
        let configuration: std::collections::BTreeMap<_, _> = env
            .iter()
            .filter(|(key, _)| key.as_str() != "METRICS_TOKEN")
            .collect();
        artifacts::json(
            &self.directory.join("invocation.json"),
            &json!({
            "startedAt":self.started_at,"scenario":scenario,"repetition":repetition,
            "args":args,"netem":options.netem,
            "netemScope":options.netem.as_ref().map(|_|&options.netem_scope),
            "captureHandshakes":options.capture_handshakes,"serverConfiguration":configuration,
            "limits":{"cpus":options.cpus,"memory":options.memory,
                "generatorCpus":options.generator_cpus}}),
        )?;
        let start = engine.clock.elapsed_ms();
        let duration =
            Duration::from_secs((options.ramp_up + options.warmup + options.duration + 180) as u64);
        for kind in [Kind::Server, Kind::Memory] {
            self.samplers.push(Sampler::start(
                engine,
                &server,
                kind,
                sampler::Configuration {
                    started: start,
                    ticks,
                    page,
                    data: self.data.clone(),
                    duration,
                },
            )?);
        }
        let generator = self
            .create(
                &self.generator,
                &options.generator_image,
                &commands::generator(options, &self.generator, &self.id, &server, &args),
            )
            .await?;
        self.generator_started = true;
        self.samplers.push(Sampler::start(
            engine,
            &generator,
            Kind::Generator,
            sampler::Configuration {
                started: start,
                ticks,
                page,
                data: self.data.clone(),
                duration,
            },
        )?);
        self.watch(&server, &generator, &origin, &token, start)
            .await?;
        // Close the generator sampler before freezing and copying its files.
        self.samplers.last_mut().unwrap().stop().await?;
        self.collect_generator().await?;
        let mut udp = Value::Null;
        if options.capture_handshakes {
            self.collect_capture().await?;
            let sidecar = format!("bench-udp-{}", self.id);
            let counters = self
                .sidecar(
                    &sidecar,
                    &options.capture_image,
                    &commands::counters(options, &sidecar, &self.id, &server),
                )
                .await?;
            artifacts::new_file(&self.directory.join("netns-udp.txt"), counters.as_bytes())?;
            udp = serde_json::to_value(parsers::netns_udp(
                &counters,
                &(0..options.workers)
                    .map(|n| options.udp_port + n as u16)
                    .collect::<Vec<_>>(),
            )?)?;
        }
        ensure!(
            self.generator_exit
                .as_ref()
                .is_some_and(|state| state.code == Some(0)),
            "Generator failed: {}",
            serde_json::to_string(&self.generator_exit)?
        );
        let (summary, raw) = summary::read(&self.directory.join("load_test_summary.json"))?;
        ensure!(
            summary.workload_passed(),
            "Incomplete or failing generator report"
        );
        let results = if options.subscription_plan == Plan::Fifo {
            Value::Null
        } else {
            artifacts::read_json(
                &self.directory.join("load_test_results.json"),
                128 * 1024 * 1024,
            )?
        };
        let identity =
            subscription::verify(&raw, &results, &options.graph(), &scenario.scenario())?;
        let finish = self.context.metrics.fetch(&origin, &token).await?;
        artifacts::new_file(
            &self.directory.join("metrics-finish.txt"),
            finish.raw.as_bytes(),
        )?;
        let cleanup_start = engine.clock.elapsed_ms();
        let mut cleaned = None;
        for _ in 0..80 {
            let snapshot = self.context.metrics.fetch(&origin, &token).await?;
            if snapshot.cleaned() {
                cleaned = Some(snapshot);
                break;
            }
            self.pause(Duration::from_millis(500)).await?;
        }
        let cleaned = cleaned.ok_or_else(|| {
            anyhow::anyhow!("Server room/session cleanup did not complete within 40 seconds")
        })?;
        artifacts::new_file(
            &self.directory.join("metrics-cleanup.txt"),
            cleaned.raw.as_bytes(),
        )?;
        let data = self
            .data
            .lock()
            .map_err(|_| anyhow::anyhow!("Owned sampler state unavailable"))?;
        self.result = Some(report::row(
            &data,
            report::Evidence {
                options,
                scenario,
                repetition: self.context.repetition,
                started_at: &self.started_at,
                ticks,
                summary: &summary,
                subscription: identity,
                finish: &finish.values,
                cleanup_ms: engine.clock.elapsed_ms() - cleanup_start,
                udp,
            },
        )?);
        Ok(())
    }
    async fn watch(
        &mut self,
        server: &str,
        generator: &str,
        origin: &str,
        token: &str,
        start: f64,
    ) -> Result<()> {
        let options = self.context.options;
        let deadline = (options.ramp_up + options.warmup + options.duration + 135) as f64 * 1000.0;
        let scrape_at = (options.ramp_up + options.warmup + options.duration - 5) as f64 * 1000.0;
        let mut scraped = false;
        loop {
            for sampler in &mut self.samplers {
                sampler.check().await?;
            }
            let generator_state = self.context.engine.state(generator, false).await?;
            let server_state = self.context.engine.state(server, false).await?;
            let elapsed = self.context.engine.clock.elapsed_ms() - start;
            if generator_state
                .as_ref()
                .is_none_or(|state| state.status != "running")
            {
                self.generator_exit = generator_state;
                break;
            }
            ensure!(
                server_state.is_some_and(|state| state.status == "running"),
                "Server container exited during load"
            );
            ensure!(elapsed <= deadline, "Outer generator deadline exceeded");
            if !scraped && elapsed >= scrape_at {
                scraped = true;
                let snapshot = self.context.metrics.fetch(origin, token).await?;
                artifacts::new_file(
                    &self.directory.join("metrics-during.txt"),
                    snapshot.raw.as_bytes(),
                )?;
            }
            self.pause(Duration::from_secs(1)).await?;
        }
        Ok(())
    }
}

fn number_or(text: &str, fallback: f64) -> f64 {
    crate::bench::options::number(text)
        .filter(|value| *value > 0.0)
        .unwrap_or(fallback)
}
