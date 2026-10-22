//! Bounded Linux observations of the owned, unreaped process identities. A
//! missing observation remains null and is judged by the pure coverage policy.

use super::{config::Config, database, policy};
use crate::bench::{
    clock::Clock,
    process::{self, Command, Host, Sink},
    resources::linux_sample,
};
use anyhow::{Result, ensure};
use nix::{
    fcntl::{OFlag, open},
    sys::stat::Mode,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::sync::OnceCell;

fn read(path: &Path, maximum: u64) -> Result<String> {
    let mut file = File::from(open(
        path,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?);
    let result = (|| -> Result<String> {
        ensure!(file.metadata()?.is_file(), "invalid_proc_observation");
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(maximum + 1)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() as u64 <= maximum, "proc_observation_limit");
        String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("invalid_proc_observation"))
    })();
    let closed = nix::unistd::close(file);
    let text = result?;
    closed?;
    Ok(text)
}

pub(super) fn process_tree(root: &Path, pid: u32, ticks: f64, page: f64) -> Result<Value> {
    let mut seen = BTreeSet::new();
    let mut pending = vec![pid];
    let mut processes = Vec::new();
    let (mut tasks, mut child_names, mut observed_bytes) = (0_usize, 0_usize, 0_usize);
    let (mut rss, mut cpu) = (0.0, 0.0);
    while let Some(pid) = pending.pop() {
        if !seen.insert(pid) {
            continue;
        }
        ensure!(pid > 1 && seen.len() <= 128, "server_process_tree_limit");
        let directory = root.join(pid.to_string());
        let stat = read(&directory.join("stat"), 65_536)?;
        observed_bytes += stat.len();
        ensure!(
            observed_bytes <= 4 * 1024 * 1024,
            "server_observation_byte_limit"
        );
        let (_, tail) = stat
            .rsplit_once(')')
            .ok_or_else(|| anyhow::anyhow!("invalid_server_process_stat"))?;
        let fields: Vec<_> = tail.split_whitespace().collect();
        let start = fields
            .get(19)
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v <= 9_007_199_254_740_991)
            .ok_or_else(|| anyhow::anyhow!("invalid_server_process_identity"))?;
        let sample = linux_sample(&stat, ticks, page)?;
        cpu += sample.cpu_seconds;
        rss += sample.rss_ki_b;
        processes.push(json!({"pid":pid,"startTicks":start}));
        let mut count = 0;
        for entry in std::fs::read_dir(directory.join("task"))? {
            count += 1;
            tasks += 1;
            ensure!(count <= 512 && tasks <= 4096, "server_task_limit");
            let name = entry?.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("invalid_server_task"))?;
            ensure!(
                !name.is_empty()
                    && name.bytes().all(|v| v.is_ascii_digit())
                    && name.parse::<u32>().ok().is_some_and(|v| v > 0),
                "invalid_server_task"
            );
            let children = read(&directory.join("task").join(name).join("children"), 65_536)?;
            observed_bytes += children.len();
            ensure!(
                observed_bytes <= 4 * 1024 * 1024,
                "server_observation_byte_limit"
            );
            for child in children.split_whitespace() {
                child_names += 1;
                ensure!(child_names <= 8192, "server_child_limit");
                let child = child
                    .parse::<u32>()
                    .map_err(|_| anyhow::anyhow!("invalid_server_child"))?;
                ensure!(child > 1 && pending.len() < 8192, "server_child_limit");
                pending.push(child);
            }
        }
        ensure!(count > 0, "server_tasks_missing");
    }
    processes.sort_by_key(|value| value["pid"].as_u64());
    Ok(
        json!({"rssKiB":rss,"cpuSeconds":cpu,"childCount":seen.len()-1,
        "processes":processes}),
    )
}

#[async_trait::async_trait]
pub(super) trait Observations: Send + Sync {
    async fn initialize(&self, server: u32) -> Result<()>;
    async fn sample(&self, server: u32, generator: u32) -> Value;
}

pub(super) struct NativeObservations<'a> {
    pub config: &'a Config,
    pub host: &'a dyn Host,
    pub clock: &'a dyn Clock,
    units: OnceCell<(f64, f64)>,
    cgroup: OnceCell<PathBuf>,
}
impl<'a> NativeObservations<'a> {
    pub fn new(config: &'a Config, host: &'a dyn Host, clock: &'a dyn Clock) -> Self {
        Self {
            config,
            host,
            clock,
            units: OnceCell::new(),
            cgroup: OnceCell::new(),
        }
    }
    async fn unit(&self, name: &str) -> Result<f64> {
        let executable =
            database::find(&self.config.environment, &self.config.directory, "getconf")?;
        let output = process::run(
            self.host,
            Command {
                executable,
                arguments: vec![name.into()],
                directory: self.config.directory.clone(),
                environment: self.config.path_environment(),
                timeout: self
                    .config
                    .bound(self.clock.elapsed_ms(), Duration::from_secs(2))?,
                stdout_limit: 4096,
                stderr_limit: 4096,
                input: Vec::new(),
                stdout: Sink::Capture,
                stderr: Sink::Capture,
                cleanup: false,
            },
        )
        .await?;
        ensure!(output.exit.success(), "invalid_resource_units");
        let value = std::str::from_utf8(&output.stdout)?.trim().parse::<f64>()?;
        ensure!(value.is_finite() && value > 0.0, "invalid_resource_units");
        Ok(value)
    }
}
#[async_trait::async_trait]
impl Observations for NativeObservations<'_> {
    async fn initialize(&self, server: u32) -> Result<()> {
        self.units
            .get_or_try_init(|| async {
                let (ticks, page) = tokio::join!(self.unit("CLK_TCK"), self.unit("PAGESIZE"));
                Ok::<_, anyhow::Error>((ticks?, page?))
            })
            .await?;
        self.cgroup
            .get_or_try_init(|| async {
                let source = read(&PathBuf::from(format!("/proc/{server}/cgroup")), 65_536)?;
                let value = source
                    .lines()
                    .find_map(|line| line.strip_prefix("0::"))
                    .ok_or_else(|| anyhow::anyhow!("unified_server_cgroup_required"))?;
                ensure!(
                    value.starts_with('/') && !value.split('/').any(|v| v == ".."),
                    "unified_server_cgroup_required"
                );
                Ok::<_, anyhow::Error>(
                    Path::new("/sys/fs/cgroup").join(value.trim_start_matches('/')),
                )
            })
            .await?;
        Ok(())
    }
    async fn sample(&self, server: u32, generator: u32) -> Value {
        let Some(&(ticks, page)) = self.units.get() else {
            return Value::Null;
        };
        let server = process_tree(Path::new("/proc"), server, ticks, page).ok();
        let generator = read(&PathBuf::from(format!("/proc/{generator}/stat")), 65_536)
            .ok()
            .and_then(|stat| linux_sample(&stat, ticks, page).ok())
            .and_then(|sample| serde_json::to_value(sample).ok());
        let host = (|| -> Result<Value> {
            policy::parse_host(
                &read(Path::new("/proc/stat"), 1024 * 1024)?,
                &read(Path::new("/proc/meminfo"), 65_536)?,
            )
        })()
        .ok();
        let cgroup = self.cgroup.get().and_then(|root| {
            policy::parse_cgroup(
                &read(&root.join("cpu.stat"), 65_536).ok()?,
                &read(&root.join("cpu.max"), 4096).ok()?,
            )
            .ok()
        });
        json!({"server":server,"generator":generator,"host":host,"cgroup":cgroup})
    }
}
