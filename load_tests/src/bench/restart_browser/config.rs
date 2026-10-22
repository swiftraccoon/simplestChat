//! Closed captured selection; the server uses the unchanged guest-only profile.

use super::files::Sources;
use crate::bench::smoke::{config as guest, files::Selected};
use anyhow::{Result, ensure};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

pub(super) const ACTIVE: f64 = 75_000.0;
pub(super) const ACCEPT: f64 = 120_000.0;
pub(super) const FINAL: f64 = 175_000.0;
pub(super) const LOG: u64 = 1024 * 1024;
pub(super) const RECEIPT: u64 = 32 * 1024;
#[derive(Debug)]
pub(super) struct InvalidArguments;
impl std::fmt::Display for InvalidArguments {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid_restart_arguments")
    }
}
impl std::error::Error for InvalidArguments {}
pub(super) struct Config {
    pub guest: guest::Config,
    pub browser: String,
    pub node_path: PathBuf,
    pub node: Option<Selected>,
    pub server: Option<Selected>,
    pub sources: Option<Sources>,
    pub run_id: String,
}
fn selected_node(
    root: &Path,
    env: &guest::Environment,
    value: Option<OsString>,
) -> Result<PathBuf> {
    if let Some(value) = value {
        let path = PathBuf::from(value);
        ensure!(path.is_absolute(), "invalid_restart_node");
        return Ok(path.canonicalize()?);
    }
    let path = env
        .get(&OsString::from("PATH"))
        .cloned()
        .unwrap_or_default();
    for directory in std::env::split_paths(&path) {
        let candidate = if directory.is_absolute() {
            directory
        } else {
            root.join(directory)
        }
        .join("node");
        if candidate.is_file() {
            use std::os::unix::fs::PermissionsExt;
            if candidate.metadata()?.permissions().mode() & 0o111 != 0 {
                return Ok(candidate.canonicalize()?);
            }
        }
    }
    anyhow::bail!("restart_node_unavailable")
}
impl Config {
    pub fn parse(args: &[OsString], root: PathBuf, env: guest::Environment) -> Result<Self> {
        let (guest, browser, node) =
            Self::arguments(args, root, env).map_err(|_| InvalidArguments)?;
        let node_path = selected_node(&guest.directory, &guest.environment, node)?;
        let mut value = Self {
            browser,
            node_path,
            guest,
            node: None,
            server: None,
            sources: None,
            run_id: uuid::Uuid::new_v4().to_string(),
        };
        let admitted: Result<()> = (|| {
            value.node = Some(Selected::open(&value.node_path)?);
            value.server = Some(Selected::open(&value.guest.binary)?);
            value.sources = Some(Sources::open(&value.guest.directory)?);
            Ok(())
        })();
        if let Err(error) = admitted {
            let _ = value.finish();
            return Err(error);
        }
        Ok(value)
    }
    fn arguments(
        args: &[OsString],
        root: PathBuf,
        env: guest::Environment,
    ) -> Result<(guest::Config, String, Option<OsString>)> {
        ensure!(
            root.to_str()
                .is_some_and(|value| value.len() <= 4000 && !value.chars().any(char::is_control)),
            "invalid_restart_workspace"
        );
        ensure!(args.len().is_multiple_of(2), "invalid_restart_arguments");
        let mut binary = None;
        let mut output = None;
        let mut browser = None;
        let mut node = None;
        for pair in args.as_chunks::<2>().0 {
            let target = match pair[0].to_str() {
                Some("--binary") => &mut binary,
                Some("--output") => &mut output,
                Some("--browser") => &mut browser,
                Some("--node") => &mut node,
                _ => anyhow::bail!("invalid_restart_arguments"),
            };
            ensure!(
                target.replace(pair[1].clone()).is_none(),
                "invalid_restart_arguments"
            );
        }
        let browser = browser
            .and_then(|v| v.into_string().ok())
            .filter(|v| matches!(v.as_str(), "chromium" | "firefox" | "webkit"))
            .ok_or_else(|| anyhow::anyhow!("invalid_restart_browser"))?;
        let binary = binary.ok_or_else(|| anyhow::anyhow!("missing_restart_binary"))?;
        let output = output.ok_or_else(|| anyhow::anyhow!("missing_restart_output"))?;
        let guest = guest::Config::parse(
            guest::Mode::Shutdown,
            &["--binary".into(), binary, "--output".into(), output],
            root,
            env,
        )?;
        ensure!(
            node.as_ref()
                .is_none_or(|path| Path::new(path).is_absolute()),
            "invalid_restart_node"
        );
        Ok((guest, browser, node))
    }
    pub fn check(&self) -> Result<()> {
        self.node.as_ref().unwrap().check()?;
        self.server.as_ref().unwrap().check()?;
        self.sources.as_ref().unwrap().check()
    }
    pub fn node_environment(&self, ports: guest::Ports) -> guest::Environment {
        let mut env = guest::Environment::new();
        for key in [
            "PATH",
            "HOME",
            "TMPDIR",
            "PLAYWRIGHT_BROWSERS_PATH",
            "XDG_CACHE_HOME",
            "XDG_CONFIG_HOME",
            "XDG_RUNTIME_DIR",
        ] {
            if let Some(value) = self.guest.environment.get(&OsString::from(key)) {
                env.insert(key.into(), value.clone());
            }
        }
        for (key, value) in [
            ("BROWSER_RESTART_RUN_ID", self.run_id.clone()),
            ("BROWSER_RESTART_ENGINE", self.browser.clone()),
            ("BROWSER_RESTART_PORT", ports.http.to_string()),
        ] {
            env.insert(key.into(), value.into());
        }
        env
    }
    pub fn finish(mut self) -> Result<()> {
        let source = self.sources.take().map(Sources::finish).transpose();
        let server = self.server.take().map(Selected::finish).transpose();
        let node = self.node.take().map(Selected::finish).transpose();
        source.and(server).and(node).map(|_| ())
    }
}
