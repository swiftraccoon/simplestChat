//! Fixed guest workload and explicit authority for the disposable Linux loopback.

use super::netem::Profile;
use crate::bench::provenance;
use anyhow::{Result, ensure};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(super) type Environment = BTreeMap<OsString, OsString>;
pub(super) const PORT: u16 = 3129;
pub(super) const MEDIA_PORT: u16 = 41100;
pub(super) const LOG_LIMIT: u64 = 64 * 1024 * 1024;
pub(super) const CLI_LIMIT: u64 = 64 * 1024;
pub(super) const RESERVE: Duration = Duration::from_secs(90);

pub(super) struct Selected {
    pub path: PathBuf,
    pub hash: String,
}

impl Selected {
    pub fn open(path: PathBuf) -> Result<Self> {
        ensure!(
            path.metadata()
                .is_ok_and(|value| value.is_file() && value.permissions().mode() & 0o111 != 0),
            "Impaired executable unavailable"
        );
        let hash = provenance::hash_file(&path)?.0;
        Ok(Self { path, hash })
    }
    pub fn verify(&self) -> Result<()> {
        ensure!(
            self.path
                .metadata()
                .is_ok_and(|value| value.is_file() && value.permissions().mode() & 0o111 != 0),
            "Impaired executable unavailable"
        );
        provenance::verify_executable(&self.path, &self.hash, "Selected impaired")
    }
}

pub(super) struct Plan {
    pub root: PathBuf,
    pub output: PathBuf,
    pub environment: Environment,
    pub server: Selected,
    pub generator: Selected,
    pub tc: Selected,
    pub sudo: Selected,
    pub clients: u32,
    pub duration: u32,
    pub profile: Profile,
    pub room: String,
}

pub(super) fn text<'a>(environment: &'a Environment, key: &str) -> Option<&'a str> {
    environment.get(OsStr::new(key)).and_then(|v| v.to_str())
}

pub(super) fn absolute(root: &Path, value: &OsStr) -> PathBuf {
    let value = Path::new(value);
    if value.is_absolute() {
        value.to_owned()
    } else {
        root.join(value)
    }
}

pub(super) fn tool(root: &Path, environment: &Environment, name: &str) -> Result<Selected> {
    let path = environment
        .get(OsStr::new("PATH"))
        .cloned()
        .unwrap_or_default();
    let path = std::env::split_paths(&path)
        .map(|directory| absolute(root, directory.as_os_str()).join(name))
        .find(|path| {
            path.metadata()
                .is_ok_and(|value| value.is_file() && value.permissions().mode() & 0o111 != 0)
        })
        .ok_or_else(|| anyhow::anyhow!("Impaired executable unavailable"))?;
    Selected::open(path)
}

fn number(environment: &Environment, key: &str, default: u32, maximum: u32) -> Result<u32> {
    match text(environment, key).filter(|value| !value.is_empty()) {
        None => Ok(default),
        Some(value) => {
            ensure!(
                value.bytes().all(|byte| byte.is_ascii_digit()),
                "Invalid impaired options"
            );
            let value = value.parse::<u32>()?;
            ensure!((1..=maximum).contains(&value), "Invalid impaired options");
            Ok(value)
        }
    }
}

impl Plan {
    pub fn parse(
        root: &Path,
        arguments: &[OsString],
        environment: Environment,
        system: &str,
    ) -> Result<Self> {
        let authorized = system == "linux"
            && if text(&environment, "ACT") == Some("true") {
                text(&environment, "LOCAL_CI_DISPOSABLE") == Some("1")
            } else {
                text(&environment, "GITHUB_ACTIONS") == Some("true")
                    && text(&environment, "RUNNER_ENVIRONMENT") == Some("github-hosted")
            };
        ensure!(
            authorized,
            "Impaired loopback requires an explicit disposable Linux runner"
        );
        let mut values = BTreeMap::new();
        for pair in arguments.chunks(2) {
            ensure!(pair.len() == 2, "Invalid impaired options");
            let key = pair[0].to_str().unwrap_or("");
            ensure!(
                matches!(key, "--output" | "--server-bin" | "--generator"),
                "Invalid impaired options"
            );
            ensure!(
                !pair[1].is_empty() && values.insert(key, &pair[1]).is_none(),
                "Invalid impaired options"
            );
        }
        let output = values
            .get("--output")
            .ok_or_else(|| anyhow::anyhow!("Output required"))?;
        let output = absolute(root, output);
        ensure!(
            output.file_name().is_some()
                && !output.exists()
                && output.parent().is_some_and(Path::is_dir),
            "Fresh impaired output required"
        );
        let choose = |key: &str, variable: &str, default: &str| {
            values
                .get(key)
                .map(|v| v.as_os_str())
                .or_else(|| {
                    environment
                        .get(OsStr::new(variable))
                        .map(OsString::as_os_str)
                })
                .filter(|v| !v.is_empty())
                .map_or_else(|| root.join(default), |v| absolute(root, v))
        };
        let server = Selected::open(choose(
            "--server-bin",
            "SIMPLESTCHAT_BIN",
            "target/release/simplestChat",
        ))?;
        let generator = Selected::open(choose(
            "--generator",
            "LOAD_TEST_BIN",
            "target/release/load_test",
        ))?;
        let tc = tool(root, &environment, "tc")?;
        let sudo = tool(root, &environment, "sudo")?;
        let clients = number(&environment, "CLIENTS", 30, 10_000)?;
        let duration = number(&environment, "DURATION", 60, 3_300)?;
        ensure!(duration >= 3, "Invalid impaired options");
        let mut settings = environment.clone();
        for (key, default) in [
            ("IMPAIR_DIRECTION", "both"),
            ("IMPAIR_LOSS", "5"),
            ("IMPAIR_DELAY_MS", "50"),
            ("IMPAIR_JITTER_MS", "10"),
        ] {
            if text(&settings, key).is_none_or(str::is_empty) {
                settings.insert(key.into(), default.into());
            }
        }
        settings.insert("IMPAIR_UDP_PORT".into(), MEDIA_PORT.to_string().into());
        settings.insert("IMPAIR_WORKERS".into(), "1".into());
        let profile = Profile::parse(&settings)?;
        ensure!(
            [
                root,
                &output,
                &server.path,
                &generator.path,
                &tc.path,
                &sudo.path
            ]
            .iter()
            .all(|path| path.to_str().is_some_and(|v| v.len() <= 4096)),
            "Invalid impaired path"
        );
        Ok(Self {
            root: root.to_owned(),
            output,
            environment: settings,
            server,
            generator,
            tc,
            sudo,
            clients,
            duration,
            profile,
            room: format!("impaired-{}", uuid::Uuid::new_v4().simple()),
        })
    }

    pub fn budget(&self) -> Duration {
        Duration::from_secs(u64::from(self.duration) + 250)
    }
    pub fn generator_budget(&self) -> Duration {
        Duration::from_secs(u64::from(self.duration) + 110)
    }
    pub fn base(&self) -> Environment {
        ["PATH", "HOME", "TMPDIR"]
            .into_iter()
            .filter_map(|key| self.environment.get_key_value(OsStr::new(key)))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }
    pub fn server_environment(&self) -> Environment {
        let mut result = self.base();
        result
            .entry("TMPDIR".into())
            .or_insert_with(|| "/tmp".into());
        if let Some(value) = self
            .environment
            .get(OsStr::new("LIBWEBRTC_FIELD_TRIALS"))
            .filter(|value| !value.is_empty())
        {
            result.insert("LIBWEBRTC_FIELD_TRIALS".into(), value.clone());
        }
        for (key, value) in [
            ("BIND_ADDR", "127.0.0.1"),
            ("PORT", "3129"),
            ("ANNOUNCE_IP", "127.0.0.1"),
            ("MEDIA_WORKERS", "1"),
            ("WEBRTC_SERVER_PORT_BASE", "41100"),
            ("ALLOW_AD_HOC_ROOMS", "true"),
            ("ALLOWED_ORIGINS", "http://127.0.0.1:3129"),
            ("REGISTRATION_ENABLED", "false"),
            ("MAX_CONNECTIONS_PER_IP", "128"),
            ("WS_HANDSHAKES_PER_MINUTE", "600"),
            ("RUST_LOG", "simplestchat=info,mediasoup=warn"),
        ] {
            result.insert(key.into(), value.into());
        }
        result
    }
    pub fn generator_arguments(&self) -> Vec<String> {
        [
            "--server",
            "ws://127.0.0.1:3129/ws",
            "--clients",
            &self.clients.to_string(),
            "--rooms",
            "4",
            "--room",
            &self.room,
            "--duration",
            &self.duration.to_string(),
            "--ramp-up",
            "65",
            "--warmup",
            "10",
            "--mode",
            "conference",
            "--quality",
            "480p",
            "--fps",
            "30",
            "--max-audio",
            "4",
            "--max-video",
            "4",
            "--subscription-plan",
            "ring-v1",
            "--subscription-seed",
            "17",
            "--output-dir",
            self.output.to_str().unwrap_or(""),
            "--run-label",
            "impaired",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }
}
