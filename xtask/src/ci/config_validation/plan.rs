//! Two closed offline validation plans; identities occupy literal argv slots.

use crate::{
    ci::{command::Command, container_engine::Policy, runner},
    tooling::{Environment, Failure, Result},
};
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    time::Instant,
};

pub(super) const PROMETHEUS: &str = concat!(
    "quay.io/prometheus/prometheus:v3.15.0@sha256:",
    "efd719c99d83b060d9daefdcf00360461adf279f45ef5391f8d111892118753e",
);
pub(super) const CADDY: &str = concat!(
    "ghcr.io/swiftraccoon/simplestchat-caddy:2.11.7-",
    "73c81fdbfe625d3df3989ba807064c81fbe2c071@sha256:",
    "23c127cb8945a1de0c6753010614b50af1643bbcf93ab8a9a7d7461efd50b7c0",
);
pub(super) const PROXY_SECRET: &str = "ci-only-proxy-secret-00000000000000000000000000000000";
pub(super) const OWNER: &str = "com.simplestchat.config-validation";
pub(super) fn failure() -> Failure {
    Failure::new(1, "Configuration validation evidence rejected.")
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Promtool,
    Caddy,
}
impl Kind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "promtool" => Some(Self::Promtool),
            "caddy" => Some(Self::Caddy),
            _ => None,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Promtool => "promtool",
            Self::Caddy => "caddy",
        }
    }
    pub fn image(self) -> &'static str {
        match self {
            Self::Promtool => PROMETHEUS,
            Self::Caddy => CADDY,
        }
    }
    pub fn repository(self) -> &'static str {
        match self {
            Self::Promtool => "quay.io/prometheus/prometheus",
            Self::Caddy => "ghcr.io/swiftraccoon/simplestchat-caddy",
        }
    }
    pub fn inputs(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Caddy => &[("", "Caddyfile")],
            Self::Promtool => &[
                ("ops/ansible/tests", "monitoring-rules.test.yml"),
                ("ops/ansible/files", "monitoring-alerts.yml"),
            ],
        }
    }
}

pub(super) struct Context {
    pub root: PathBuf,
    pub temporary: PathBuf,
    pub environment: Environment,
    pub kind: Kind,
    pub started: Instant,
}
impl Context {
    pub fn new(
        root: &Path,
        environment: Environment,
        kind: Kind,
        system: &str,
        started: Instant,
    ) -> Result<Self> {
        if !runner::authorized(&environment, system) {
            return Err(Failure::new(
                2,
                "Configuration validation requires hosted or disposable Linux CI.",
            ));
        }
        let temporary = environment
            .get(OsStr::new("RUNNER_TEMP"))
            .map(PathBuf::from)
            .ok_or_else(failure)?;
        if root.to_str().is_none()
            || !super::engine::safe_path(root)
            || !super::engine::safe_path(&temporary)
            || root.as_os_str().as_encoded_bytes().contains(&match kind {
                Kind::Promtool => b':',
                Kind::Caddy => b',',
            })
        {
            return Err(Failure::new(2, "Configuration validation path invalid."));
        }
        if kind == Kind::Caddy
            && (environment
                .get(OsStr::new("CADDY_IMAGE"))
                .and_then(|v| v.to_str())
                != Some(CADDY)
                || environment
                    .get(OsStr::new("TRUSTED_PROXY_SECRET"))
                    .and_then(|v| v.to_str())
                    != Some(PROXY_SECRET))
        {
            return Err(Failure::new(
                2,
                "Configuration validation fixed inputs invalid.",
            ));
        }
        Ok(Self {
            root: root.into(),
            temporary,
            environment,
            kind,
            started,
        })
    }
    pub fn create(&self, endpoint: &str, name: &str, token: &str, image: &str) -> Command {
        let mut args = vec![
            "container".to_owned(),
            "create".into(),
            "--name".into(),
            name.into(),
            "--label".into(),
            format!("{OWNER}={}:{}", token, self.kind.label()),
        ];
        let fixed: Vec<String> = match self.kind {
            Kind::Promtool => vec![
                "--network=none".into(),
                "--read-only".into(),
                "--cap-drop=ALL".into(),
                "--security-opt=no-new-privileges:true".into(),
                "--memory=128m".into(),
                "--cpus=0.25".into(),
                "--pids-limit=64".into(),
                "--tmpfs".into(),
                "/tmp:rw,nosuid,nodev,noexec,size=64m".into(),
                "--volume".into(),
                format!("{}/ops/ansible:/work:ro", self.root.display()),
                "--workdir".into(),
                "/work/tests".into(),
                "--entrypoint".into(),
                "/bin/promtool".into(),
                image.into(),
                "test".into(),
                "rules".into(),
                "monitoring-rules.test.yml".into(),
            ],
            Kind::Caddy => vec![
                "--network".into(),
                "none".into(),
                "--read-only".into(),
                "--cap-drop".into(),
                "ALL".into(),
                "--cap-add".into(),
                "NET_BIND_SERVICE".into(),
                "--security-opt".into(),
                "no-new-privileges".into(),
                "--tmpfs".into(),
                "/config:rw,nosuid,nodev,noexec,size=16m".into(),
                "--tmpfs".into(),
                "/data:rw,nosuid,nodev,noexec,size=16m".into(),
                "--env".into(),
                "TRUSTED_PROXY_SECRET".into(),
                "--mount".into(),
                format!(
                    "type=bind,src={}/Caddyfile,dst=/etc/caddy/Caddyfile,readonly",
                    self.root.display()
                ),
                "--entrypoint".into(),
                "/usr/bin/caddy".into(),
                image.into(),
                "validate".into(),
                "--config".into(),
                "/etc/caddy/Caddyfile".into(),
                "--adapter".into(),
                "caddyfile".into(),
            ],
        };
        args.extend(fixed);
        Policy::validation().command(
            Some(endpoint),
            &args.iter().map(String::as_str).collect::<Vec<_>>(),
            false,
        )
    }
}
