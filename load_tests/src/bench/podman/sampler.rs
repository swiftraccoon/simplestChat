//! Bounded long-lived exec streams retain sample arrival times and partial failures.

use super::{
    engine::{Engine, MIB},
    parsers::{self, ServerSample},
};
use crate::bench::{
    clock::Clock,
    process::{Child, Sink, Stream},
    resources::{ProcessSample, Sample},
};
use anyhow::{Result, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::JoinHandle;

pub(super) const SERVER: &str = concat!(
    "while :; do cat /proc/1/stat; echo @@S; ",
    "cat /sys/fs/cgroup/cpu.stat; echo @@Y; cat /proc/1/task/*/stat; echo @@E; sleep 0.5; done"
);
pub(super) const GENERATOR: &str = "while :; do cat /proc/1/stat; echo @@E; sleep 0.5; done";
pub(super) const MEMORY: &str = concat!(
    "while :; do cat /proc/1/smaps_rollup; echo @@M; ",
    "awk '/^[0-9a-f]+-[0-9a-f]+ /{range=$1; name=$6} ",
    "/^Rss:/{if ($2 >= 2048) print range, $2, name}' ",
    "/proc/1/smaps; echo @@C; cat /sys/fs/cgroup/memory.stat; echo @@E; sleep 5; done"
);
const MAX_BLOCK: usize = 2 * 1024 * 1024;
const MAX_SAMPLES: usize = 8192;

#[derive(Clone, Copy)]
pub(super) enum Kind {
    Server,
    Generator,
    Memory,
}
impl Kind {
    fn script(self) -> &'static str {
        match self {
            Self::Server => SERVER,
            Self::Generator => GENERATOR,
            Self::Memory => MEMORY,
        }
    }
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct GeneratorSample {
    pub(super) elapsed_ms: f64,
    pub(super) generator: Option<ProcessSample>,
}

#[derive(Default, Serialize)]
pub(super) struct Data {
    pub(super) server: Vec<ServerSample>,
    pub(super) generator: Vec<GeneratorSample>,
    pub(super) memory: Vec<Value>,
}
impl Data {
    pub(super) fn resource_samples(&self) -> (Vec<Sample>, Vec<Sample>) {
        (
            self.server
                .iter()
                .map(|row| Sample {
                    elapsed_ms: row.elapsed_ms,
                    server: row.server,
                    generator: None,
                })
                .collect(),
            self.generator
                .iter()
                .map(|row| Sample {
                    elapsed_ms: row.elapsed_ms,
                    server: None,
                    generator: row.generator,
                })
                .collect(),
        )
    }
}

pub(super) struct Configuration {
    pub(super) started: f64,
    pub(super) ticks: f64,
    pub(super) page: f64,
    pub(super) data: Arc<Mutex<Data>>,
    pub(super) duration: Duration,
}

pub(super) struct Sampler {
    child: Box<dyn Child>,
    task: Option<JoinHandle<Result<()>>>,
}
impl Sampler {
    pub(super) fn start(
        engine: &Engine,
        id: &str,
        kind: Kind,
        config: Configuration,
    ) -> Result<Self> {
        let Configuration {
            started,
            ticks,
            page,
            data,
            duration,
        } = config;
        let mut child = engine.spawn(
            &[
                "exec".into(),
                id.into(),
                "sh".into(),
                "-c".into(),
                kind.script().into(),
            ],
            duration,
            64 * MIB,
            Sink::Stream,
            Sink::Capture,
            false,
        )?;
        let stream = child
            .take_stdout()
            .ok_or_else(|| anyhow::anyhow!("Owned sampler stdout unavailable"))?;
        let clock = engine.clock.clone();
        let task = tokio::spawn(read(stream, clock, kind, started, ticks, page, data));
        Ok(Self {
            child,
            task: Some(task),
        })
    }
    pub(super) async fn check(&mut self) -> Result<()> {
        if self.task.as_ref().is_some_and(JoinHandle::is_finished) {
            self.task.take().unwrap().await??;
        }
        // A sampler that closes normally can still have partial observations;
        // the common resource-window coverage gate decides whether they suffice.
        self.child.poll().await?;
        Ok(())
    }
    pub(super) async fn stop(&mut self) -> Result<()> {
        let stopped = self.child.stop(Duration::from_secs(2)).await;
        let reader = if let Some(mut task) = self.task.take() {
            match tokio::time::timeout(Duration::from_secs(5), &mut task).await {
                Ok(result) => result
                    .map_err(anyhow::Error::from)
                    .and_then(|result| result),
                Err(_) => {
                    task.abort();
                    let _ = task.await;
                    Err(anyhow::anyhow!("Owned sampler did not close"))
                }
            }
        } else {
            Ok(())
        };
        stopped?;
        reader
    }
}
impl Drop for Sampler {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

async fn read(
    mut stream: Box<dyn Stream>,
    clock: Arc<dyn Clock>,
    kind: Kind,
    started: f64,
    ticks: f64,
    page: f64,
    data: Arc<Mutex<Data>>,
) -> Result<()> {
    let mut chunk = vec![0; 65536];
    let mut buffer = Vec::new();
    loop {
        let count = stream.read(&mut chunk).await?;
        if count == 0 {
            return Ok(());
        }
        for byte in &chunk[..count] {
            buffer.push(*byte);
            ensure!(
                buffer.len() <= MAX_BLOCK,
                "Owned sampler block is too large"
            );
            if buffer.ends_with(b"@@E\n") {
                let block = String::from_utf8_lossy(&buffer[..buffer.len() - 4]);
                record(
                    &mut *data
                        .lock()
                        .map_err(|_| anyhow::anyhow!("Owned sampler state unavailable"))?,
                    kind,
                    clock.elapsed_ms() - started,
                    &block,
                    ticks,
                    page,
                )?;
                buffer.clear();
            }
        }
    }
}

pub(super) fn record(
    data: &mut Data,
    kind: Kind,
    elapsed: f64,
    block: &str,
    ticks: f64,
    page: f64,
) -> Result<()> {
    match kind {
        Kind::Server => {
            ensure!(
                data.server.len() < MAX_SAMPLES,
                "Too many owned server samples"
            );
            let value = parsers::server_block(block, ticks, page).ok();
            data.server.push(ServerSample {
                elapsed_ms: elapsed,
                server: value.as_ref().map(|v| v.server),
                server_detail: value.map(|v| v.server_detail),
            });
        }
        Kind::Generator => {
            ensure!(
                data.generator.len() < MAX_SAMPLES,
                "Too many owned generator samples"
            );
            data.generator.push(GeneratorSample {
                elapsed_ms: elapsed,
                generator: crate::bench::resources::linux_sample(block, ticks, page).ok(),
            });
        }
        Kind::Memory => {
            ensure!(
                data.memory.len() < MAX_SAMPLES,
                "Too many owned memory samples"
            );
            let mut value = json!({"elapsedMs":elapsed});
            match parsers::memory_block(block) {
                Ok(block) => value
                    .as_object_mut()
                    .unwrap()
                    .extend(serde_json::to_value(block)?.as_object().unwrap().clone()),
                Err(_) => {
                    value["rssKiB"] = Value::Null;
                }
            }
            data.memory.push(value);
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "sampler_tests.rs"]
mod tests;
