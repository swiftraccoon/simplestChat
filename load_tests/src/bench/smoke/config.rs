//! Fixed guest-only smoke profile. No caller supplies a service URL or credentials.

use anyhow::{Result, ensure};
use std::{
    collections::BTreeMap, ffi::OsString, os::unix::fs::DirBuilderExt, path::PathBuf,
    time::Duration,
};

pub(super) type Environment = BTreeMap<OsString, OsString>;
pub(super) const ACTIVE_MS: f64 = 150_000.0;
pub(super) const FINAL_MS: f64 = 240_000.0;
pub(super) const START: Duration = Duration::from_secs(20);
pub(super) const DRAIN: Duration = Duration::from_secs(20);
pub(super) const LOG_BYTES: u64 = 32_768;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    Shutdown,
    Diagnostics,
}
impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Shutdown => "shutdown-smoke",
            Self::Diagnostics => "diagnostics-smoke",
        }
    }
}
pub(super) struct Config {
    pub mode: Mode,
    pub binary: PathBuf,
    pub output: PathBuf,
    pub directory: PathBuf,
    pub environment: Environment,
    pub room: String,
}
fn path(value: &std::ffi::OsStr) -> Result<PathBuf> {
    let path = PathBuf::from(value);
    ensure!(
        path.is_absolute()
            && path.components().count() <= 20
            && path.components().all(|p| matches!(
                p,
                std::path::Component::RootDir | std::path::Component::Normal(_)
            ))
            && !path
                .as_os_str()
                .as_encoded_bytes()
                .iter()
                .any(|b| matches!(b, 0..=31 | 127)),
        "invalid_smoke_path"
    );
    Ok(path)
}
impl Config {
    pub fn parse(
        mode: Mode,
        arguments: &[OsString],
        directory: PathBuf,
        environment: Environment,
    ) -> Result<Self> {
        let mut binary = None;
        let mut output = None;
        ensure!(arguments.len().is_multiple_of(2), "invalid_smoke_arguments");
        for pair in arguments.as_chunks::<2>().0 {
            match pair[0].to_str() {
                Some("--binary") if binary.is_none() => binary = Some(path(&pair[1])?),
                Some("--output") if output.is_none() => output = Some(path(&pair[1])?),
                _ => anyhow::bail!("invalid_smoke_arguments"),
            }
        }
        let binary = binary.ok_or_else(|| anyhow::anyhow!("missing_smoke_binary"))?;
        let output = match output {
            Some(path) => path,
            None => {
                let parent = directory.join("results");
                // The default parent is repository-owned; every selected leaf
                // is still new,0700 and admitted without following links.
                if !parent.exists() {
                    std::fs::DirBuilder::new().mode(0o700).create(&parent)?;
                }
                path(
                    parent
                        .join(format!("{}.{}", mode.name(), uuid::Uuid::new_v4()))
                        .as_os_str(),
                )?
            }
        };
        ensure!(output.to_str().is_some(), "invalid_smoke_output_encoding");
        Ok(Self {
            mode,
            binary,
            output,
            directory,
            environment,
            room: format!("shutdown-smoke-{}", uuid::Uuid::new_v4()),
        })
    }
    pub fn server_environment(&self, ports: Ports) -> Environment {
        let mut result = Environment::from([(
            "PATH".into(),
            self.environment
                .get(&OsString::from("PATH"))
                .cloned()
                .unwrap_or_else(|| "/usr/bin:/bin".into()),
        )]);
        for (key, value) in [
            ("BIND_ADDR", "127.0.0.1".into()),
            ("PORT", ports.http.to_string()),
            ("ANNOUNCE_IP", "127.0.0.1".into()),
            ("MEDIA_WORKERS", "1".into()),
            ("WEBRTC_SERVER_PORT_BASE", ports.media.to_string()),
            ("ALLOWED_ORIGINS", ports.origin()),
            ("ALLOW_AD_HOC_ROOMS", "true".into()),
            ("REGISTRATION_ENABLED", "false".into()),
            ("RUN_MIGRATIONS", "false".into()),
            ("RUST_LOG", "simplestchat=info,mediasoup=warn".into()),
        ] {
            result.insert(key.into(), value.into());
        }
        if self.mode == Mode::Diagnostics {
            result.insert(
                "DIAGNOSTICS_PATH".into(),
                self.output.join("server-diagnostics.jsonl").into(),
            );
        }
        result
    }
}
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub(super) struct Ports {
    pub http: u16,
    pub media: u16,
}
impl Ports {
    pub fn validate(self) -> Result<Self> {
        ensure!(
            self.http >= 1000 && self.media >= 1000,
            "invalid_smoke_ports"
        );
        Ok(self)
    }
    pub fn origin(self) -> String {
        format!("http://127.0.0.1:{}", self.http)
    }
    pub fn websocket(self) -> String {
        format!("ws://127.0.0.1:{}/ws", self.http)
    }
}
