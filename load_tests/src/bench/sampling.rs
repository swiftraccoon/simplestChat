//! Resource observations for an owned, unreaped process identity. Failed
//! observations remain missing samples and are judged by the coverage policy.

use super::process::{Command, Host, Sink};
use super::resources::{ProcessSample, cpu_seconds, linux_sample};
use anyhow::{Result, ensure};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::OnceCell;

#[async_trait::async_trait]
pub trait Resources: Send + Sync {
    async fn sample(&self, pid: u32) -> Option<ProcessSample>;
}

pub struct NativeResources<'a> {
    host: &'a dyn Host,
    directory: PathBuf,
    environment: BTreeMap<OsString, OsString>,
    units: OnceCell<(f64, f64)>,
}

impl<'a> NativeResources<'a> {
    pub fn new(
        host: &'a dyn Host,
        directory: PathBuf,
        environment: &BTreeMap<OsString, OsString>,
    ) -> Self {
        let mut environment = super::commands::clean(environment);
        environment.insert("LC_ALL".into(), "C".into());
        Self {
            host,
            directory,
            environment,
            units: OnceCell::new(),
        }
    }

    async fn execute(&self, program: &str, arguments: Vec<String>) -> Result<String> {
        let result = super::process::run(
            self.host,
            Command {
                executable: program.into(),
                arguments,
                directory: self.directory.clone(),
                environment: self.environment.clone(),
                timeout: Duration::from_secs(2),
                stdout_limit: 64 * 1024,
                stderr_limit: 64 * 1024,
                input: Vec::new(),
                stdout: Sink::Capture,
                stderr: Sink::Capture,
                cleanup: false,
            },
        )
        .await?;
        ensure!(result.exit.success(), "Resource observation unavailable");
        String::from_utf8(result.stdout)
            .map_err(|_| anyhow::anyhow!("Invalid resource observation"))
    }

    async fn linux(&self, pid: u32) -> Result<ProcessSample> {
        let &(ticks, page) = self
            .units
            .get_or_try_init(|| async {
                let (ticks, page) = tokio::try_join!(
                    self.execute("getconf", vec!["CLK_TCK".into()]),
                    self.execute("getconf", vec!["PAGESIZE".into()]),
                )?;
                let ticks = super::options::number(ticks.trim());
                let page = super::options::number(page.trim());
                match (ticks, page) {
                    (Some(ticks), Some(page)) if ticks > 0.0 && page > 0.0 => Ok((ticks, page)),
                    _ => anyhow::bail!("Invalid Linux resource units"),
                }
            })
            .await?;
        let stat = tokio::fs::read(format!("/proc/{pid}/stat")).await?;
        ensure!(stat.len() <= 64 * 1024, "Resource observation limit");
        linux_sample(std::str::from_utf8(&stat)?, ticks, page)
    }

    async fn ps(&self, pid: u32) -> Result<ProcessSample> {
        let text = self
            .execute(
                "ps",
                vec![
                    "-p".into(),
                    pid.to_string(),
                    "-o".into(),
                    "rss=,time=".into(),
                ],
            )
            .await?;
        let mut fields = text.split_whitespace();
        let rss = fields.next().and_then(super::options::number);
        let cpu = fields.next().map(cpu_seconds).transpose()?;
        match (rss, cpu) {
            (Some(rss_ki_b), Some(cpu_seconds)) => Ok(ProcessSample {
                rss_ki_b,
                cpu_seconds,
            }),
            _ => anyhow::bail!("Invalid resource observation"),
        }
    }
}

#[async_trait::async_trait]
impl Resources for NativeResources<'_> {
    async fn sample(&self, pid: u32) -> Option<ProcessSample> {
        if cfg!(target_os = "linux") {
            self.linux(pid).await.ok()
        } else {
            self.ps(pid).await.ok()
        }
    }
}

pub trait Ports: Send + Sync {
    /// Probe only the selected local TCP listener and wildcard media UDP ports.
    /// Probe handles are always closed before any server process is launched.
    fn available(&self, port: u16, media_port: u16, workers: u32) -> Result<()>;
}

pub struct NativePorts;

impl Ports for NativePorts {
    fn available(&self, port: u16, media_port: u16, workers: u32) -> Result<()> {
        ensure!(
            (1..=4).contains(&workers),
            "Invalid owned media worker count"
        );
        drop(
            std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
                .map_err(|_| anyhow::anyhow!("Owned TCP port unavailable"))?,
        );
        for offset in 0..workers {
            let port = u32::from(media_port)
                .checked_add(offset)
                .and_then(|port| u16::try_from(port).ok())
                .ok_or_else(|| anyhow::anyhow!("Invalid owned media port"))?;
            drop(
                std::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, port))
                    .map_err(|_| anyhow::anyhow!("Owned media port unavailable"))?,
            );
        }
        Ok(())
    }
}
