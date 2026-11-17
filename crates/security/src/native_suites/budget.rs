//! How many compilers a build may run and how much memory it may take,
//! bounded by what the host and its control groups actually allow.

use crate::{Fault, Result, require};
use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
};

/// One mebibyte.
const MIB: u64 = 1 << 20;
/// The most compilers a build runs.
pub const MAX_WORKERS: u64 = 4;
/// The most bytes of a control group's file that are read.
const MAX_METADATA: usize = 4096;
/// The least memory a build may have, in mebibytes.
const MIN_MEMORY_MIB: u64 = 2048;
/// The mounts of the control group controllers whose limits are read,
/// by the controller's name, the unified hierarchy's first.
const CONTROLLER_MOUNTS: [(&str, &[&str]); 3] = [
    ("", &[""]),
    ("cpu", &["cpu", "cpu,cpuacct", "cpuacct,cpu"]),
    ("memory", &["memory"]),
];

/// What a build may use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    /// The compilers it runs at once.
    pub workers: u64,
    /// The memory its compilation may take, in mebibytes.
    pub ram_mib: u64,
    /// The memory left for the queries after the reserve, in mebibytes.
    pub query_ram_mib: i64,
}

/// The budget of `cpus` processors and `memory_bytes` of memory, with the
/// compilers set by `requested` where one is asked for: from one to four,
/// and no more than the budget allows. The queries keep what the reviewed
/// CodeQL action reserves: one gibibyte, and a twentieth of what is above
/// eight.
pub fn budget(cpus: u64, memory_bytes: u64, requested: Option<&str>) -> Result<Budget> {
    let memory_mib = memory_bytes / MIB;
    require(
        cpus > 0 && memory_mib >= MIN_MEMORY_MIB,
        "codeql_insufficient_resources",
    )?;
    let mut workers = MAX_WORKERS
        .min(cpus)
        .min(1.max(memory_mib.saturating_sub(1024) / 2048));
    if let Some(requested) = requested {
        let one =
            requested.len() == 1 && ('1'..='4').contains(&requested.chars().next().unwrap_or('0'));
        require(one, "codeql_build_jobs_range")?;
        let asked = u64::from(requested.as_bytes()[0] - b'0');
        require(asked <= workers, "codeql_build_jobs_budget")?;
        workers = asked;
    }
    // The reference computes in floating point; the same arithmetic is
    // exact here in mebibytes and twentieths, truncated as it truncates.
    let total = memory_bytes as f64 / MIB as f64;
    let query = total - 1024.0 - (total - 8192.0).max(0.0) / 20.0;
    Ok(Budget {
        workers,
        ram_mib: 6144.min(memory_mib * 3 / 5),
        query_ram_mib: query as i64,
    })
}

/// The text of the small file `path`, stripped, or nothing when it is not
/// there.
fn optional_text(path: &Path) -> Result<Option<String>> {
    use std::io::Read as _;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(Fault("codeql_resource_metadata_unread")),
    };
    let mut read = Vec::new();
    let taken = file.take(MAX_METADATA as u64 + 1).read_to_end(&mut read);
    taken.map_err(|_| Fault("codeql_resource_metadata_unread"))?;
    require(read.len() <= MAX_METADATA, "codeql_resource_metadata_size")?;
    let text = String::from_utf8(read).map_err(|_| Fault("codeql_resource_metadata_size"))?;
    Ok(Some(text.trim().to_owned()))
}

/// The directories below the control group root `root` whose limits bound
/// this process: every controller's mount, and for each membership that
/// `membership` lists, the group's directory and those above it.
fn controller_directories(root: &Path, membership: &Path) -> Result<BTreeSet<PathBuf>> {
    let mut directories: BTreeSet<PathBuf> = CONTROLLER_MOUNTS
        .iter()
        .flat_map(|(_, mounts)| mounts.iter().map(|mount| root.join(mount)))
        .collect();
    for entry in optional_text(membership)?.unwrap_or_default().lines() {
        let fields: Vec<&str> = entry.splitn(3, ':').collect();
        require(fields.len() == 3, "codeql_cgroup_membership")?;
        let relative = Path::new(fields[2].trim_start_matches('/'));
        if relative
            .components()
            .any(|part| part == Component::ParentDir)
        {
            // The group is outside this namespace's view.
            continue;
        }
        let controllers: Vec<&str> = if fields[1].is_empty() {
            vec![""]
        } else {
            fields[1].split(',').collect()
        };
        for controller in controllers {
            let mounts = CONTROLLER_MOUNTS
                .iter()
                .find(|(name, _)| *name == controller);
            for mount in mounts.map_or(&[][..], |(_, mounts)| mounts) {
                let group = root.join(mount).join(relative);
                directories.extend(group.ancestors().map(Path::to_path_buf));
            }
        }
    }
    Ok(directories
        .into_iter()
        .filter(|directory| directory.starts_with(root))
        .collect())
}

/// A whole number the reference reads with `int`: digits with at most one
/// sign.
fn number(text: &str) -> Result<i128> {
    text.trim()
        .parse::<i128>()
        .map_err(|_| Fault("codeql_resource_number"))
}

/// The processors and the memory that the control groups below `root`
/// allow the process whose membership `membership` lists, of either
/// hierarchy, ancestors' ceilings included.
pub fn cgroup_limits(root: &Path, membership: &Path) -> Result<(Vec<u64>, Vec<u64>)> {
    let (mut cpus, mut memory) = (Vec::new(), Vec::new());
    for directory in controller_directories(root, membership)? {
        if let Some(quota) =
            optional_text(&directory.join("cpu.max"))?.filter(|text| !text.is_empty())
        {
            let parts: Vec<&str> = quota.split_whitespace().collect();
            let [amount, period] = parts[..] else {
                return Err(Fault("codeql_resource_number"));
            };
            if amount != "max" {
                let (amount, period) = (number(amount)?, number(period)?);
                require(amount > 0 && period > 0, "codeql_cpu_quota")?;
                cpus.push(1.max(amount / period) as u64);
            }
        }
        if let Some(quota) =
            optional_text(&directory.join("cpu.cfs_quota_us"))?.filter(|text| !text.is_empty())
        {
            let quota = number(&quota)?;
            if quota > 0 {
                let period = optional_text(&directory.join("cpu.cfs_period_us"))?;
                let period = match period {
                    Some(period) => number(&period)?,
                    None => 0,
                };
                require(period > 0, "codeql_cpu_period")?;
                cpus.push(1.max(quota / period) as u64);
            }
        }
        for name in ["memory.max", "memory.limit_in_bytes"] {
            let limit = optional_text(&directory.join(name))?;
            if let Some(limit) = limit.filter(|text| !text.is_empty() && text != "max") {
                let limit = number(&limit)?;
                require(limit > 0, "codeql_memory_limit")?;
                memory.push(u64::try_from(limit).map_err(|_| Fault("codeql_memory_limit"))?);
            }
        }
    }
    Ok((cpus, memory))
}

/// The budget of this host: the fewest processors and the least memory that
/// the machine, this process's affinity and its control groups allow, with
/// the compilers set by `requested` where one is asked for.
pub fn detect(requested: Option<&str>) -> Result<Budget> {
    let available = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    let mut cpus = vec![available as u64];
    let memory = if cfg!(target_os = "linux") {
        let (quota_cpus, quota_memory) =
            cgroup_limits(Path::new("/sys/fs/cgroup"), Path::new("/proc/self/cgroup"))?;
        cpus.extend(quota_cpus);
        let mut memory = vec![physical_memory()?];
        memory.extend(quota_memory);
        memory
    } else {
        require(cfg!(target_os = "macos"), "codeql_resource_platform")?;
        vec![physical_memory()?]
    };
    let cpus = cpus.into_iter().min().unwrap_or(1);
    let memory = memory.into_iter().min().unwrap_or(0);
    budget(cpus, memory, requested)
}

/// The memory of this machine, in bytes.
#[cfg(target_os = "linux")]
fn physical_memory() -> Result<u64> {
    let info = rustix::system::sysinfo();
    Ok(u64::from(info.totalram).saturating_mul(u64::from(info.mem_unit)))
}

/// The memory of this machine, in bytes, as `sysctl` says it.
#[cfg(not(target_os = "linux"))]
fn physical_memory() -> Result<u64> {
    let mut command = std::process::Command::new("/usr/sbin/sysctl");
    command.args(["-n", "hw.memsize"]);
    let asked = crate::context::asked(
        &mut command,
        &[],
        std::time::Duration::from_secs(5),
        (4096, 4096),
    );
    let (status, printed) = asked.map_err(|_| Fault("codeql_resource_platform"))?;
    require(status == 0, "codeql_resource_platform")?;
    let printed = String::from_utf8(printed).map_err(|_| Fault("codeql_resource_platform"))?;
    printed
        .trim()
        .parse::<u64>()
        .map_err(|_| Fault("codeql_resource_platform"))
}
