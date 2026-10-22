//! The existing hosted comparison profile, captured before acquiring resources.

use crate::tooling::{Environment, Failure, Result};
use std::{
    ffi::OsStr,
    path::{Component, Path, PathBuf},
    time::Duration,
};

pub(super) const WATCHDOG: Duration = Duration::from_secs(90 * 60);
pub(super) const RESERVE: Duration = Duration::from_secs(5 * 60);
pub(super) const BENCH_GRACE: Duration = Duration::from_secs(90);
pub(super) const CLI_GRACE: Duration = Duration::from_secs(5);
pub(super) const SETTLEMENT: Duration = Duration::from_secs(20);
pub(super) const LOG_LIMIT: u64 = 64 * 1024 * 1024;

pub(super) fn failure(code: u8) -> Failure {
    Failure::new(
        code,
        "Controlled performance comparison failed; inspect its private evidence.",
    )
}

fn path(value: &Path) -> Result<()> {
    if !value.is_absolute()
        || value.as_os_str().as_encoded_bytes().len() > 4096
        || value.components().count() > 20
        || value.to_str().is_none()
        || value
            .components()
            .any(|v| !matches!(v, Component::RootDir | Component::Normal(_)))
    {
        return Err(failure(2));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Profile {
    pub clients: u16,
    pub workers: u8,
}
impl Profile {
    pub fn parse(environment: &Environment) -> Result<Self> {
        let selected = |name: &str, default: &'static str| -> Result<String> {
            environment
                .get(OsStr::new(name))
                .filter(|v| !v.is_empty())
                .map_or(Ok(default.into()), |v| {
                    v.to_str().map(str::to_owned).ok_or_else(|| failure(2))
                })
        };
        let clients = match selected("CLIENTS", "10")?.as_str() {
            "10" => 10,
            "30" => 30,
            "100" => 100,
            _ => return Err(failure(2)),
        };
        let workers = match selected("WORKERS", "1")?.as_str() {
            "1" => 1,
            "2" => 2,
            _ => return Err(failure(2)),
        };
        Ok(Self { clients, workers })
    }
    pub fn scenario(self) -> &'static str {
        if self.clients == 10 {
            "conference"
        } else {
            "multi-room"
        }
    }
    pub fn ramp(self) -> &'static str {
        match self.clients {
            10 => "5",
            30 => "65",
            _ => "202",
        }
    }
    pub fn minimum(self) -> Duration {
        let ramp = match self.clients {
            10 => 5,
            30 => 65,
            _ => 202,
        };
        Duration::from_secs(6 * (ramp + 10 + 60))
    }
}

pub(super) struct Plan {
    pub root: PathBuf,
    pub temporary: PathBuf,
    pub baseline: PathBuf,
    pub comparison: PathBuf,
    pub target: PathBuf,
    pub reference: String,
    pub profile: Profile,
    pub environment: Environment,
}
impl Plan {
    pub fn parse(root: &Path, environment: Environment, platform: &str) -> Result<Self> {
        if platform != "linux"
            || environment.get(OsStr::new("GITHUB_ACTIONS")) != Some(&"true".into())
            || environment.get(OsStr::new("RUNNER_OS")) != Some(&"Linux".into())
        {
            return Err(failure(2));
        }
        path(root)?;
        let temporary = environment
            .get(OsStr::new("RUNNER_TEMP"))
            .map(PathBuf::from)
            .ok_or_else(|| failure(2))?;
        path(&temporary)?;
        // A selected temporary parent may have ordinary system aliases. Resolve
        // it once before retaining its physical descriptor/name bindings.
        let temporary = temporary.canonicalize().map_err(|_| failure(2))?;
        path(&temporary)?;
        let reference = environment
            .get(OsStr::new("BASELINE_REF"))
            .filter(|v| !v.is_empty())
            .map_or(Ok("HEAD~1".to_owned()), |v| {
                v.to_str().map(str::to_owned).ok_or_else(|| failure(2))
            })?;
        if reference.len() > 4096 || reference.contains('\0') {
            return Err(failure(2));
        }
        let profile = Profile::parse(&environment)?;
        if root == temporary
            || temporary.starts_with(root.join("target"))
            || root.starts_with(temporary.join("baseline"))
            || root.starts_with(temporary.join("comparison"))
        {
            return Err(failure(2));
        }
        Ok(Self {
            root: root.into(),
            baseline: temporary.join("baseline"),
            comparison: temporary.join("comparison"),
            target: root.join("target"),
            temporary,
            reference,
            profile,
            environment,
        })
    }
    pub fn baseline_binary(&self) -> PathBuf {
        self.baseline.join("target/release/simplestChat")
    }
    pub fn candidate_binary(&self) -> PathBuf {
        self.target.join("release/simplestChat")
    }
    pub fn generator(&self) -> PathBuf {
        self.target.join("release/load_test")
    }
    pub fn comparison_arguments(&self) -> Vec<String> {
        let mut args = vec!["bench".into(), "local".into()];
        for (name, value) in [
            ("--baseline-root", self.baseline.clone()),
            ("--baseline-bin", self.baseline_binary()),
            ("--candidate-root", self.root.clone()),
            ("--candidate-bin", self.candidate_binary()),
            ("--generator", self.generator()),
            ("--output", self.comparison.clone()),
        ] {
            args.extend([name.into(), value.to_string_lossy().into_owned()]);
        }
        for (name, value) in [
            ("--clients", self.profile.clients.to_string()),
            ("--scenarios", self.profile.scenario().into()),
            ("--repetitions", "3".into()),
            ("--ramp-up", self.profile.ramp().into()),
            ("--warmup", "10".into()),
            ("--duration", "60".into()),
            ("--workers", self.profile.workers.to_string()),
        ] {
            args.extend([name.into(), value]);
        }
        if self.profile.clients != 10 {
            args.extend(
                [
                    "--subscription-plan",
                    "ring-v1",
                    "--subscription-seed",
                    "17",
                ]
                .into_iter()
                .map(str::to_owned),
            );
        }
        args
    }
}
