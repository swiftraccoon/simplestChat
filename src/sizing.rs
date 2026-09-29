#![forbid(unsafe_code)]
//! What this process was given and what that carries, said once at startup so
//! an operator can see a mis-sized deployment in the first log lines.
//!
//! The costs are the reference figures `build/capacity.py` uses (an EPYC-class
//! VPS, 2026-09-26): a meeting participant 8 millicores of worker CPU, a
//! webinar viewer 3, and a room of everyone publishing stops near 28 whatever
//! the host. `build/capacity.py run` measures a host's own.

use std::path::Path;

/// Meeting participants one worker core carries (85 per worker at the 0.7 guard).
const MEETING_PARTICIPANTS_PER_WORKER: usize = 85;
/// Webinar viewers one worker carries with Chrome-timed publishers.
const WEBINAR_VIEWERS_PER_WORKER: usize = 175;
/// Where the all-publishing grid stops: the per-viewer bitrate cap, not CPU.
const GRID_PARTICIPANTS: usize = 28;

/// The cgroup v2 CPU quota in whole CPUs, or `None` without a limit.
pub fn parse_cpu_max(contents: &str) -> Option<f64> {
    let mut fields = contents.split_whitespace();
    let quota = fields.next()?;
    let period: f64 = fields.next()?.parse().ok()?;
    if quota == "max" || period <= 0.0 {
        return None;
    }
    let quota: f64 = quota.parse().ok()?;
    Some(quota / period)
}

/// A cgroup v2 memory limit in bytes, or `None` without one.
pub fn parse_memory_max(contents: &str) -> Option<u64> {
    let value = contents.trim();
    if value == "max" {
        return None;
    }
    value.parse().ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sizing {
    pub workers: usize,
    /// Whole CPUs of quota, when the cgroup sets one.
    pub quota_cpus: Option<u64>,
    pub memory_mib: Option<u64>,
    pub max_connections: Option<usize>,
    pub max_rooms: Option<usize>,
}

impl Sizing {
    /// About how many meeting participants these workers carry.
    pub fn meeting_participants(&self) -> usize {
        self.workers * MEETING_PARTICIPANTS_PER_WORKER
    }

    /// About how many webinar viewers, spread over every worker.
    pub fn webinar_viewers(&self) -> usize {
        self.workers * WEBINAR_VIEWERS_PER_WORKER
    }

    /// One line for the log, plus the warnings a mis-sized deployment earns.
    pub fn report(&self) -> (String, Vec<String>) {
        let quota = self.quota_cpus.map_or_else(
            || "no CPU quota".to_owned(),
            |cpus| format!("a {cpus}-CPU quota"),
        );
        let memory = self
            .memory_mib
            .map_or_else(|| "no memory limit".to_owned(), |mib| format!("{mib} MiB"));
        let caps = format!(
            "MAX_CONNECTIONS={} MAX_ROOMS={}",
            self.max_connections
                .map_or_else(|| "default".to_owned(), |value| value.to_string()),
            self.max_rooms
                .map_or_else(|| "default".to_owned(), |value| value.to_string())
        );
        let line = format!(
            "Sizing: {} media worker{} under {quota} and {memory}; reference costs project about {} meeting participants, {} webinar viewers and all-publishing rooms of {} ({caps})",
            self.workers,
            if self.workers == 1 { "" } else { "s" },
            self.meeting_participants(),
            self.webinar_viewers(),
            GRID_PARTICIPANTS,
        );
        let mut warnings = Vec::new();
        if let Some(cpus) = self.quota_cpus {
            let cpus = usize::try_from(cpus).unwrap_or(usize::MAX);
            if self.workers > cpus {
                warnings.push(format!(
                    "{} media workers share a {cpus}-CPU quota: workers contend; set MEDIA_WORKERS to the quota",
                    self.workers
                ));
            } else if cpus > self.workers + 1 {
                warnings.push(format!(
                    "a {cpus}-CPU quota runs only {} media workers: {} CPUs idle; raise MEDIA_WORKERS",
                    self.workers,
                    cpus - self.workers
                ));
            }
        }
        if let Some(limit) = self.max_connections {
            if limit > self.webinar_viewers() {
                warnings.push(format!(
                    "MAX_CONNECTIONS={limit} exceeds the {} viewers these workers are projected to carry; the per-worker guard will refuse joins first",
                    self.webinar_viewers()
                ));
            } else if limit < self.meeting_participants() / 2 {
                warnings.push(format!(
                    "MAX_CONNECTIONS={limit} is under half the {} meeting participants these workers carry",
                    self.meeting_participants()
                ));
            }
        }
        if let Some(mib) = self.memory_mib
            && mib < 512
        {
            warnings.push(format!(
                "{mib} MiB is under the 512 MiB floor the sizing model assumes"
            ));
        }
        (line, warnings)
    }
}

fn read_env_usize(name: &str) -> Option<usize> {
    std::env::var(name).ok()?.trim().parse().ok()
}

/// The process's sizing from its cgroup files and environment.
pub fn from_process(workers: usize, cgroup: &Path) -> Sizing {
    let read = |name: &str| std::fs::read_to_string(cgroup.join(name)).ok();
    Sizing {
        workers,
        quota_cpus: read("cpu.max")
            .and_then(|contents| parse_cpu_max(&contents))
            .map(|cpus| cpus.round() as u64),
        memory_mib: read("memory.max")
            .and_then(|contents| parse_memory_max(&contents))
            .map(|bytes| bytes / (1024 * 1024)),
        max_connections: read_env_usize("MAX_CONNECTIONS"),
        max_rooms: read_env_usize("MAX_ROOMS"),
    }
}

/// Log the sizing line and its warnings.
pub fn announce(workers: usize) {
    let sizing = from_process(workers, Path::new("/sys/fs/cgroup"));
    let (line, warnings) = sizing.report();
    tracing::info!("{line}");
    for warning in warnings {
        tracing::warn!("Sizing: {warning}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cgroup_files_parse_with_and_without_limits() {
        assert_eq!(parse_cpu_max("max 100000\n"), None);
        assert_eq!(parse_cpu_max("300000 100000\n"), Some(3.0));
        assert_eq!(parse_cpu_max("50000 100000"), Some(0.5));
        assert_eq!(parse_cpu_max("garbage"), None);
        assert_eq!(parse_memory_max("max\n"), None);
        assert_eq!(parse_memory_max("2147483648\n"), Some(2_147_483_648));
    }

    #[test]
    fn the_report_names_idle_cpus_contention_and_caps_out_of_proportion() {
        let sized = Sizing {
            workers: 3,
            quota_cpus: Some(3),
            memory_mib: Some(8192),
            max_connections: Some(525),
            max_rooms: Some(525),
        };
        let (line, warnings) = sized.report();
        assert!(
            line.contains("3 media workers under a 3-CPU quota and 8192 MiB"),
            "{line}"
        );
        assert!(
            line.contains("255 meeting participants, 525 webinar viewers"),
            "{line}"
        );
        assert!(warnings.is_empty(), "{warnings:?}");

        let idle = Sizing {
            workers: 2,
            quota_cpus: Some(4),
            ..sized.clone()
        };
        assert!(idle.report().1[0].contains("2 CPUs idle"));
        let contended = Sizing {
            workers: 4,
            quota_cpus: Some(2),
            ..sized.clone()
        };
        assert!(contended.report().1[0].contains("share a 2-CPU quota"));
        let over = Sizing {
            max_connections: Some(2000),
            ..sized.clone()
        };
        assert!(over.report().1[0].contains("exceeds the 525 viewers"));
        let under = Sizing {
            max_connections: Some(50),
            ..sized.clone()
        };
        assert!(under.report().1[0].contains("under half the 255"));
        let unlimited = Sizing {
            quota_cpus: None,
            memory_mib: None,
            max_connections: None,
            max_rooms: None,
            workers: 1,
        };
        let (line, warnings) = unlimited.report();
        assert!(line.contains("1 media worker under no CPU quota and no memory limit"));
        assert!(line.contains("MAX_CONNECTIONS=default"));
        assert!(warnings.is_empty());
    }

    #[test]
    fn a_process_reads_its_cgroup_files_when_they_exist() {
        let directory =
            std::env::temp_dir().join(format!("simplestchat-sizing-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("cpu.max"), "200000 100000\n").unwrap();
        std::fs::write(directory.join("memory.max"), "3221225472\n").unwrap();
        let sizing = from_process(2, &directory);
        let missing = from_process(2, &directory.join("absent"));
        std::fs::remove_dir_all(&directory).unwrap();
        assert_eq!(
            (sizing.quota_cpus, sizing.memory_mib),
            (Some(2), Some(3072))
        );
        assert_eq!((missing.quota_cpus, missing.memory_mib), (None, None));
    }
}
