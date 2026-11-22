//! Pinned plugin releases and literal privilege/network boundaries.

use super::Plugin;
use crate::{
    command::{Command, Program},
    runner,
};
use simplestchat_toolkit::tooling::{Environment, Failure, Result};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

/// The largest asset curl may download and the installer may copy.
pub(super) const MAX_BYTES: u64 = 256 * 1024 * 1024;
/// The first system directory the Docker CLI searches for plugins. Only the
/// caller's and root's own `~/.docker/cli-plugins` and configured extra
/// directories precede it; the runner image keeps its copies in
/// `/usr/libexec/docker/cli-plugins`, which is searched later.
const PLUGINS: &str = "/usr/local/lib/docker/cli-plugins";

/// A lowercase SHA-256 hex digest. [`Checksum::new`] refuses any other text,
/// so a malformed pin fails when its constant is compiled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Checksum(&'static str);
impl Checksum {
    /// Accept exactly 64 lowercase hexadecimal digits.
    ///
    /// # Panics
    ///
    /// Panics on any other text; in a constant that is a compile error.
    pub(super) const fn new(text: &'static str) -> Self {
        let bytes = text.as_bytes();
        assert!(bytes.len() == 64, "A SHA-256 digest has 64 digits.");
        let mut index = 0;
        while index < bytes.len() {
            assert!(
                matches!(bytes[index], b'0'..=b'9' | b'a'..=b'f'),
                "A SHA-256 digest is lowercase hexadecimal."
            );
            index += 1;
        }
        Self(text)
    }

    pub(super) const fn as_str(self) -> &'static str {
        self.0
    }
}

/// The runner architectures every pinned release publishes an asset for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Machine {
    Amd64,
    Arm64,
}
impl Machine {
    /// Read `uname -m`. Every other machine is refused with status 2.
    pub(super) fn from_uname(machine: &str) -> Result<Self> {
        match machine {
            "x86_64" => Ok(Self::Amd64),
            "aarch64" | "arm64" => Ok(Self::Arm64),
            _ => Err(Failure::new(
                2,
                "Unsupported plugin controller architecture.",
            )),
        }
    }
}

/// One release asset and the digest its bytes must have before installation.
#[derive(Clone, Copy, Debug)]
pub(super) struct Asset {
    /// The release's own spelling of the architecture, recorded in evidence.
    pub architecture: &'static str,
    /// The asset's file name below the release's download path.
    file: &'static str,
    pub digest: Checksum,
}

/// One pinned Docker CLI plugin release and the exact line its version
/// command prints. Only the two constants below exist.
#[derive(Debug)]
pub(super) struct Release {
    /// The Docker project, the `docker-<name>` plugin file and the staging
    /// and report prefix.
    pub name: &'static str,
    /// The name in the fixed public failure line.
    pub title: &'static str,
    /// The release tag without its `v` prefix.
    pub version: &'static str,
    amd64: Asset,
    arm64: Asset,
    /// The `docker` arguments that print the installed plugin's version.
    version_arguments: &'static [&'static str],
    /// The complete line, before trailing newlines, that the caller's and
    /// root's version commands must both print.
    pub answer: &'static str,
}
impl Release {
    pub(super) const fn asset(&self, machine: Machine) -> Asset {
        match machine {
            Machine::Amd64 => self.amd64,
            Machine::Arm64 => self.arm64,
        }
    }

    /// The installed plugin's absolute path.
    pub(super) fn path(&self) -> String {
        format!("{PLUGINS}/docker-{}", self.name)
    }
}

const COMPOSE: Release = Release {
    name: "compose",
    title: "Compose",
    version: "5.6.0",
    amd64: Asset {
        architecture: "x86_64",
        file: "docker-compose-linux-x86_64",
        digest: Checksum::new("40343e21ca777173e69cff5dbafeb37c6f81f3b0d57d9e597f036e95eb63e76a"),
    },
    arm64: Asset {
        architecture: "aarch64",
        file: "docker-compose-linux-aarch64",
        digest: Checksum::new("733ec76717ceb59052a9609b9dadfb523b2df8eab57a54212872d10a58078ea2"),
    },
    version_arguments: &["compose", "version", "--short"],
    answer: "5.6.0",
};

// `docker buildx version` prints the package, the version and the release
// commit, which both published binaries embed as their only revision.
const BUILDX: Release = Release {
    name: "buildx",
    title: "Buildx",
    version: "0.39.1",
    amd64: Asset {
        architecture: "amd64",
        file: "buildx-v0.39.1.linux-amd64",
        digest: Checksum::new("903090816da68f8bc1a2ff5346dd163a5adacfce4548d003c75df42cd47a9bf4"),
    },
    arm64: Asset {
        architecture: "arm64",
        file: "buildx-v0.39.1.linux-arm64",
        digest: Checksum::new("05c88e0f9152825e609a1cdb391faf14d5f94d23d071eaecb87bae69d98da8b8"),
    },
    version_arguments: &["buildx", "version"],
    answer: "github.com/docker/buildx v0.39.1 334b63fe2bb0f0e89a8fd067704a0d113f57ec25",
};

impl Plugin {
    pub(super) const fn release(self) -> &'static Release {
        match self {
            Self::Compose => &COMPOSE,
            Self::Buildx => &BUILDX,
        }
    }
}

pub(super) struct Context {
    pub root: PathBuf,
    pub environment: Environment,
    pub temporary: PathBuf,
    pub release: &'static Release,
    pub asset: Asset,
}
impl Context {
    /// Admit only hosted GitHub or disposable local Linux CI, a supported
    /// machine and an existing absolute `RUNNER_TEMP`; each refusal is status 2.
    pub(super) fn new(
        root: &Path,
        environment: Environment,
        system: &str,
        machine: &str,
        plugin: Plugin,
    ) -> Result<Self> {
        if !runner::authorized(&environment, system) {
            return Err(Failure::new(
                2,
                "Plugin installation requires hosted GitHub or disposable local Linux CI.",
            ));
        }
        let release = plugin.release();
        let asset = release.asset(Machine::from_uname(machine)?);
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
            release,
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

pub(super) fn download(
    release: &Release,
    asset: Asset,
    sink: std::sync::Arc<std::fs::File>,
) -> Command {
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
                "https://github.com/docker/{}/releases/download/v{}/{}",
                release.name, release.version, asset.file
            ),
        ],
        65,
    );
    request.stdout = Some(sink);
    request
}

pub(super) fn install_directory() -> Command {
    command(Program::Sudo, &["install", "-d", "-m", "0755", PLUGINS], 15)
}

pub(super) fn install_file(release: &Release, path: &Path) -> Command {
    let mut request = command(Program::Sudo, &["install", "-m", "0755"], 15);
    request
        .arguments
        .extend([path.as_os_str().to_owned(), release.path().into()]);
    request
}

pub(super) fn caller_version(release: &Release) -> Command {
    command(Program::Docker, release.version_arguments, 10)
}

pub(super) fn root_version(release: &Release) -> Command {
    // sudo can isolate the root command's process group. Keep the existing
    // privileged inner timeout as well as the outer owned CLI's 15-second cap.
    let mut arguments = vec![
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
    ];
    arguments.extend(release.version_arguments);
    command(Program::Sudo, &arguments, 15)
}
