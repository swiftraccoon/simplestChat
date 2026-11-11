//! What a command on the prepared host may touch: fixed commands, the clocks
//! and the fixed directories of one deployment. Tests substitute each.

pub(crate) mod command;
pub(crate) mod evidence;
pub(crate) mod failure;
pub(crate) mod fault;
#[cfg(test)]
pub(crate) mod replay;
pub(crate) mod state;

use chrono::{DateTime, Utc};
use command::{Commands, Request};
use failure::Result;
use std::path::PathBuf;
use std::time::Duration;

/// Whether `text` is exactly `length` lowercase hexadecimal digits.
pub(crate) fn hexadecimal(text: &str, length: usize) -> bool {
    text.len() == length
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Whether `text` is a content-addressed image identity.
pub(crate) fn image_id(text: &str) -> bool {
    text.strip_prefix("sha256:")
        .is_some_and(|hash| hexadecimal(hash, 64))
}

/// Whether `text` is a kernel boot identity: 8-4-4-4-12 lowercase hexadecimal.
pub(crate) fn boot_identity(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.iter().map(|group| group.len()).eq([8, 4, 4, 4, 12])
        && groups.iter().all(|group| hexadecimal(group, group.len()))
}

/// A lower-case DNS name of at least two labels ending in a letter-only one.
pub(crate) fn domain(name: &str) -> bool {
    let labels: Vec<&str> = name.split('.').collect();
    let alphanumeric = |byte: &u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    let label = |label: &&str| {
        let bytes = label.as_bytes();
        (1..=63).contains(&bytes.len())
            && bytes.iter().all(|byte| alphanumeric(byte) || *byte == b'-')
            && bytes.first().is_some_and(alphanumeric)
            && bytes.last().is_some_and(alphanumeric)
    };
    let Some((last, others)) = labels.split_last() else {
        return false;
    };
    !others.is_empty()
        && others.iter().all(label)
        && (2..=63).contains(&last.len())
        && last.bytes().all(|byte| byte.is_ascii_lowercase())
}

/// Write one line; a line that cannot be written is a failure of its own.
pub(crate) fn emit(mut stream: impl std::io::Write, line: &str) -> bool {
    stream
        .write_all(line.as_bytes())
        .and_then(|()| stream.flush())
        .is_ok()
}

/// Wall and monotonic time.
pub(crate) trait Clock {
    /// The current instant in UTC.
    fn now(&self) -> DateTime<Utc>;
    /// A monotonic reading for deadlines.
    fn monotonic(&self) -> Duration;
    /// Wait between polls.
    fn sleep(&self, duration: Duration);
}

/// The machine's own clocks.
pub(crate) struct System;

impl System {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl Clock for System {
    fn now(&self) -> DateTime<Utc> {
        std::time::SystemTime::now().into()
    }

    /// The kernel's monotonic clock itself, not a reading relative to this
    /// process: a journal's deadline is compared by later processes.
    fn monotonic(&self) -> Duration {
        let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
        let seconds = u64::try_from(now.tv_sec).unwrap_or_default();
        let nanoseconds = u32::try_from(now.tv_nsec).unwrap_or_default();
        Duration::new(seconds, nanoseconds)
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// The fixed locations of one deployment.
pub(crate) struct Paths {
    /// Private monitoring state.
    pub state: PathBuf,
    /// Reviewed SQL beside the installed executable.
    pub sql: PathBuf,
    /// The public deployment's data root.
    pub root: PathBuf,
    /// Monitoring settings written by provisioning.
    pub settings: PathBuf,
    /// The selected release.
    pub images: PathBuf,
    /// Kernel interface statistics.
    pub network: PathBuf,
    /// The shared runtime directory of the workload lock and its journal.
    pub work: PathBuf,
    /// The kernel's identity of the current boot.
    pub boot: PathBuf,
    /// The offsite backup's private settings and credentials.
    pub offsite: PathBuf,
    /// The public project's protected configuration.
    pub config: PathBuf,
    /// The relay project's protected configuration.
    pub relay: PathBuf,
    /// Where a walk that follows no link starts: the machine's root.
    pub anchor: PathBuf,
}

impl Paths {
    /// The paths of a prepared host.
    pub(crate) fn fixed() -> Self {
        Self {
            state: "/var/lib/simplestchat-monitoring".into(),
            sql: "/usr/local/libexec/simplestchat-public".into(),
            root: "/srv/simplestchat-public".into(),
            settings: "/etc/simplestchat-monitoring/settings.json".into(),
            images: "/etc/simplestchat-public/images.json".into(),
            network: "/sys/class/net".into(),
            work: "/run/simplestchat-bench".into(),
            boot: "/proc/sys/kernel/random/boot_id".into(),
            offsite: "/etc/simplestchat-backup".into(),
            config: "/etc/simplestchat-public".into(),
            relay: "/etc/simplestchat-turn".into(),
            anchor: "/".into(),
        }
    }
}

/// One deployment as a command sees it.
pub(crate) struct Host<'a> {
    pub commands: &'a dyn Commands,
    pub clock: &'a dyn Clock,
    pub paths: Paths,
    /// The owner private files must have: root on a host.
    pub owner: u32,
}

/// The local daemon, ignoring inherited Docker selectors.
pub(crate) const DOCKER: [&str; 3] = ["/usr/bin/docker", "--host", "unix:///var/run/docker.sock"];

/// The database container of the public project.
pub(crate) const POSTGRES: &str = "simplestchat-public-postgres-1";

impl Host<'_> {
    /// The kernel's identity of the current boot.
    pub(crate) fn boot(&self) -> Result<String> {
        let bytes = state::read(&self.paths.boot, 4096)?;
        let value = String::from_utf8(bytes).map_err(|_| failure::Failure::Value)?;
        let value = value.trim();
        failure::require(boot_identity(value), "Invalid host boot identity")?;
        Ok(value.to_owned())
    }

    /// GET `url` directly, without proxies, redirects or stored configuration,
    /// and return a successful body of at most `limit` bytes.
    pub(crate) fn get(&self, url: &str, limit: usize, oversized: &'static str) -> Result<Vec<u8>> {
        let argv = [
            "/usr/bin/curl",
            "--disable",
            "--silent",
            "--show-error",
            "--fail",
            "--noproxy",
            "*",
            "--max-redirs",
            "0",
            "--proto",
            "=http,https",
            "--max-time",
            "3",
            "--output",
            "-",
            url,
        ];
        let argv = argv.map(str::to_owned).to_vec();
        self.commands
            .run(&Request::fixed(argv, 5.0, limit, oversized))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::Cell;

    /// A clock that stands still unless a test moves it; sleeping advances it.
    pub(crate) struct Fixed {
        pub now: Cell<DateTime<Utc>>,
        pub monotonic: Cell<Duration>,
        /// How far each monotonic reading advances the next one.
        pub tick: Cell<Duration>,
    }

    impl Fixed {
        pub(crate) fn at(text: &str) -> Self {
            let now = DateTime::parse_from_rfc3339(text)
                .unwrap()
                .with_timezone(&Utc);
            Self {
                now: Cell::new(now),
                monotonic: Cell::new(Duration::ZERO),
                tick: Cell::new(Duration::ZERO),
            }
        }
    }

    impl Clock for Fixed {
        fn now(&self) -> DateTime<Utc> {
            self.now.get()
        }

        fn monotonic(&self) -> Duration {
            let reading = self.monotonic.get();
            self.monotonic.set(reading + self.tick.get());
            reading
        }

        fn sleep(&self, duration: Duration) {
            self.monotonic.set(self.monotonic.get() + duration);
        }
    }

    /// Paths of one deployment under a scratch directory.
    pub(crate) fn paths(root: &std::path::Path) -> Paths {
        Paths {
            state: root.join("state"),
            sql: root.join("sql"),
            root: root.join("public"),
            settings: root.join("settings.json"),
            images: root.join("images.json"),
            network: root.join("net"),
            work: root.join("work"),
            boot: root.join("boot_id"),
            offsite: root.join("offsite"),
            config: root.join("config"),
            relay: root.join("turn"),
            anchor: root.to_path_buf(),
        }
    }
}
