//! The mechanics profile never acquires an impairment or database resource.

use super::super::plan::{Environment, Selected, absolute, tool};
use crate::bench::artifacts;
use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};
use std::{ffi::OsString, path::Path, path::PathBuf, time::Duration};

pub(super) const PORT: u16 = 3109;
pub(super) const MEDIA_PORT: u16 = 41100;
pub(super) const BUDGET: Duration = Duration::from_secs(720);
pub(super) const NODE_BUDGET: Duration = Duration::from_secs(600);
pub(super) const RESERVE: Duration = Duration::from_secs(90);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Mechanics,
    Adaptive,
    Tcp,
}
impl Mode {
    pub fn adaptive(self) -> bool {
        self == Self::Adaptive
    }
    pub fn controlled(self) -> bool {
        self != Self::Mechanics
    }
    pub fn tcp(self) -> bool {
        self == Self::Tcp
    }
    pub fn budget(self) -> Duration {
        if self.tcp() {
            Duration::from_secs(1200)
        } else if self.adaptive() {
            Duration::from_secs(960)
        } else {
            BUDGET
        }
    }
    pub fn reserve(self) -> Duration {
        if self.tcp() {
            Duration::from_secs(450)
        } else if self.adaptive() {
            Duration::from_secs(300)
        } else {
            RESERVE
        }
    }
    pub fn stem(self) -> &'static str {
        if self.tcp() {
            "browser-tcp"
        } else if self.adaptive() {
            "browser-adaptive"
        } else {
            "browser-mechanics"
        }
    }
}

pub(super) struct KernelTools {
    pub tc: Selected,
    pub sudo: Selected,
    pub iptables: Option<Selected>,
}

pub(super) struct Plan {
    pub mode: Mode,
    pub kernel: Option<KernelTools>,
    pub root: PathBuf,
    pub output: PathBuf,
    pub environment: Environment,
    pub server: Selected,
    pub node: Selected,
    pub entry: PathBuf,
    pub entry_hash: String,
    pub run_id: String,
}

fn entry_hash(path: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(artifacts::read_regular(
        path,
        256 * 1024,
    )?)))
}

fn fixed(environment: &Environment, key: &str, allowed: &[&str]) -> Result<()> {
    if let Some(value) = environment.get(std::ffi::OsStr::new(key)) {
        let value = value
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid mechanics options"))?;
        ensure!(
            value.is_empty() || allowed.contains(&value),
            "Conflicting mechanics options"
        );
    }
    Ok(())
}

impl Plan {
    pub fn parse(root: &Path, arguments: &[OsString], environment: Environment) -> Result<Self> {
        Self::parse_mode(
            root,
            arguments,
            environment,
            Mode::Mechanics,
            std::env::consts::OS,
        )
    }

    pub fn parse_mode(
        root: &Path,
        arguments: &[OsString],
        environment: Environment,
        mode: Mode,
        system: &str,
    ) -> Result<Self> {
        if mode.controlled() {
            super::super::plan::authorize(&environment, system)?;
        }
        for key in [
            "CANARY_MODE",
            "CANARY_FORCE_RELAY",
            "CALL_OUTCOME_E2E",
            "TEST_BROWSER_HTTPS",
            "WEBKIT_TEST_NETWORK",
            "TURN_E2E",
            "PASSKEY_E2E",
        ] {
            fixed(&environment, key, &["0"])?;
        }
        fixed(
            &environment,
            "IMPAIRED_BLOCK_UDP",
            &[if mode.tcp() { "1" } else { "0" }],
        )?;
        if mode.tcp() {
            fixed(&environment, "WEBRTC_SERVER_TCP", &["true"])?;
        }
        for key in ["IMPAIRED_ROOM", "CANARY_CORRELATION_FILE", "NODE_OPTIONS"] {
            fixed(&environment, key, &[])?;
        }
        for (key, expected) in [
            ("BASE_URL", "http://127.0.0.1:3109"),
            ("E2E_BROWSER", "chromium"),
            (
                "IMPAIR_SCRIPT",
                if mode.controlled() {
                    "rust-owned"
                } else {
                    "none"
                },
            ),
            (
                "IMPAIRED_PROFILES",
                if mode.tcp() {
                    "baseline"
                } else if mode.adaptive() {
                    "baseline,lossy,constrained,severe,recovery,lossyJoin"
                } else {
                    "baseline,silentAudio"
                },
            ),
            (
                "IMPAIRED_SILENT_AUDIO",
                if mode.controlled() { "0" } else { "1" },
            ),
            ("IMPAIR_UDP_PORT", "41100"),
            ("IMPAIR_WORKERS", "1"),
            ("IMPAIR_DIRECTION", "downlink"),
            ("IMPAIR_LOSS", "0"),
            ("IMPAIR_DELAY_MS", "0"),
            ("IMPAIR_JITTER_MS", "0"),
            ("IMPAIR_RATE_KBIT", "0"),
        ] {
            fixed(&environment, key, &[expected])?;
        }
        let mut values = std::collections::BTreeMap::new();
        for pair in arguments.chunks(2) {
            ensure!(pair.len() == 2, "Invalid mechanics options");
            let key = pair[0].to_str().unwrap_or("");
            ensure!(
                matches!(key, "--output" | "--server-bin" | "--node"),
                "Invalid mechanics options"
            );
            ensure!(
                !pair[1].is_empty() && values.insert(key, &pair[1]).is_none(),
                "Invalid mechanics options"
            );
        }
        let output = absolute(
            root,
            values
                .get("--output")
                .ok_or_else(|| anyhow::anyhow!("Output required"))?,
        );
        ensure!(
            output.file_name().is_some()
                && !output.exists()
                && output.parent().is_some_and(Path::is_dir),
            "Fresh mechanics output required"
        );
        let output = output
            .parent()
            .unwrap()
            .canonicalize()?
            .join(output.file_name().unwrap());
        ensure!(
            output
                .to_str()
                .is_some_and(|value| value.bytes().all(|byte| byte >= 32 && byte != 127)),
            "Invalid mechanics output path"
        );
        let server = Selected::open(
            values
                .get("--server-bin")
                .map(|value| absolute(root, value))
                .or_else(|| {
                    environment
                        .get(std::ffi::OsStr::new("SIMPLESTCHAT_BIN"))
                        .filter(|value| !value.is_empty())
                        .map(|value| absolute(root, value))
                })
                .unwrap_or_else(|| root.join("target/release/simplestChat")),
        )?;
        let node = match values.get("--node") {
            Some(value) => Selected::open(absolute(root, value))?,
            None => tool(root, &environment, "node")?,
        };
        let entry = root.join(format!("web/e2e/{}.cjs", mode.stem()));
        let entry_hash = entry_hash(&entry)?;
        ensure!(
            [root, &output, &server.path, &node.path, &entry]
                .iter()
                .all(|path| path.to_str().is_some_and(|value| value.len() <= 4096)),
            "Invalid mechanics path"
        );
        let kernel = if mode.controlled() {
            Some(KernelTools {
                tc: tool(root, &environment, "tc")?,
                sudo: tool(root, &environment, "sudo")?,
                iptables: if mode.tcp() {
                    Some(tool(root, &environment, "iptables")?)
                } else {
                    None
                },
            })
        } else {
            None
        };
        Ok(Self {
            mode,
            kernel,
            root: root.to_owned(),
            output,
            environment,
            server,
            node,
            entry,
            entry_hash,
            run_id: uuid::Uuid::new_v4().to_string(),
        })
    }

    pub fn verify(&self) -> Result<()> {
        self.server.verify()?;
        self.node.verify()?;
        if let Some(kernel) = &self.kernel {
            kernel.tc.verify()?;
            kernel.sudo.verify()?;
            if let Some(iptables) = &kernel.iptables {
                iptables.verify()?;
            }
        }
        ensure!(
            entry_hash(&self.entry)? == self.entry_hash,
            "Mechanics entry changed"
        );
        Ok(())
    }

    pub fn base(&self) -> Environment {
        ["PATH", "HOME", "TMPDIR"]
            .into_iter()
            .filter_map(|key| self.environment.get_key_value(std::ffi::OsStr::new(key)))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }

    pub fn server_environment(&self) -> Environment {
        let mut environment = self.base();
        environment
            .entry("TMPDIR".into())
            .or_insert_with(|| "/tmp".into());
        for key in [
            "LIBWEBRTC_FIELD_TRIALS",
            "WEBRTC_MIN_OUTGOING_BITRATE",
            "WEBRTC_SERVER_TCP",
        ] {
            if let Some(value) = self
                .environment
                .get(std::ffi::OsStr::new(key))
                .filter(|value| !value.is_empty())
            {
                environment.insert(key.into(), value.clone());
            }
        }
        for (key, value) in [
            ("BIND_ADDR", "127.0.0.1"),
            ("PORT", "3109"),
            ("ANNOUNCE_IP", "127.0.0.1"),
            ("MEDIA_WORKERS", "1"),
            ("WEBRTC_SERVER_PORT_BASE", "41100"),
            ("ALLOW_AD_HOC_ROOMS", "true"),
            ("ALLOWED_ORIGINS", "http://127.0.0.1:3109"),
            ("REGISTRATION_ENABLED", "false"),
            ("RUST_LOG", "simplestchat=info,mediasoup=warn"),
        ] {
            environment.insert(key.into(), value.into());
        }
        if self.mode.tcp() {
            environment.insert("WEBRTC_SERVER_TCP".into(), "true".into());
        }
        environment
    }

    pub fn node_environment(&self) -> Environment {
        let mut environment = self.base();
        for key in [
            "PLAYWRIGHT_MODULE",
            "PLAYWRIGHT_BROWSERS_PATH",
            "XDG_CACHE_HOME",
            "XDG_CONFIG_HOME",
            "XDG_RUNTIME_DIR",
        ] {
            if let Some(value) = self.environment.get(std::ffi::OsStr::new(key)) {
                environment.insert(key.into(), value.clone());
            }
        }
        for (key, value) in [
            ("BASE_URL", "http://127.0.0.1:3109"),
            ("IMPAIR_UDP_PORT", "41100"),
            ("IMPAIR_WORKERS", "1"),
            (
                "IMPAIR_SCRIPT",
                if self.mode.controlled() {
                    "rust-owned"
                } else {
                    "none"
                },
            ),
            ("E2E_BROWSER", "chromium"),
            (
                "IMPAIRED_PROFILES",
                if self.mode.tcp() {
                    "baseline"
                } else if self.mode.adaptive() {
                    "baseline,lossy,constrained,severe,recovery,lossyJoin"
                } else {
                    "baseline,silentAudio"
                },
            ),
            (
                "IMPAIRED_SILENT_AUDIO",
                if self.mode.controlled() { "0" } else { "1" },
            ),
            ("TEST_BROWSER_HTTPS", "0"),
        ] {
            environment.insert(key.into(), value.into());
        }
        if self.mode.tcp() {
            environment.insert("WEBRTC_SERVER_TCP".into(), "true".into());
            environment.insert("IMPAIRED_BLOCK_UDP".into(), "1".into());
        }
        environment.insert("E2E_ARTIFACTS".into(), self.output.clone().into_os_string());
        environment.insert(
            if self.mode.tcp() {
                "BROWSER_TCP_RUN_ID"
            } else if self.mode.adaptive() {
                "BROWSER_ADAPTIVE_RUN_ID"
            } else {
                "BROWSER_MECHANICS_RUN_ID"
            }
            .into(),
            self.run_id.clone().into(),
        );
        environment
    }
}
