//! What a command on the prepared host may touch: fixed commands, the clocks
//! and the fixed directories of one deployment. Tests substitute each.

pub(crate) mod command;
pub(crate) mod evidence;
pub(crate) mod failure;
pub(crate) mod state;

use chrono::{DateTime, Utc};
use command::{Commands, Request};
use failure::Result;
use std::path::PathBuf;
use std::time::Duration;

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
        }
    }
}
