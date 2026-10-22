//! Pinned release selection and literal privilege/network boundaries.

use crate::ci::{
    command::{Command, Program},
    runner,
};
use crate::tooling::{Environment, Failure, Result};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

pub(super) const VERSION: &str = "5.6.0";
pub(super) const MAX_BYTES: u64 = 256 * 1024 * 1024;
pub(super) const PLUGIN: &str = "/usr/local/lib/docker/cli-plugins/docker-compose";

#[derive(Clone, Copy)]
pub(super) struct Asset {
    pub architecture: &'static str,
    pub digest: &'static str,
}
impl Asset {
    pub(super) fn for_machine(machine: &str) -> Result<Self> {
        match machine {
            "x86_64" => Ok(Self {
                architecture: "x86_64",
                digest: "40343e21ca777173e69cff5dbafeb37c6f81f3b0d57d9e597f036e95eb63e76a",
            }),
            "aarch64" | "arm64" => Ok(Self {
                architecture: "aarch64",
                digest: "733ec76717ceb59052a9609b9dadfb523b2df8eab57a54212872d10a58078ea2",
            }),
            _ => Err(Failure::new(
                2,
                "Unsupported Compose controller architecture.",
            )),
        }
    }
}

pub(super) struct Context {
    pub root: PathBuf,
    pub environment: Environment,
    pub temporary: PathBuf,
    pub asset: Asset,
}
impl Context {
    pub(super) fn new(
        root: &Path,
        environment: Environment,
        system: &str,
        machine: &str,
    ) -> Result<Self> {
        if !runner::authorized(&environment, system) {
            return Err(Failure::new(
                2,
                "Compose requires hosted GitHub or disposable local Linux CI.",
            ));
        }
        let asset = Asset::for_machine(machine)?;
        let temporary = PathBuf::from(runner::text(&environment, "RUNNER_TEMP"));
        if !temporary.is_absolute() || temporary.is_symlink() || !temporary.is_dir() {
            return Err(Failure::new(
                2,
                "RUNNER_TEMP must name an existing absolute directory.",
            ));
        }
        Ok(Self {
            root: root.into(),
            environment,
            temporary,
            asset,
        })
    }
}

fn command(program: Program, args: &[&str], seconds: u64) -> Command {
    let mut command = Command::capture(program, args);
    command.timeout = Duration::from_secs(seconds);
    command.grace = Duration::from_secs(2);
    command.capture = Some(4096);
    command
}

pub(super) fn download(asset: Asset, sink: std::sync::Arc<std::fs::File>) -> Command {
    let mut request = command(
        Program::Curl,
        &[
            "--disable",
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--proto",
            "=https",
            "--tlsv1.2",
            "--connect-timeout",
            "10",
            "--max-time",
            "60",
            "--max-filesize",
            &MAX_BYTES.to_string(),
            &format!(
                concat!(
                    "https://github.com/docker/compose/releases/download/v",
                    "{}/docker-compose-linux-{}"
                ),
                VERSION, asset.architecture
            ),
        ],
        65,
    );
    request.stdout = Some(sink);
    request
}

pub(super) fn install_directory() -> Command {
    command(
        Program::Sudo,
        &[
            "install",
            "-d",
            "-m",
            "0755",
            "/usr/local/lib/docker/cli-plugins",
        ],
        15,
    )
}

pub(super) fn install_file(path: &Path) -> Command {
    let mut request = command(Program::Sudo, &["install", "-m", "0755"], 15);
    request
        .arguments
        .extend([path.as_os_str().to_owned(), PLUGIN.into()]);
    request
}

pub(super) fn caller_version() -> Command {
    command(Program::Docker, &["compose", "version", "--short"], 10)
}

pub(super) fn root_version() -> Command {
    // sudo can isolate the root command's process group. Keep the existing
    // privileged inner timeout as well as the outer owned CLI's 15-second cap.
    command(
        Program::Sudo,
        &[
            "env",
            "-i",
            "PATH=/usr/sbin:/usr/bin:/sbin:/bin",
            "LC_ALL=C",
            "timeout",
            "--signal=TERM",
            "--kill-after=2s",
            "10s",
            "/usr/bin/docker",
            "--host",
            "unix:///var/run/docker.sock",
            "compose",
            "version",
            "--short",
        ],
        15,
    )
}
