"""Bound fresh CodeQL compilation and analysis by actual CPU and memory limits."""

from __future__ import annotations

import os
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

from security_tools import require

MIB = 1024**2
MAX_WORKERS = 4
MAX_METADATA = 4096
MIN_MEMORY_MIB = 2048
MEMBERSHIP_FIELDS = 3
CONTROLLER_MOUNTS = {"": ("",), "cpu": ("cpu", "cpu,cpuacct", "cpuacct,cpu"), "memory": ("memory",)}


@dataclass(frozen=True)
class Budget:
    """Conservative limits shared by hosted traced compilation and local analysis."""

    workers: int
    ram_mib: int


def optional_text(path: Path) -> str | None:
    """Read only small kernel resource metadata, allowing an absent controller."""
    try:
        with path.open() as source:
            text = source.read(MAX_METADATA + 1)
    except FileNotFoundError:
        return None
    require(len(text) <= MAX_METADATA, "codeql_resource_metadata_size")
    return text.strip()


def controller_directories(root: Path, membership: Path) -> set[Path]:
    """Locate visible roots and ancestors in common v1/v2 controller mounts."""
    directories = {root / mount for mounts in CONTROLLER_MOUNTS.values() for mount in mounts}
    for entry in (optional_text(membership) or "").splitlines():
        fields = entry.split(":", 2)
        require(len(fields) == MEMBERSHIP_FIELDS, "codeql_cgroup_membership")
        relative = Path(fields[2].lstrip("/"))
        if ".." in relative.parts:
            continue  # The container exposes a host-relative path outside its own namespace.
        controllers = fields[1].split(",") if fields[1] else [""]
        mounts = {mount for name in controllers for mount in CONTROLLER_MOUNTS.get(name, ())}
        for mount in mounts:
            base = root / mount
            directories.update((base / relative, *(base / relative).parents))
    return {
        directory for directory in directories if directory == root or root in directory.parents
    }


def cgroup_limits(root: Path, membership: Path) -> tuple[list[int], list[int]]:
    """Include v1/v2 controller limits and any visible ancestor ceilings."""
    cpus: list[int] = []
    memory: list[int] = []
    for directory in controller_directories(root, membership):
        quota = optional_text(directory / "cpu.max")
        if quota:
            amount, period = quota.split()
            if amount != "max":
                require(int(amount) > 0 and int(period) > 0, "codeql_cpu_quota")
                cpus.append(max(1, int(amount) // int(period)))
        quota = optional_text(directory / "cpu.cfs_quota_us")
        if quota and int(quota) > 0:
            period = optional_text(directory / "cpu.cfs_period_us")
            require(period is not None and int(period) > 0, "codeql_cpu_period")
            cpus.append(max(1, int(quota) // int(period or "0")))
        for name in ("memory.max", "memory.limit_in_bytes"):
            limit = optional_text(directory / name)
            if limit and limit != "max":
                require(int(limit) > 0, "codeql_memory_limit")
                memory.append(int(limit))
    return cpus, memory


def budget(cpus: int, memory_bytes: int, override: str | None = None) -> Budget:
    """Reserve one GiB for the runner, two per compiler, and 40% during queries."""
    memory_mib = memory_bytes // MIB
    require(cpus > 0 and memory_mib >= MIN_MEMORY_MIB, "codeql_insufficient_resources")
    workers = min(MAX_WORKERS, cpus, max(1, (memory_mib - 1024) // 2048))
    if override is not None:
        require(re.fullmatch(r"[1-4]", override), "codeql_build_jobs_range")
        require(int(override) <= workers, "codeql_build_jobs_budget")
        workers = int(override)
    return Budget(workers, min(6144, memory_mib * 3 // 5))


def detect(override: str | None = None) -> Budget:
    """Respect container quotas even when host CPU/memory discovery ignores them."""
    cpus = [os.cpu_count() or 1]
    if sys.platform == "linux":
        cpus.append(len(os.sched_getaffinity(0)))
        memory = [os.sysconf("SC_PHYS_PAGES") * os.sysconf("SC_PAGE_SIZE")]
        quota_cpus, quota_memory = cgroup_limits(Path("/sys/fs/cgroup"), Path("/proc/self/cgroup"))
        cpus.extend(quota_cpus)
        memory.extend(quota_memory)
    else:
        require(sys.platform == "darwin", "codeql_resource_platform")
        result = subprocess.run(
            ["/usr/sbin/sysctl", "-n", "hw.memsize"],
            check=True,
            capture_output=True,
            timeout=5,
        )
        memory = [int(result.stdout)]
    return budget(min(cpus), min(memory), override)


if __name__ == "__main__":
    selected = detect(os.environ.get("CODEQL_BUILD_JOBS"))
    _ = sys.stdout.write(f"{selected.workers}\n")
