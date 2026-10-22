use super::failure;
use crate::tooling::{Environment, Result};
use std::{
    ffi::OsStr,
    path::{Component, Path, PathBuf},
};

pub(super) struct Config {
    pub root: PathBuf,
    pub caller: PathBuf,
    pub output: PathBuf,
    pub temporary: PathBuf,
    pub address: String,
    pub phase: String,
    pub artifact_only: bool,
    pub environment: Environment,
}
fn text<'a>(env: &'a Environment, name: &str) -> Option<&'a str> {
    env.get(OsStr::new(name)).and_then(|value| value.to_str())
}
fn path(root: &Path, value: &str) -> Result<PathBuf> {
    let path = root.join(value);
    if value.is_empty()
        || path.as_os_str().as_encoded_bytes().len() > 4096
        || path
            .components()
            .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
        || path.to_str().is_none()
    {
        return Err(failure(2));
    }
    Ok(path)
}
fn physical_output(path: PathBuf) -> Result<PathBuf> {
    let mut existing = path;
    let mut suffix = Vec::new();
    while !existing.try_exists().map_err(|_| failure(2))? {
        suffix.push(existing.file_name().ok_or_else(|| failure(2))?.to_owned());
        if suffix.len() > 24 || !existing.pop() {
            return Err(failure(2));
        }
    }
    let mut output = existing.canonicalize().map_err(|_| failure(2))?;
    for name in suffix.into_iter().rev() {
        output.push(name);
    }
    Ok(output)
}
impl Config {
    pub fn read(
        root: &Path,
        caller: &Path,
        args: &[String],
        env: &Environment,
        platform: &str,
    ) -> Result<Self> {
        if platform != "macos"
            || text(env, "MDNS_DIAGNOSTIC_E2E") != Some("1")
            || !matches!(args, [phase] | [phase, _] if phase == "preflight" || phase == "snapshot")
            || (args.len() == 2 && args[1] != "--artifact-only")
        {
            return Err(failure(2));
        }
        let root = root.canonicalize().map_err(|_| failure(2))?;
        let caller = caller.canonicalize().map_err(|_| failure(2))?;
        let output = physical_output(path(
            &caller,
            text(env, "E2E_ARTIFACTS").ok_or_else(|| failure(2))?,
        )?)?;
        // Canonicalize only the explicitly selected existing temporary parent:
        // Darwin commonly exposes /var/folders through a symlink.
        let temporary = path(&caller, text(env, "TMPDIR").unwrap_or("/tmp"))?
            .canonicalize()
            .map_err(|_| failure(2))?;
        if temporary.starts_with(&output) {
            return Err(failure(2));
        }
        let address = text(env, "TEST_ANNOUNCE_IP").unwrap_or("").to_owned();
        // Bad addresses are diagnostic observations (exit 1), as in the JS.
        if address.len() > 64 {
            return Err(failure(2));
        }
        let mut environment = Environment::new();
        if let Some(path) = env.get(OsStr::new("PATH")) {
            if path.as_encoded_bytes().len() > 128 * 1024 {
                return Err(failure(2));
            }
            environment.insert("PATH".into(), path.clone());
        }
        environment.insert("LC_ALL".into(), "C".into());
        Ok(Self {
            root,
            caller,
            output,
            temporary,
            address,
            phase: args[0].clone(),
            artifact_only: args.len() == 2,
            environment,
        })
    }
    pub fn limit(&self) -> f64 {
        if self.artifact_only { 55.0 } else { 90.0 }
    }
}
