#!/usr/bin/env python3
r"""Measure how many SimplestChat participants a server carries, and what they cost.

`run` calibrates the host it runs on. It starts the server image as an owned,
loopback-only container under the CPU quota being evaluated and the load
generator in the same network namespace, emulating browsers (three simulcast
layers, Opus DTX, every remote producer, tile-sized layer requests). It grows
three workloads until the server's own admission signals or the kernel say
stop, then prints the settings to use and writes calibration.json:

  meetings       rooms of five all-publishing participants: the everyday case,
                 and the capacity a cost comparison uses
  large-meeting  one all-publishing room: sizes MAX_PARTICIPANTS_PER_ROOM
  webinar        one presenter and a growing audience

`compare` ranks several hosts' reports by cost per 1,000 participant-hours.

The generator shares the host and costs about 1.5 times the server's CPU per
participant, so the server gets a quarter of it (two workers for the one-room
workload); each figure is measured per core and projected to the cores the app
will have.

Nothing is published: media stays on the containers' loopback, and the only
host port is the server's HTTP endpoint on 127.0.0.1. Needs Python 3.10 or
later and Docker or Podman on Linux (Podman's VM on macOS works for trials).

  python3 build/capacity.py run --server-image IMAGE --generator-image IMAGE \
      --label cx32 --monthly-price 6.80
  python3 build/capacity.py compare results/capacity.*/calibration.json
"""

from __future__ import annotations

import argparse
import contextlib
import json
import math
import re
import secrets
import shutil
import signal
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request
from dataclasses import asdict, dataclass, field, replace
from datetime import datetime, timezone
from pathlib import Path
from typing import TYPE_CHECKING, Final, Literal, NoReturn, cast

if TYPE_CHECKING:
    from collections.abc import Callable, Iterable, Mapping, Sequence
    from http.client import HTTPResponse
    from types import FrameType

    from _typeshed import DataclassInstance

# `datetime.UTC` is Python 3.11; hosts running Ubuntu 22.04 have 3.10.
UTC: Final = timezone.utc  # noqa: UP017
SCHEMA_VERSION: Final = 1
SERVER_PORT: Final = 3000
WORKER_PORT_BASE: Final = 40_000
SOURCE_ADDRESSES: Final = 250
HOURS_PER_MONTH: Final = 730
MIB: Final = 2**20
# IPv4 20 + UDP 8 + RTP 12 + header extensions 12 + SRTP tag 10 bytes: the
# generator counts RTP payload bytes, egress pays for every packet's headers.
PACKET_OVERHEAD_BYTES: Final = 62
# The server's default consumer cap (64): in an all-publishing room beyond 33
# people each browser shows only 32 peers.
FULL_MEDIA_ROOM_LIMIT: Final = 33
# The server's per-viewer send cap (`max_outgoing_bitrate`); a viewer whose
# estimate sits at it has no room for more video, whatever the host.
VIEWER_BITRATE_CAP: Final = 3_000_000
AT_CAP_SHARE: Final = 0.95
# The server's socket buffer default (1 MiB) needs these kernel ceilings.
SOCKET_BUFFER_CEILING: Final = 2 * MIB
# A host this busy before any load undercounts its capacity.
BUSY_HOST_FRACTION: Final = 0.1
# Room limits keep this share of the measured ceiling as headroom.
ROOM_LIMIT_SHARE: Final = 0.9
# Projections aim this far below the worker guard; searches stop when the
# ceiling is bracketed this tightly.
PROJECTION_MARGIN: Final = 0.95
BRACKET_SHARE: Final = 0.1
# Spread workloads are judged by their busiest worker; project with this much
# imbalance over the mean.
SPREAD_IMBALANCE: Final = 1.15
MEMORY_POLL_SECONDS: Final = 10
LIVENESS_POLL_SECONDS: Final = 5
SCRAPE_SECONDS: Final = 10
HTTP_OK: Final = 200

Workload = Literal["meetings", "large-meeting", "webinar"]
WORKLOADS: Final[tuple[Workload, ...]] = ("meetings", "large-meeting", "webinar")
# Only "measured" has a failure above it; the others say why a search stopped short.
Bound = Literal[
    "measured", "generator-limited", "step-limited", "interrupted", "below-first-size", "not-run"
]


class CapacityError(RuntimeError):
    """A calibration step could not run; the message says why."""


# --- Typed JSON access --------------------------------------------------------


def as_object(value: object, what: str) -> dict[str, object]:
    """Return `value` as a JSON object or fail naming what was expected."""
    if not isinstance(value, dict):
        message = f"{what} must be a JSON object"
        raise CapacityError(message)
    return cast("dict[str, object]", value)


def as_number(value: object, what: str) -> float:
    """Return `value` as a JSON number (booleans are not numbers)."""
    if isinstance(value, bool) or not isinstance(value, int | float):
        message = f"{what} must be a number"
        raise CapacityError(message)
    return float(value)


def as_list(value: object, what: str) -> list[object]:
    """Return `value` as a JSON array."""
    if not isinstance(value, list):
        message = f"{what} must be a JSON array"
        raise CapacityError(message)
    return cast("list[object]", value)


def read_json(path: Path) -> dict[str, object]:
    """Read a JSON object from `path`."""
    return as_object(cast("object", json.loads(path.read_text(encoding="utf-8"))), str(path))


def camel(name: str) -> str:
    """`snake_case` as `camelCase`."""
    head, *rest = name.split("_")
    return head + "".join(part.title() for part in rest)


def record(value: DataclassInstance) -> dict[str, object]:
    """Return a dataclass as a JSON object with camelCase keys."""
    fields = cast("dict[str, object]", asdict(value))
    return {camel(key): item for key, item in fields.items()}


# --- Kernel and server counters ---------------------------------------------


@dataclass(frozen=True, slots=True)
class CpuStat:
    """A cgroup v2 `cpu.stat` reading."""

    usage_usec: int
    periods: int
    throttled: int


def parse_cpu_stat(text: str) -> CpuStat:
    """Parse cgroup v2 `cpu.stat` (periods stay 0 without a quota)."""
    values: dict[str, int] = {}
    for line in text.splitlines():
        key, _, value = line.partition(" ")
        if value.strip().isdigit():
            values[key] = int(value)
    if "usage_usec" not in values:
        message = "cpu.stat lacks usage_usec"
        raise CapacityError(message)
    return CpuStat(values["usage_usec"], values.get("nr_periods", 0), values.get("nr_throttled", 0))


def throttled_share(before: CpuStat, after: CpuStat) -> float:
    """Share of enforcement periods throttled between two readings."""
    periods = after.periods - before.periods
    return (after.throttled - before.throttled) / periods if periods > 0 else 0.0


@dataclass(frozen=True, slots=True)
class HostTimes:
    """The host's aggregate CPU time from `/proc/stat`, in clock ticks."""

    total: int
    idle: int
    steal: int


# Field positions of the aggregate cpu line: user, nice, system, idle, iowait,
# irq, softirq, steal; guest time is already inside user time.
IDLE_FIELD: Final = 3
IOWAIT_FIELD: Final = 4
STEAL_FIELD: Final = 7
COUNTED_FIELDS: Final = 8


def parse_proc_stat(text: str) -> HostTimes:
    """Parse the aggregate `cpu` line of `/proc/stat`."""
    for line in text.splitlines():
        parts = line.split()
        if parts and parts[0] == "cpu":
            ticks = [int(part) for part in parts[1:]] + [0] * COUNTED_FIELDS
            idle = ticks[IDLE_FIELD] + ticks[IOWAIT_FIELD]
            return HostTimes(sum(ticks[:COUNTED_FIELDS]), idle, ticks[STEAL_FIELD])
    message = "/proc/stat lacks the aggregate cpu line"
    raise CapacityError(message)


def count_cpus(proc_stat: str) -> int:
    """Count the host's logical CPUs from `/proc/stat`."""
    return sum(1 for line in proc_stat.splitlines() if re.match(r"cpu\d+ ", line))


# ARM reports codes, not names: the implementers and the server cores clouds rent.
ARM_IMPLEMENTERS: Final = {"0x41": "ARM", "0x48": "HiSilicon", "0x61": "Apple", "0xc0": "Ampere"}
ARM_CORES: Final = {
    ("0x41", "0xd08"): "ARM Cortex-A72",  # Raspberry Pi 4
    ("0x41", "0xd0b"): "ARM Cortex-A76",  # Raspberry Pi 5
    ("0x41", "0xd0c"): "ARM Neoverse-N1",  # AWS Graviton2, Ampere Altra, Oracle A1
    ("0x41", "0xd40"): "ARM Neoverse-V1",  # AWS Graviton3
    ("0x41", "0xd49"): "ARM Neoverse-N2",  # Azure Cobalt 100
    ("0x41", "0xd4f"): "ARM Neoverse-V2",  # AWS Graviton4, Google Axion, NVIDIA Grace
    ("0x41", "0xd84"): "ARM Neoverse-V3",
    ("0x41", "0xd8e"): "ARM Neoverse-N3",
    ("0x48", "0xd01"): "HiSilicon Kunpeng 920",
    ("0xc0", "0xac3"): "AmpereOne",
}


def parse_cpu_model(cpuinfo: str) -> str:
    """Return the CPU model: x86's model name, or an ARM core named from its codes."""
    fields: dict[str, str] = {}
    for line in cpuinfo.splitlines():
        name, _, value = line.partition(":")
        _ = fields.setdefault(name.strip(), value.strip())
    if fields.get("model name"):
        return fields["model name"]
    implementer = fields.get("CPU implementer", "").lower()
    part = fields.get("CPU part", "").lower()
    if implementer and part:
        vendor = ARM_IMPLEMENTERS.get(implementer)
        if (implementer, part) in ARM_CORES:
            return ARM_CORES[implementer, part]
        return (
            f"{vendor} CPU part {part}" if vendor else f"CPU implementer {implementer} part {part}"
        )
    return fields.get("Hardware") or "unknown"


def parse_platform(dmi: str) -> str:
    """Return the machine's DMI vendor and product: a cloud's instance type on some."""
    return " ".join(line.strip() for line in dmi.splitlines() if line.strip())


def parse_meminfo_kib(meminfo: str) -> int:
    """MemTotal in KiB from `/proc/meminfo`."""
    for line in meminfo.splitlines():
        if line.startswith("MemTotal:"):
            return int(line.split()[1])
    return 0


@dataclass(frozen=True, slots=True)
class UdpCounters:
    """The server namespace's UDP counters and its workers' socket drops."""

    in_datagrams: int
    rcvbuf_errors: int
    socket_drops: int


# `/proc/net/udp` rows: `sl: local rem st tx:rx tr tm retr uid timeout inode
# ref pointer drops`.
UDP_ROW_COLUMNS: Final = 13


def parse_udp(snmp: str, table: str, ports: Iterable[int]) -> UdpCounters:
    """Parse `/proc/net/snmp` totals and the drops of sockets bound to `ports`."""
    udp = [line.split()[1:] for line in snmp.splitlines() if line.startswith("Udp:")]
    if len(udp) != len(("header", "values")):
        message = "/proc/net/snmp lacks Udp counters"
        raise CapacityError(message)
    totals = dict(zip(udp[0], (int(value) for value in udp[1]), strict=False))
    wanted = set(ports)
    drops = 0
    for line in table.splitlines():
        columns = line.split()
        if len(columns) < UDP_ROW_COLUMNS or not re.fullmatch(r"\d+:", columns[0]):
            continue
        if int(columns[1].rsplit(":", 1)[1], 16) in wanted:
            drops += int(columns[-1])
    return UdpCounters(totals.get("InDatagrams", 0), totals.get("RcvbufErrors", 0), drops)


def parse_prometheus(text: str) -> dict[str, float]:
    """Parse Prometheus text exposition into `name{labels}` -> value."""
    values: dict[str, float] = {}
    for line in text.splitlines():
        if not line or line.startswith("#"):
            continue
        name, _, value = line.rpartition(" ")
        try:
            values[name] = float(value)
        except ValueError:
            continue
    return values


def worker_loads(metrics: Mapping[str, float]) -> dict[int, float]:
    """Each media worker's share of one core over the server's last 10 s."""
    loads: dict[int, float] = {}
    for name, value in metrics.items():
        match = re.fullmatch(r'simplestchat_media_worker_cpu\{worker="(\d+)"\}', name)
        if match:
            loads[int(match.group(1))] = value
    return loads


# --- Workloads -----------------------------------------------------------------


@dataclass(frozen=True, slots=True)
class Browser:
    """The browser each synthetic participant emulates (generator options)."""

    capture: str = "720p"
    speakers: int = 1
    viewport_width: int = 1440
    pixel_ratio: float = 2.0
    layout: str = "classic"


# Joins cost handshakes on the worker that takes them, so a ceiling holds for a
# join rate: at 1.5 joins a second per worker a step of 157 participants per
# worker collapsed at the ramp's end (the guard refused 39, the kernel dropped a
# fifth of the media) where 150 per worker had passed at 0.375. Meetings arrive
# at 0.5 a second per worker; one room at 1.5, an audience's lighter joins at 4.
MEETING_JOINS_PER_WORKER_SECOND: Final = 0.5
ROOM_JOINS_PER_SECOND: Final = 1.5
WEBINAR_JOINS_PER_SECOND: Final = 4.0
MINIMUM_RAMP_SECONDS: Final = 10


def join_rate(workload: Workload, workers: int) -> float:
    """Return the joins a second for a workload measured on `workers` media workers."""
    if workload == "meetings":
        return MEETING_JOINS_PER_WORKER_SECOND * workers
    return ROOM_JOINS_PER_SECOND if workload == "large-meeting" else WEBINAR_JOINS_PER_SECOND


@dataclass(frozen=True, slots=True)
class StepPlan:
    """One measured run: a workload at a size."""

    workload: Workload
    size: int
    meeting_size: int
    warmup_seconds: int
    duration_seconds: int
    workers: int = 1

    @property
    def clients(self) -> int:
        """Synthetic clients: participants, or the audience plus its presenter."""
        return self.size + 1 if self.workload == "webinar" else self.size

    @property
    def rooms(self) -> int:
        """Rooms the clients are spread over."""
        return self.size // self.meeting_size if self.workload == "meetings" else 1

    @property
    def joins_per_second(self) -> float:
        """How fast the clients arrive."""
        return join_rate(self.workload, self.workers)

    @property
    def ramp_seconds(self) -> int:
        """How long the clients take to join."""
        return max(MINIMUM_RAMP_SECONDS, math.ceil(self.clients / self.joins_per_second))

    def generator_args(self, browser: Browser, label: str, revisions: tuple[str, str]) -> list[str]:
        """Return the load generator's command line for this step."""
        # One presenter: ceil(clients * ratio) must be exactly one.
        ratio = 0.5 / self.clients if self.workload == "webinar" else 1.0
        options = {
            "--server": f"ws://127.0.0.1:{SERVER_PORT}/ws",
            "--clients": str(self.clients),
            "--rooms": str(self.rooms),
            "--room": f"capacity-{self.workload}",
            "--publish-ratio": f"{ratio:.12f}",
            "--source-addresses": str(SOURCE_ADDRESSES),
            "--ramp-up": str(self.ramp_seconds),
            "--warmup": str(self.warmup_seconds),
            "--duration": str(self.duration_seconds),
            "--deadline-grace": "60",
            "--profile": "browser",
            "--capture": browser.capture,
            "--speakers": str(browser.speakers),
            "--viewport-width": str(browser.viewport_width),
            "--pixel-ratio": f"{browser.pixel_ratio:g}",
            "--layout": browser.layout,
            "--output-dir": "/results",
            "--run-label": label,
            "--server-revision": revisions[0],
            "--generator-revision": revisions[1],
        }
        return [item for pair in options.items() for item in pair]


def forwarded_streams(workload: Workload, size: int, meeting_size: int) -> float:
    """Consumers the server forwards to at a size: the load model for projections."""
    if workload == "meetings":
        return size * 2.0 * (meeting_size - 1)
    if workload == "large-meeting":
        # Browsers consume every peer's audio and video, up to 64 consumers.
        return size * float(min(2 * (size - 1), 64))
    return size * 2.0


# --- Judging a step -----------------------------------------------------------


@dataclass(frozen=True, slots=True)
class Limits:
    """What a passing step must stay within."""

    worker_load: float = 0.7
    drop_share: float = 0.001
    generator_throttled: float = 0.05
    # Quota throttling stalls every worker at once, whatever each one's load.
    server_throttled: float = 0.1
    memory_share: float = 0.85
    steal_share: float = 0.05


@dataclass(slots=True)
class StepResult:
    """What one measured run observed."""

    workload: Workload
    size: int
    clients: int
    rooms: int
    started_at: str
    generator_passed: bool = False
    generator_errors: int = 0
    # The generator's first failure reasons, and how many it reported.
    generator_failures: list[str] = field(default_factory=list[str])
    generator_failure_count: int = 0
    failed_connections: int = 0
    failed_consumers: int = 0
    # From the clients' results: video among the failures, and viewers whose
    # estimate sat at the per-viewer cap.
    failed_video_consumers: int = 0
    viewers_at_cap: int = 0
    validated_consumers: int = 0
    refused_joins: int = 0
    worker_load_peak: dict[int, float] = field(default_factory=dict[int, float])
    worker_load_mean: dict[int, float] = field(default_factory=dict[int, float])
    server_cores: float = 0.0
    server_throttled: float = 0.0
    # Scrapes in the window that read every worker's load, and those in which
    # the server reported itself CPU-saturated.
    load_scrapes: int = 0
    server_saturated_scrapes: int = 0
    generator_cores: float = 0.0
    generator_throttled: float = 0.0
    # The generator's throttling while its clients joined, before the window.
    generator_setup_throttled: float = 0.0
    host_steal: float = 0.0
    host_busy: float = 0.0
    # Every datagram the clients sent the workers over the run (RTP, RTCP, STUN,
    # DTLS): what the workers' drops are a share of.
    sent_datagrams: int = 0
    socket_drops: int = 0
    # The shared namespace's totals count the clients' own receive traffic as well.
    namespace_datagrams: int = 0
    rcvbuf_errors: int = 0
    memory_peak_bytes: int = 0
    memory_limit_bytes: int = 0
    generator_memory_peak_bytes: int = 0
    generator_out_of_memory: bool = False
    received_packets: int = 0
    received_payload_bytes: int = 0
    measurement_seconds: float = 0.0
    receive_ready_p99_ms: float = 0.0
    server_exit: str = ""
    error: str = ""

    @property
    def busiest_worker(self) -> float:
        """The busiest worker's highest 10-second load in the window."""
        return max(self.worker_load_peak.values(), default=0.0)

    @property
    def mean_worker(self) -> float:
        """The workers' mean load over the window."""
        loads = self.worker_load_mean.values()
        return sum(loads) / len(loads) if loads else 0.0

    @property
    def drop_share(self) -> float:
        """Datagrams the kernel discarded at the workers' sockets, as a share of what was sent."""
        if not self.sent_datagrams:
            return 1.0 if self.socket_drops else 0.0
        return self.socket_drops / self.sent_datagrams

    @property
    def egress_mbps(self) -> float:
        """Media the server sent in the generator's window, on the wire, in Mbit/s."""
        if self.measurement_seconds <= 0:
            return 0.0
        wire = self.received_payload_bytes + self.received_packets * PACKET_OVERHEAD_BYTES
        return wire * 8 / self.measurement_seconds / 1e6


@dataclass(frozen=True, slots=True)
class Verdict:
    """Whether a step passed, whether it measured the server at all, and why."""

    passed: bool
    valid: bool
    reasons: tuple[str, ...]


# The web client's lowest layer, and the video tiles a browser shows at most
# (half of the server's 64 consumers).
LOWEST_LAYER_BPS: Final = 100_000
MOST_VIDEO_TILES: Final = 32


def video_tiles(step: StepResult) -> int:
    """Remote video tiles each participant of a step's workload shows."""
    if step.workload == "meetings":
        return max(0, step.clients // max(1, step.rooms) - 1)
    if step.workload == "large-meeting":
        return min(max(0, step.size - 1), MOST_VIDEO_TILES)
    return 1


def lost_media(step: StepResult) -> str:
    """Say which consumers lost media, and whether the per-viewer cap explains it."""
    # Estimates sit at the cap on any healthy network: blame it only when even
    # the lowest layer of every tile needs more than it.
    starved = video_tiles(step) * LOWEST_LAYER_BPS >= AT_CAP_SHARE * VIEWER_BITRATE_CAP
    if starved and step.viewers_at_cap and step.failed_video_consumers == step.failed_consumers:
        return (
            f"{step.failed_consumers} video consumers lost media while {step.viewers_at_cap} "
            + f"viewers' estimates sat at the server's {VIEWER_BITRATE_CAP // 1_000_000} Mbit/s "
            + "per-viewer cap: their tiles need more than a viewer may receive, whatever the CPU"
        )
    return f"{step.failed_consumers} consumers lost media"


def judge(step: StepResult, limits: Limits) -> Verdict:
    """Apply the pass criteria; a throttled generator makes a step invalid, not failed."""
    if step.error:
        return Verdict(passed=False, valid=False, reasons=(step.error,))
    throttled = step.generator_throttled > limits.generator_throttled
    setup_throttled = step.generator_setup_throttled > limits.generator_throttled
    valid = not throttled and not setup_throttled and not step.generator_out_of_memory
    checks = (
        (
            throttled,
            f"the generator was throttled in {step.generator_throttled:.0%} of periods, "
            + "so it, not the server, set the pace",
        ),
        (
            setup_throttled,
            f"the generator was throttled in {step.generator_setup_throttled:.0%} of periods "
            + "while clients joined, so it, not the server, set the pace",
        ),
        (
            step.generator_out_of_memory,
            "the generator ran out of memory, so it, not the server, set the pace",
        ),
        (bool(step.server_exit), f"server exited: {step.server_exit}"),
        (
            bool(step.failed_connections or step.refused_joins),
            f"{step.failed_connections} joins failed "
            + f"({step.refused_joins} refused by the worker guard)",
        ),
        (bool(step.failed_consumers), lost_media(step)),
        (
            step.drop_share > limits.drop_share,
            f"the kernel dropped {step.drop_share:.2%} of inbound datagrams",
        ),
        (
            step.busiest_worker > limits.worker_load,
            f"a worker reached {step.busiest_worker:.2f} cores (guard {limits.worker_load:.2f})",
        ),
        (
            step.server_throttled > limits.server_throttled,
            f"the server's CPU quota ran out in {step.server_throttled:.0%} of periods",
        ),
        (
            step.server_saturated_scrapes > 0,
            "the server reported itself CPU-saturated, so /ready failed and it refused joins",
        ),
        (
            bool(step.memory_limit_bytes)
            and step.memory_peak_bytes > limits.memory_share * step.memory_limit_bytes,
            "memory rose above the memory guard's share of the limit",
        ),
    )
    reasons = [reason for failed, reason in checks if failed]
    if not step.generator_passed and not reasons:
        if step.generator_failures:
            more = step.generator_failure_count - 1
            suffix = f" (and {more} more)" if more > 0 else ""
            reasons.append(f"the generator's checks failed: {step.generator_failures[0]}{suffix}")
        else:
            reasons.append(f"the generator's own checks failed ({step.generator_errors} errors)")
    return Verdict(passed=valid and not reasons, valid=valid, reasons=tuple(reasons))


# --- Searching for a ceiling --------------------------------------------------


@dataclass(frozen=True, slots=True)
class Trial:
    """A judged step as the search sees it."""

    size: int
    passed: bool
    valid: bool
    busiest_worker: float
    mean_worker: float
    generator_cores: float


@dataclass(frozen=True, slots=True)
class Search:
    """How one workload's ceiling is searched."""

    workload: Workload
    meeting_size: int
    first: int
    granularity: int
    max_steps: int
    minimum: int

    @property
    def spreads(self) -> bool:
        """Whether the load spreads over workers (many rooms, or viewers placed apart)."""
        return self.workload != "large-meeting"


# The share of its quota the generator may plan to use, and the least a passing
# step grows the next one by.
GENERATOR_BUDGET: Final = 0.85
MINIMUM_GROWTH: Final = 1.15


def round_down(size: float, granularity: int) -> int:
    """Round a size down to the workload's step, never below one step."""
    return max(granularity, int(size // granularity) * granularity)


def projected_size(search: Search, trial: Trial, limits: Limits, generator_quota: float) -> int:
    """Return the size where the trial's load reaches the guard, within the generator's quota."""

    def streams(size: int) -> float:
        return forwarded_streams(search.workload, size, search.meeting_size)

    load = trial.mean_worker * SPREAD_IMBALANCE if search.spreads else trial.busiest_worker
    target = streams(trial.size) * limits.worker_load * PROJECTION_MARGIN / max(load, 0.01)
    size = trial.size
    while streams(size + 1) <= target and size < trial.size * 20:
        size += 1
    if trial.generator_cores > 0:
        per_stream = trial.generator_cores / streams(trial.size)
        while size > trial.size and per_stream * streams(size) > generator_quota * GENERATOR_BUDGET:
            size -= 1
    return size


def next_size(
    search: Search,
    trials: Sequence[Trial],
    limits: Limits,
    generator_quota: float,
) -> int | None:
    """Return the next size to try, or None once the ceiling is bracketed or out of reach."""
    if not trials:
        return search.first
    if len(trials) >= search.max_steps or not trials[-1].valid:
        return None
    lo = max((trial.size for trial in trials if trial.passed), default=None)
    hi = min((trial.size for trial in trials if not trial.passed), default=None)
    if lo is not None and hi is not None:
        if hi - lo <= max(search.granularity, int(lo * BRACKET_SHARE)):
            return None
        candidate = round_down((lo + hi) / 2, search.granularity)
    elif hi is not None:
        # Only failures: shrink toward the minimum.
        candidate = round_down(hi * 0.6, search.granularity)
        if candidate < search.minimum:
            return None
    else:
        best = max((trial for trial in trials if trial.passed), key=lambda trial: trial.size)
        grown = max(
            projected_size(search, best, limits, generator_quota),
            math.ceil(best.size * MINIMUM_GROWTH),
        )
        candidate = round_down(grown, search.granularity)
        if best.generator_cores > 0:
            capped = projected_size(search, best, Limits(worker_load=1e9), generator_quota)
            candidate = min(candidate, round_down(capped, search.granularity))
    known = {trial.size for trial in trials}
    return None if candidate in known or (lo is not None and candidate <= lo) else candidate


# --- Ceilings, recommendations and cost --------------------------------------


@dataclass(frozen=True, slots=True)
class Ceiling:
    """What a workload's search found."""

    workload: Workload
    ceiling: int | None
    bound: Bound
    busiest_worker: float
    egress_mbps: float
    memory_peak_bytes: int
    # The media workers it was measured with.
    workers: int = 1
    # What the failure just above a measured ceiling ran into.
    limit: str = ""


def stop_reason(last: tuple[StepResult, Verdict]) -> Bound:
    """Why a search whose last step did not run properly stopped."""
    step, verdict = last
    return "interrupted" if not verdict.valid and step.error else "generator-limited"


def ceiling_of(
    workload: Workload,
    trials: Sequence[tuple[StepResult, Verdict]],
    generator_quota: float,
    *,
    workers: int,
) -> Ceiling:
    """Summarize a workload's trials: measured only when a valid failure lies above."""
    if not trials:
        return Ceiling(workload, None, "not-run", 0.0, 0.0, 0, workers)
    passing = [step for step, verdict in trials if verdict.passed]
    if not passing:
        failed = any(verdict.valid for _, verdict in trials)
        bound: Bound = "below-first-size" if failed else "interrupted"
        return Ceiling(workload, None, bound, 0.0, 0.0, 0, workers)
    best = max(passing, key=lambda step: step.size)
    above = [
        (step, verdict)
        for step, verdict in trials
        if verdict.valid and not verdict.passed and step.size > best.size
    ]
    limit = "; ".join(min(above, key=lambda pair: pair[0].size)[1].reasons) if above else ""
    if above:
        bound = "measured"
    elif not trials[-1][1].valid:
        bound = stop_reason(trials[-1])
    elif best.generator_cores * MINIMUM_GROWTH > GENERATOR_BUDGET * generator_quota:
        bound = "generator-limited"
    else:
        bound = "step-limited"
    return Ceiling(
        workload,
        best.size,
        bound,
        best.busiest_worker,
        best.egress_mbps,
        best.memory_peak_bytes,
        workers,
        limit,
    )


@dataclass(frozen=True, slots=True)
class Prices:
    """What the host costs, for the cost model."""

    monthly: float
    egress_per_gb: float = 0.0
    included_egress_tb: float = 0.0
    port_mbps: float = 0.0


# Plan the network port to this share of its speed.
PORT_HEADROOM: Final = 0.8
# The server's default per-worker guard (CPU_SATURATION_WORKER_UTILIZATION).
DEFAULT_WORKER_THRESHOLD: Final = 0.7
# A limit allows twice the memory a measured peak needed.
MEMORY_HEADROOM: Final = 2.0
# The host keeps a quarter of its memory, at least 1 GiB, for the database,
# the proxy and the system.
HOST_MEMORY_RESERVE_SHARE: Final = 0.25
HOST_MEMORY_RESERVE_MIB: Final = 1024
MINIMUM_APP_MEMORY_MIB: Final = 512


@dataclass(frozen=True, slots=True)
class Deployment:
    """What the operator will run: steps are measured toward it, settings describe it."""

    app_cpus: float
    memory_mib: int
    worker_threshold: float = DEFAULT_WORKER_THRESHOLD

    @property
    def workers(self) -> int:
        """One media worker per whole CPU of the app's quota."""
        return max(1, math.floor(self.app_cpus))


def deployment_for(
    host_cpus: int,
    host_mib: int,
    *,
    app_cpus: float | None,
    app_memory_mib: int | None,
    worker_threshold: float = DEFAULT_WORKER_THRESHOLD,
) -> Deployment:
    """Return all but one CPU and the memory beyond the host's reserve, unless chosen."""
    reserve = max(HOST_MEMORY_RESERVE_MIB, int(host_mib * HOST_MEMORY_RESERVE_SHARE))
    return Deployment(
        app_cpus=app_cpus or max(1.0, float(host_cpus - 1)),
        memory_mib=app_memory_mib or max(MINIMUM_APP_MEMORY_MIB, host_mib - reserve),
        worker_threshold=worker_threshold,
    )


@dataclass(frozen=True, slots=True)
class Projection:
    """What the deployment carries in meetings, bounded by its CPUs and its memory."""

    per_core: float
    cpu_participants: int
    participants: int
    limited_by: Literal["cpu", "memory"]
    memory_limit_mib: int
    lower_bound: bool
    mbps_per_participant: float


def project(ceilings: Mapping[Workload, Ceiling], deployment: Deployment) -> Projection:
    """Scale the per-worker meeting ceiling to the deployment, within its memory."""
    meetings = ceilings.get("meetings")
    size = meetings.ceiling if meetings is not None and meetings.ceiling else 0
    per_core = size / meetings.workers if meetings is not None and size else 0.0
    cpu_participants = math.floor(per_core * deployment.workers)
    participants = cpu_participants
    limited_by: Literal["cpu", "memory"] = "cpu"
    needed = 0.0
    if meetings is not None and size and meetings.memory_peak_bytes:
        per_participant = meetings.memory_peak_bytes / size * MEMORY_HEADROOM
        memory_participants = math.floor(deployment.memory_mib * MIB / per_participant)
        if memory_participants < participants:
            participants, limited_by = memory_participants, "memory"
        needed = per_participant * participants
    # The largest room as measured, and the webinar spread over the app's workers.
    for ceiling in ceilings.values():
        if ceiling.workload == "meetings" or not ceiling.memory_peak_bytes:
            continue
        spread = deployment.workers / max(1, ceiling.workers)
        scale = max(1.0, spread) if ceiling.workload == "webinar" else 1.0
        needed = max(needed, ceiling.memory_peak_bytes * scale * MEMORY_HEADROOM)
    memory_limit = 0
    if needed:
        rounded = math.ceil(needed / MIB / 256) * 256
        memory_limit = min(deployment.memory_mib, max(MINIMUM_APP_MEMORY_MIB, rounded))
    mbps = meetings.egress_mbps / size if meetings is not None and size else 0.0
    lower_bound = meetings is None or meetings.bound != "measured"
    return Projection(
        per_core, cpu_participants, participants, limited_by, memory_limit, lower_bound, mbps
    )


@dataclass(frozen=True, slots=True)
class Cost:
    """Cost per participant at the host's projected capacity."""

    participants: int
    limited_by: Literal["cpu", "memory", "network"]
    egress_gb_per_participant_hour: float
    dollars_per_1000_participant_hours: float
    participants_per_monthly_dollar: float


def cost_of(projection: Projection, prices: Prices) -> Cost:
    """Cost per 1,000 participant-hours with the host full around the clock."""
    limited_by: Literal["cpu", "memory", "network"] = projection.limited_by
    participants = projection.participants
    mbps = projection.mbps_per_participant
    if prices.port_mbps > 0 and mbps > 0:
        network_bound = int(prices.port_mbps * PORT_HEADROOM / mbps)
        if network_bound < participants:
            participants, limited_by = network_bound, "network"
    gb_per_hour = mbps * 3600 / 8 / 1000
    hours = participants * HOURS_PER_MONTH
    overage_gb = max(0.0, hours * gb_per_hour - prices.included_egress_tb * 1000)
    monthly = prices.monthly + overage_gb * prices.egress_per_gb
    per_1000 = monthly / hours * 1000 if hours else math.inf
    per_dollar = participants / prices.monthly if prices.monthly else math.inf
    return Cost(participants, limited_by, gb_per_hour, per_1000, per_dollar)


def recommendations(
    ceilings: Mapping[Workload, Ceiling],
    projection: Projection,
    deployment: Deployment,
    rmem_max: int,
) -> list[str]:
    """Environment and host settings for the deployment, from the measured ceilings."""
    lines = [f"SIMPLESTCHAT_CPUS={deployment.app_cpus:g}", f"MEDIA_WORKERS={deployment.workers}"]
    room = ceilings.get("large-meeting")
    if room is not None and room.ceiling:
        lines.append(
            f"MAX_PARTICIPANTS_PER_ROOM={max(2, math.floor(room.ceiling * ROOM_LIMIT_SHARE))}"
        )
    if projection.memory_limit_mib:
        lines.append(f"SIMPLESTCHAT_MEMORY_LIMIT={projection.memory_limit_mib}m")
    if deployment.worker_threshold != DEFAULT_WORKER_THRESHOLD:
        lines.append(f"CPU_SATURATION_WORKER_UTILIZATION={deployment.worker_threshold:g}")
    if rmem_max < SOCKET_BUFFER_CEILING:
        lines.append(
            "host sysctl: net.core.rmem_max=2097152 net.core.wmem_max=2097152 "
            + "(the workers ask for 1 MiB socket buffers)"
        )
    return lines


@dataclass(frozen=True, slots=True)
class Shape:
    """The quotas one workload is measured under."""

    server_cpus: float
    workers: int
    generator_cpus: float


# The generator emulates browsers (DTLS, SRTP, simulcast) and costs about 1.5
# times the server's CPU per participant, so per-core workloads give the server
# a quarter of the host. One room's ceiling depends on viewers spreading to a
# second worker, so that workload keeps two workers where the host has four
# CPUs and the deployment runs two. The rest, less a little for the engine,
# goes to the generator.
PER_CORE_SERVER_SHARE: Final = 0.25
ROOM_WORKERS: Final = 2
ENGINE_RESERVE_CPUS: Final = 0.2
MINIMUM_GENERATOR_CPUS: Final = 0.5


def shape_for(
    workload: Workload,
    host_cpus: int,
    *,
    deployment_workers: int,
    server_cpus: float | None = None,
    generator_cpus: float | None = None,
) -> Shape:
    """Return a workload's quotas on this host; an operator's choices win."""
    if server_cpus is None:
        if workload == "large-meeting":
            room = min(ROOM_WORKERS, max(1, host_cpus // 2), max(1, deployment_workers))
            server_cpus = float(room)
        else:
            server_cpus = float(max(1, round(host_cpus * PER_CORE_SERVER_SHARE)))
    if generator_cpus is None:
        spare = round(host_cpus - server_cpus - ENGINE_RESERVE_CPUS, 1)
        generator_cpus = max(MINIMUM_GENERATOR_CPUS, spare)
    return Shape(server_cpus, max(1, math.floor(server_cpus)), generator_cpus)


# --- Engine -------------------------------------------------------------------


@dataclass(frozen=True, slots=True)
class Engine:
    """The container engine's command line (docker or podman)."""

    executable: str

    def run(self, *args: str, timeout: float = 120, check: bool = True) -> str:
        """Run an engine command and return its standard output."""
        completed = subprocess.run(  # noqa: S603 -- resolved engine executable, argument list.
            [self.executable, *args], capture_output=True, text=True, timeout=timeout, check=False
        )
        if check and completed.returncode != 0:
            detail = (completed.stderr or completed.stdout).strip().splitlines() or ["no output"]
            message = f"{Path(self.executable).name} {args[0]} failed: {detail[-1]}"
            raise CapacityError(message)
        return completed.stdout

    def read(self, container: str, path: str) -> str:
        """Read a file inside a container."""
        return self.run("exec", container, "cat", path, timeout=20)

    def running(self, container: str) -> bool:
        """Whether a container is still running."""
        state = self.run(
            "inspect", "--format", "{{.State.Running}}", container, check=False, timeout=20
        )
        return state.strip() == "true"

    def oom_killed(self, container: str) -> bool:
        """Whether the kernel killed a container for exceeding its memory limit."""
        state = self.run(
            "inspect", "--format", "{{.State.OOMKilled}}", container, check=False, timeout=20
        )
        return state.strip() == "true"

    def logs(self, container: str) -> str:
        """Return a container's output with standard error interleaved (the server logs there)."""
        completed = subprocess.run(  # noqa: S603 -- resolved engine executable, argument list.
            [self.executable, "logs", container],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            timeout=60,
            check=False,
        )
        return completed.stdout

    def remove(self, container: str) -> None:
        """Remove a container this tool started, if it exists."""
        _ = self.run("rm", "--force", container, check=False, timeout=60)


def find_engine(preferred: str | None) -> Engine:
    """Use the requested engine, else docker, else podman."""
    for candidate in (preferred,) if preferred else ("docker", "podman"):
        executable = shutil.which(candidate) if candidate else None
        if executable:
            return Engine(executable)
    message = "Neither docker nor podman is on PATH"
    raise CapacityError(message)


def free_port() -> int:
    """Return a currently free TCP port on 127.0.0.1."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.bind(("127.0.0.1", 0))
        return int(cast("tuple[str, int]", probe.getsockname())[1])


def scrape(port: int, token: str) -> dict[str, float]:
    """Read the server's metrics (empty when unavailable)."""
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}/metrics", headers={"Authorization": f"Bearer {token}"}
    )
    try:
        # A fixed loopback http URL built above.
        opened = cast("HTTPResponse", urllib.request.urlopen(request, timeout=5))  # noqa: S310
        with opened as response:
            return parse_prometheus(response.read().decode())
    except (urllib.error.URLError, TimeoutError, ConnectionError):
        return {}


def wait_ready(port: int, deadline: float) -> None:
    """Wait for the server's health endpoint."""
    while time.monotonic() < deadline:
        try:
            health = f"http://127.0.0.1:{port}/health"
            opened = cast("HTTPResponse", urllib.request.urlopen(health, timeout=2))
            with opened as response:
                if response.status == HTTP_OK:
                    return
        except (urllib.error.URLError, TimeoutError, ConnectionError):
            pass
        time.sleep(0.5)
    message = "the server did not become healthy"
    raise CapacityError(message)


def image_facts(engine: Engine, image: str) -> dict[str, str]:
    """Return an image's id and source revision label."""
    image_id = engine.run("image", "inspect", "--format", "{{.Id}}", image).strip()
    revision = engine.run(
        "image",
        "inspect",
        "--format",
        '{{index .Config.Labels "org.opencontainers.image.revision"}}',
        image,
    ).strip()
    return {
        "reference": image,
        "id": image_id,
        "revision": "" if revision == "<no value>" else revision,
    }


# --- Running a step -----------------------------------------------------------


@dataclass(frozen=True, slots=True)
class Context:
    """Everything the steps share."""

    engine: Engine
    run_id: str
    server_image: str
    generator_image: str
    revisions: tuple[str, str]
    shape: Shape
    # The per-worker guard the server runs with and the judge applies.
    worker_threshold: float
    server_memory: str
    generator_memory: str
    browser: Browser
    output: Path
    log: Callable[[str], None]


@dataclass(frozen=True, slots=True)
class Mark:
    """CPU readings at one edge of the measurement window."""

    at: float
    server: CpuStat
    generator: CpuStat
    host: HostTimes


# CPU readings as clients start joining, and at the window's start and end.
Marks = tuple[Mark | None, Mark | None, Mark | None]


def take_mark(engine: Engine, server: str, generator: str) -> Mark:
    """Read server, generator and host CPU at this instant."""
    return Mark(
        time.monotonic(),
        parse_cpu_stat(engine.read(server, "/sys/fs/cgroup/cpu.stat")),
        parse_cpu_stat(engine.read(generator, "/sys/fs/cgroup/cpu.stat")),
        parse_proc_stat(engine.read(server, "/proc/stat")),
    )


def apply_marks(result: StepResult, start: Mark, end: Mark) -> None:
    """CPU use and throttling of server, generator and host over the window."""
    seconds = max(end.at - start.at, 1.0)
    result.server_cores = (end.server.usage_usec - start.server.usage_usec) / 1e6 / seconds
    result.server_throttled = throttled_share(start.server, end.server)
    result.generator_cores = (end.generator.usage_usec - start.generator.usage_usec) / 1e6 / seconds
    result.generator_throttled = throttled_share(start.generator, end.generator)
    total = end.host.total - start.host.total
    if total > 0:
        result.host_steal = (end.host.steal - start.host.steal) / total
        result.host_busy = 1 - (end.host.idle - start.host.idle) / total


def server_command(context: Context, name: str, port: int, token: str) -> list[str]:
    """`run` arguments for the owned server container."""
    environment = {
        "BIND_ADDR": "0.0.0.0",  # noqa: S104 -- inside the container; published to 127.0.0.1 only.
        "PORT": str(SERVER_PORT),
        "ANNOUNCE_IP": "127.0.0.1",
        "MEDIA_WORKERS": str(context.shape.workers),
        "CPU_SATURATION_WORKER_UTILIZATION": f"{context.worker_threshold:g}",
        "ALLOW_AD_HOC_ROOMS": "true",
        "ALLOWED_ORIGINS": f"http://127.0.0.1:{SERVER_PORT}",
        "METRICS_TOKEN": token,
        "RUST_LOG": "simplestChat=warn,mediasoup=warn",
    }
    return [
        "run", "--detach", "--name", name,
        "--cpus", f"{context.shape.server_cpus:g}",
        "--memory", context.server_memory, "--memory-swap", context.server_memory,
        "--pids-limit", "4096", "--read-only",
        "--tmpfs", "/tmp:rw,nosuid,nodev,noexec,size=64m",  # noqa: S108 -- container tmpfs.
        "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
        "--publish", f"127.0.0.1:{port}:{SERVER_PORT}/tcp",
        *(item for key, value in environment.items() for item in ("--env", f"{key}={value}")),
        context.server_image,
    ]  # fmt: skip


@dataclass(slots=True)
class Watch:
    """Periodic observations of one running step."""

    engine: Engine
    server: str
    generator: str
    port: int
    token: str
    window: tuple[float, float]
    result: StepResult
    # Scrapes count only when they read every one of these workers.
    workers: int = 1
    setup_mark: Mark | None = None
    start_mark: Mark | None = None
    end_mark: Mark | None = None
    peaks: dict[int, float] = field(default_factory=dict[int, float])
    sums: dict[int, float] = field(default_factory=dict[int, float])
    scrapes: int = 0
    next_scrape: float = 0.0
    next_liveness: float = 0.0
    next_memory: float = 0.0

    def alive(self, now: float) -> bool:
        """Check both containers every few seconds; False once the generator is done."""
        if now < self.next_liveness:
            return True
        self.next_liveness = now + LIVENESS_POLL_SECONDS
        if not self.engine.running(self.generator):
            return False
        if not self.engine.running(self.server):
            self.result.server_exit = "the server stopped during the run"
            return False
        return True

    def memory(self, now: float) -> None:
        """Track the server's peak cgroup memory."""
        if now < self.next_memory:
            return
        self.next_memory = now + MEMORY_POLL_SECONDS
        for container in (self.server, self.generator):
            try:
                value = self.engine.read(container, "/sys/fs/cgroup/memory.current").strip()
            except CapacityError:
                continue  # A stopped container is the liveness check's to report.
            if not value.isdigit():
                continue
            if container == self.server:
                self.result.memory_peak_bytes = max(self.result.memory_peak_bytes, int(value))
            else:
                peak = max(self.result.generator_memory_peak_bytes, int(value))
                self.result.generator_memory_peak_bytes = peak

    def marks(self, now: float) -> None:
        """Read CPU as clients start joining, and at the window's start and end."""
        try:
            if self.setup_mark is None and now < self.window[0]:
                self.setup_mark = take_mark(self.engine, self.server, self.generator)
            if self.start_mark is None and now >= self.window[0]:
                self.start_mark = take_mark(self.engine, self.server, self.generator)
                self.next_scrape = now + SCRAPE_SECONDS
            if self.end_mark is None and self.start_mark is not None and now >= self.window[1]:
                self.end_mark = take_mark(self.engine, self.server, self.generator)
        except CapacityError:
            # A container that just stopped is the liveness check's to report; any other
            # failure would leave the step without its CPU figures.
            if self.engine.running(self.server) and self.engine.running(self.generator):
                raise

    def loads(self, now: float) -> None:
        """Scrape the workers' 10-second loads through the window."""
        if self.start_mark is None or self.end_mark is not None or now < self.next_scrape:
            return
        self.next_scrape = now + SCRAPE_SECONDS
        self.record_scrape(scrape(self.port, self.token))

    def record_scrape(self, metrics: Mapping[str, float]) -> None:
        """Count a scrape that read every worker; note the server's own saturation."""
        if metrics.get("simplestchat_cpu_saturated", 0.0) >= 1:
            self.result.server_saturated_scrapes += 1
        loads = worker_loads(metrics)
        if len(loads) < self.workers:
            return
        self.scrapes += 1
        for worker, value in loads.items():
            self.peaks[worker] = max(self.peaks.get(worker, 0.0), value)
            self.sums[worker] = self.sums.get(worker, 0.0) + value

    def finish(self) -> None:
        """Record the worker loads on the result."""
        self.result.worker_load_peak = dict(self.peaks)
        self.result.load_scrapes = self.scrapes
        if self.scrapes:
            self.result.worker_load_mean = {
                worker: total / self.scrapes for worker, total in self.sums.items()
            }


# How long a generator may overrun its window before the step is abandoned.
OVERRUN_SECONDS: Final = 180
# The generator's clock starts before `run --detach` returns and it exits as its
# own window closes: the CPU window ends this much earlier, so both marks are
# read while it still runs.
WINDOW_END_GUARD_SECONDS: Final = 5


def step_window(started: float, plan: StepPlan) -> tuple[float, float]:
    """Return the measurement window on the monotonic clock for a generator started then."""
    start = started + plan.ramp_seconds + plan.warmup_seconds
    return start, start + plan.duration_seconds - WINDOW_END_GUARD_SECONDS


def monitor(
    context: Context,
    plan: StepPlan,
    names: tuple[str, str],
    endpoint: tuple[int, str],
    result: StepResult,
) -> Marks:
    """Watch a running step: memory, worker loads in the window, and the window's CPU marks."""
    watch = Watch(
        engine=context.engine,
        server=names[0],
        generator=names[1],
        port=endpoint[0],
        token=endpoint[1],
        window=step_window(time.monotonic(), plan),
        result=result,
        workers=context.shape.workers,
    )
    deadline = watch.window[1] + OVERRUN_SECONDS
    while watch.alive(now := time.monotonic()):
        if now > deadline:
            result.error = "the generator did not finish before its deadline"
            break
        watch.memory(now)
        watch.marks(now)
        watch.loads(now)
        time.sleep(1)
    watch.finish()
    return watch.setup_mark, watch.start_mark, watch.end_mark


def run_step(context: Context, plan: StepPlan, index: int) -> StepResult:
    """Run one workload size and collect what the server and the kernel saw."""
    name = f"{context.run_id}-{index}"
    server, generator = f"capacity-server-{name}", f"capacity-gen-{name}"
    port, token = free_port(), secrets.token_hex(24)
    engine = context.engine
    directory = context.output / f"{index:02d}-{plan.workload}-{plan.size}"
    directory.mkdir(parents=True, exist_ok=False)
    ports = [WORKER_PORT_BASE + worker for worker in range(context.shape.workers)]
    started_at = datetime.now(UTC).isoformat(timespec="seconds")
    result = StepResult(plan.workload, plan.size, plan.clients, plan.rooms, started_at)
    marks: Marks = (None, None, None)
    try:
        _ = engine.run(*server_command(context, server, port, token))
        wait_ready(port, time.monotonic() + 90)
        before = parse_udp(
            engine.read(server, "/proc/net/snmp"), engine.read(server, "/proc/net/udp"), ports
        )
        memory_limit = engine.read(server, "/sys/fs/cgroup/memory.max").strip()
        result.memory_limit_bytes = int(memory_limit) if memory_limit.isdigit() else 0
        label = f"capacity-{plan.workload}-{plan.size}"
        _ = engine.run(
            "run", "--detach", "--name", generator, "--network", f"container:{server}",
            "--cpus", f"{context.shape.generator_cpus:g}", "--memory", context.generator_memory,
            "--pids-limit", "8192", "--env", "RUST_LOG=warn", context.generator_image,
            *plan.generator_args(context.browser, label, context.revisions),
        )  # fmt: skip
        marks = monitor(context, plan, (server, generator), (port, token), result)
        if engine.running(server):
            after = parse_udp(
                engine.read(server, "/proc/net/snmp"), engine.read(server, "/proc/net/udp"), ports
            )
            result.namespace_datagrams = after.in_datagrams - before.in_datagrams
            result.socket_drops = after.socket_drops - before.socket_drops
            result.rcvbuf_errors = after.rcvbuf_errors - before.rcvbuf_errors
            final = scrape(port, token)
            result.refused_joins = int(final.get("simplestchat_joins_refused_saturated_total", 0))
            _ = (directory / "metrics.txt").write_text(
                "\n".join(f"{key} {value:g}" for key, value in sorted(final.items())),
                encoding="utf-8",
            )
        elif not result.server_exit:
            result.server_exit = "the server stopped during the run"
    except (CapacityError, subprocess.TimeoutExpired) as error:
        result.error = str(error)
    finally:
        # Evidence first, whatever happened; removal must run even if collecting fails.
        with contextlib.suppress(CapacityError, subprocess.TimeoutExpired, OSError):
            collect(engine, (server, generator), directory)
            if engine.oom_killed(server):
                result.server_exit = "the server ran out of memory"
            result.generator_out_of_memory = engine.oom_killed(generator)
        engine.remove(generator)
        engine.remove(server)
    conclude(result, marks, directory / "load_test_summary.json")
    _ = (directory / "step.json").write_text(json.dumps(record(result), indent=2), encoding="utf-8")
    return result


# The last part of each container's log a step keeps.
LOG_TAIL_CHARACTERS: Final = 200_000


def collect(engine: Engine, names: tuple[str, str], directory: Path) -> None:
    """Keep a step's evidence: the generator's results and both containers' logs."""
    server, generator = names
    _ = engine.run("cp", f"{generator}:/results/.", str(directory), check=False, timeout=120)
    for container, file in ((generator, "generator.log"), (server, "server.log")):
        log = engine.logs(container)[-LOG_TAIL_CHARACTERS:]
        _ = (directory / file).write_text(log, encoding="utf-8")


# A window's worth of per-worker loads: fewer scrapes cannot say how busy it was.
MINIMUM_LOAD_SCRAPES: Final = 2


def conclude(result: StepResult, marks: Marks, summary: Path) -> None:
    """Fill a finished step from its CPU marks and the generator's summary."""
    setup, start, end = marks
    if start is not None and end is not None:
        apply_marks(result, start, end)
    if setup is not None and start is not None:
        result.generator_setup_throttled = throttled_share(setup.generator, start.generator)
    if not summary.exists():
        if not result.error and not result.generator_out_of_memory:
            result.error = "the generator wrote no summary"
        return
    try:
        read_summary(result, read_json(summary))
        clients = summary.with_name("load_test_results.json")
        if clients.exists():
            listed = cast("object", json.loads(clients.read_text(encoding="utf-8")))
            read_clients(result, as_list(listed, "clients"))
    except (CapacityError, ValueError) as error:
        result.error = result.error or f"the generator's summary is unusable: {error}"
        return
    # Without the window's readings a pass or a failure may be the generator's
    # doing; only a server that stopped needs none.
    if result.error or result.server_exit or result.generator_out_of_memory:
        return
    if end is None:
        result.error = "no CPU readings cover the window, so the generator's throttling is unknown"
    elif result.load_scrapes < MINIMUM_LOAD_SCRAPES:
        result.error = (
            "the server's per-worker loads were not readable through the window, "
            + "so its busiest worker is unknown"
        )


# A failing run can list one reason per client; reports keep the first few.
REPORTED_FAILURES: Final = 5


def read_clients(result: StepResult, clients: Sequence[object]) -> None:
    """Count starved video consumers and viewers at the cap from per-client results."""
    for entry in clients:
        client = as_object(entry, "client")
        bitrate = client.get("lastAvailableBitrate")
        at_cap = AT_CAP_SHARE * VIEWER_BITRATE_CAP
        if bitrate is not None and as_number(bitrate, "bitrate") >= at_cap:
            result.viewers_at_cap += 1
        for delivery in as_list(client.get("consumerDelivery", []), "consumerDelivery"):
            consumer = as_object(delivery, "consumer")
            if consumer.get("passed") is False and consumer.get("isAudio") is False:
                result.failed_video_consumers += 1


def read_summary(result: StepResult, summary: Mapping[str, object]) -> None:
    """Copy the generator summary's delivery figures into a step result."""

    def count(source: Mapping[str, object], key: str) -> int:
        return int(as_number(source.get(key, 0), key))

    run = as_object(summary.get("run"), "run")
    result.generator_passed = run.get("passed") is True
    failures = [str(reason) for reason in as_list(run.get("failureReasons", []), "failureReasons")]
    result.generator_failures = failures[:REPORTED_FAILURES]
    result.generator_failure_count = len(failures)
    result.generator_errors = count(summary, "totalErrors")
    result.failed_connections = count(summary, "failedConnections")
    result.failed_consumers = count(summary, "failedConsumers")
    result.validated_consumers = count(summary, "validatedConsumers")
    # A generator past its hard deadline writes only its failure reasons.
    if "measurement" not in summary:
        return
    if "totalDatagramsSent" not in summary:
        # RTP alone leaves out the viewers' feedback: a webinar's drops came to 110 %.
        message = "the generator does not count its datagrams; rebuild the load-test image"
        raise CapacityError(message)
    result.sent_datagrams = count(summary, "totalDatagramsSent")
    measurement = as_object(summary["measurement"], "measurement")
    result.received_packets = count(measurement, "packetsReceived")
    result.received_payload_bytes = count(measurement, "bytesReceived")
    result.measurement_seconds = as_number(measurement.get("durationMs", 0), "durationMs") / 1000
    ready = summary.get("receiveMediaReady")
    if isinstance(ready, dict):
        result.receive_ready_p99_ms = as_number(
            cast("dict[str, object]", ready).get("p99Ms", 0), "p99Ms"
        )


# --- The calibration ------------------------------------------------------------


def host_facts(engine: Engine, image: str) -> dict[str, object]:
    """CPU model, logical CPUs, memory, idle load and socket buffer ceilings."""
    script = (
        "cat /proc/cpuinfo; echo @@; cat /proc/meminfo; echo @@; cat /proc/stat; echo @@; "
        "sleep 5; cat /proc/stat; echo @@; cat /proc/sys/net/core/rmem_max; echo @@; "
        "cat /proc/sys/kernel/osrelease; echo @@; "
        "cat /sys/class/dmi/id/sys_vendor /sys/class/dmi/id/product_name 2>/dev/null; true"
    )
    output = engine.run(
        "run", "--rm", "--network", "none", "--entrypoint", "sh", image, "-c", script, timeout=120
    )
    parts = [part.strip() for part in output.split("@@")]
    if len(parts) != len(("cpuinfo", "meminfo", "stat", "stat", "rmem", "kernel", "dmi")):
        message = "could not read the host's /proc facts through the server image"
        raise CapacityError(message)
    cpuinfo, meminfo, stat_a, stat_b, rmem, kernel, dmi = parts
    a, b = parse_proc_stat(stat_a), parse_proc_stat(stat_b)
    total = max(1, b.total - a.total)
    return {
        "cpuModel": parse_cpu_model(cpuinfo),
        "platform": parse_platform(dmi),
        "logicalCpus": count_cpus(stat_a),
        "memoryMib": parse_meminfo_kib(meminfo) // 1024,
        "idleBusyShare": round(1 - (b.idle - a.idle) / total, 4),
        "idleStealShare": round((b.steal - a.steal) / total, 4),
        "rmemMax": int(rmem) if rmem.isdigit() else 0,
        "kernel": kernel,
    }


def search_workload(
    context: Context, search: Search, limits: Limits, timing: tuple[int, int], first_index: int
) -> list[tuple[StepResult, Verdict]]:
    """Grow one workload until its ceiling is bracketed or out of reach."""
    history: list[tuple[StepResult, Verdict]] = []
    while True:
        trials = [
            Trial(
                step.size,
                verdict.passed,
                verdict.valid,
                step.busiest_worker,
                step.mean_worker,
                step.generator_cores,
            )
            for step, verdict in history
        ]
        size = next_size(search, trials, limits, context.shape.generator_cpus)
        if size is None:
            return history
        plan = StepPlan(
            search.workload,
            size,
            search.meeting_size,
            timing[0],
            timing[1],
            context.shape.workers,
        )
        context.log(f"{search.workload} at {size} ({plan.clients} clients in {plan.rooms} rooms)")
        step = run_step(context, plan, first_index + len(history))
        verdict = judge(step, limits)
        history.append((step, verdict))
        outcome = "pass" if verdict.passed else ("invalid" if not verdict.valid else "fail")
        detail = f": {'; '.join(verdict.reasons)}" if verdict.reasons else ""
        context.log(
            f"  {outcome}, busiest worker {step.busiest_worker:.2f}, drops {step.drop_share:.3%}, "
            + f"generator {step.generator_cores:.2f} cores{detail}"
        )
        if step.host_steal > limits.steal_share:
            context.log(
                f"  warning: the hypervisor withheld {step.host_steal:.0%} of the host's CPU "
                + "(steal); repeat the calibration to see whether that is typical here"
            )


@dataclass
class Options(argparse.Namespace):
    """Command-line options."""

    command: str = ""
    server_image: str = ""
    generator_image: str = ""
    engine: str | None = None
    output: str = ""
    label: str = ""
    workloads: list[str] = field(default_factory=lambda: list(WORKLOADS))
    meeting_size: int = 5
    server_cpus: float | None = None
    generator_cpus: float | None = None
    app_cpus: float | None = None
    app_memory_mib: int | None = None
    worker_threshold: float = DEFAULT_WORKER_THRESHOLD
    quick: bool = False
    capture: str = "720p"
    speakers: int = 1
    viewport_width: int = 1440
    pixel_ratio: float = 2.0
    layout: str = "classic"
    monthly_price: float | None = None
    egress_price_per_gb: float = 0.0
    included_egress_tb: float = 0.0
    port_mbps: float = 0.0
    reports: list[str] = field(default_factory=list[str])


def memory_limit(host_mib: int, share: float, most_mib: int) -> str:
    """Return a container memory limit: a share of the host, 512 MiB up to `most_mib`."""
    return f"{max(512, min(most_mib, int(host_mib * share)))}m"


def host_warnings(host: Mapping[str, object]) -> list[str]:
    """Conditions that make this host's figures low or rough."""
    warnings: list[str] = []
    busy = as_number(host["idleBusyShare"], "idleBusyShare")
    if busy > BUSY_HOST_FRACTION:
        warnings.append(f"the host was {busy:.0%} busy before any load")
    if as_number(host["logicalCpus"], "logicalCpus") < 2:  # noqa: PLR2004 -- one each.
        warnings.append("one CPU cannot hold both the server and the generator; figures are rough")
    if as_number(host["rmemMax"], "rmemMax") < SOCKET_BUFFER_CEILING:
        warnings.append(
            "net.core.rmem_max clamps the workers' 1 MiB socket buffers, so the kernel drops "
            + "bursts the server would carry and the ceilings come out low; first run "
            + "sysctl -w net.core.rmem_max=2097152 net.core.wmem_max=2097152"
        )
    return warnings


def search_for(workload: Workload, shape: Shape, meeting_size: int, steps: int) -> Search:
    """Where a workload's search starts, how it steps, and its floor."""
    if workload == "meetings":
        first = meeting_size * 2 * shape.workers
        return Search(workload, meeting_size, first, meeting_size, steps, meeting_size)
    if workload == "large-meeting":
        return Search(workload, meeting_size, 8, 1, steps, 2)
    return Search(workload, meeting_size, 25 * shape.workers, 5, steps, 5)


def calibrate(options: Options) -> int:
    """Run the `run` command: calibrate this host."""
    engine = find_engine(options.engine)
    output = Path(options.output or f"results/capacity.{datetime.now(UTC):%Y%m%dT%H%M%SZ}")
    output.mkdir(parents=True, exist_ok=False)
    log_path = output / "calibration.log"

    def log(message: str) -> None:
        line = f"{datetime.now(UTC):%H:%M:%S} {message}"
        print(line, flush=True)  # noqa: T201 -- intentional progress output.
        with log_path.open("a", encoding="utf-8") as handle:
            _ = handle.write(line + "\n")

    images = {
        "server": image_facts(engine, options.server_image),
        "generator": image_facts(engine, options.generator_image),
    }
    log("reading host facts (a 5 s idle sample)")
    host = host_facts(engine, options.server_image)
    cpus = int(as_number(host["logicalCpus"], "logicalCpus"))
    host_mib = int(as_number(host["memoryMib"], "memoryMib"))
    deployment = deployment_for(
        cpus,
        host_mib,
        app_cpus=options.app_cpus,
        app_memory_mib=options.app_memory_mib,
        worker_threshold=options.worker_threshold,
    )
    shapes: dict[Workload, Shape] = {
        workload: shape_for(
            workload,
            cpus,
            deployment_workers=deployment.workers,
            server_cpus=options.server_cpus,
            generator_cpus=options.generator_cpus,
        )
        for workload in WORKLOADS
    }
    for warning in host_warnings(host):
        log(f"warning: {warning}")
    context = Context(
        engine=engine,
        run_id=secrets.token_hex(4),
        server_image=options.server_image,
        generator_image=options.generator_image,
        revisions=(
            images["server"]["revision"][:16] or "unknown",
            images["generator"]["revision"][:16] or "unknown",
        ),
        shape=shapes["meetings"],
        worker_threshold=deployment.worker_threshold,
        # The generator holds a WebRTC stack per participant; the server's
        # memory guard works from its own limit.
        server_memory=memory_limit(host_mib, 0.3, 4096),
        generator_memory=memory_limit(host_mib, 0.5, 8192),
        browser=Browser(
            options.capture,
            options.speakers,
            options.viewport_width,
            options.pixel_ratio,
            options.layout,
        ),
        output=output,
        log=log,
    )
    limits = Limits(worker_load=deployment.worker_threshold)
    timing = (20, 30) if options.quick else (30, 60)
    steps = 3 if options.quick else 6
    log(
        f"host {cpus} CPUs, {host['cpuModel']}; server {context.server_memory}, "
        + f"generator {context.generator_memory}"
    )
    results: dict[Workload, list[tuple[StepResult, Verdict]]] = {}
    index = 1
    for workload in WORKLOADS:
        if workload in options.workloads:
            shape = shapes[workload]
            log(
                f"{workload}: server {shape.server_cpus:g} CPUs ({shape.workers} workers), "
                + f"generator {shape.generator_cpus:g} CPUs"
            )
            scoped = replace(context, shape=shape)
            plan = search_for(workload, shape, options.meeting_size, steps)
            results[workload] = search_workload(scoped, plan, limits, timing, index)
            index += len(results[workload])
    ceilings: dict[Workload, Ceiling] = {
        workload: ceiling_of(
            workload,
            results.get(workload, []),
            shapes[workload].generator_cpus,
            workers=shapes[workload].workers,
        )
        for workload in WORKLOADS
    }
    report = build_report(
        options,
        Measured(
            host=host,
            images=images,
            context=context,
            limits=limits,
            deployment=deployment,
            shapes=shapes,
        ),
        results,
        ceilings,
    )
    _ = (output / "calibration.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
    for line in summary_lines(report):
        print(line)  # noqa: T201 -- intentional report output.
    print(f"\nReport: {output / 'calibration.json'}")  # noqa: T201 -- intentional report output.
    return 0


@dataclass(frozen=True, slots=True)
class Measured:
    """How the calibration was measured."""

    host: Mapping[str, object]
    images: Mapping[str, Mapping[str, str]]
    context: Context
    limits: Limits
    deployment: Deployment
    shapes: Mapping[Workload, Shape]


def build_report(
    options: Options,
    measured: Measured,
    results: Mapping[Workload, Sequence[tuple[StepResult, Verdict]]],
    ceilings: Mapping[Workload, Ceiling],
) -> dict[str, object]:
    """Build the machine-readable calibration report."""
    context, deployment = measured.context, measured.deployment
    projection = project(ceilings, deployment)
    cost: dict[str, object] | None = None
    if options.monthly_price is not None and projection.participants:
        prices = Prices(
            options.monthly_price,
            options.egress_price_per_gb,
            options.included_egress_tb,
            options.port_mbps,
        )
        cost = record(cost_of(projection, prices)) | {"prices": record(prices)}
    steps: list[dict[str, object]] = [
        record(step)
        | {
            "passed": verdict.passed,
            "valid": verdict.valid,
            "reasons": list(verdict.reasons),
            "busiestWorker": step.busiest_worker,
            "dropShare": step.drop_share,
            "egressMbps": step.egress_mbps,
        }
        for trials in results.values()
        for step, verdict in trials
    ]
    rmem = int(as_number(measured.host.get("rmemMax", 0), "rmemMax"))
    return {
        "schemaVersion": SCHEMA_VERSION,
        "label": options.label or str(measured.host.get("platform") or measured.host["cpuModel"]),
        "finishedAt": datetime.now(UTC).isoformat(timespec="seconds"),
        "host": dict(measured.host),
        "images": {key: dict(value) for key, value in measured.images.items()},
        "measurement": {
            "shapes": {workload: record(shape) for workload, shape in measured.shapes.items()},
            "joinsPerSecond": {
                workload: join_rate(workload, shape.workers)
                for workload, shape in measured.shapes.items()
            },
            "serverMemory": context.server_memory,
            "generatorMemory": context.generator_memory,
            "browser": record(context.browser),
            "limits": record(measured.limits),
            "meetingSize": options.meeting_size,
        },
        "ceilings": {workload: record(ceiling) for workload, ceiling in ceilings.items()},
        "deployment": record(deployment) | {"workers": deployment.workers},
        "projection": {
            "appCpus": deployment.app_cpus,
            "meetingParticipantsPerCore": round(projection.per_core, 2),
            "cpuParticipants": projection.cpu_participants,
            "meetingParticipants": projection.participants,
            "limitedBy": projection.limited_by,
            # Without a failure above it the ceiling, and so the projection, is a floor.
            "lowerBound": projection.lower_bound,
            "egressMbpsPerParticipant": round(projection.mbps_per_participant, 3),
        },
        "recommendations": recommendations(ceilings, projection, deployment, rmem),
        "cost": cost,
        "steps": steps,
    }


LOWER_BOUND_REASONS: Final = {
    "generator-limited": "the generator ran out of CPU first",
    "step-limited": "the search ran out of steps",
    "interrupted": "a later step could not run; see its step.json",
}


def describe_ceiling(entry: Mapping[str, object], unit: str = "") -> str:
    """Describe a ceiling in words: a size, or at least a size and why no more is known."""
    bound = str(entry.get("bound"))
    value = entry.get("ceiling")
    if bound == "not-run":
        return "not measured"
    if bound == "below-first-size":
        return "below the first size tried"
    if value is None:
        return "not measured (a step could not run; see its step.json)"
    size = f"{int(as_number(value, 'ceiling'))}{' ' + unit if unit else ''}"
    reason = LOWER_BOUND_REASONS.get(bound)
    return f"at least {size} ({reason})" if reason else size


def summary_lines(report: Mapping[str, object]) -> list[str]:
    """Summarize a report for people."""
    ceilings = as_object(report["ceilings"], "ceilings")
    projection = as_object(report["projection"], "projection")
    measurement = as_object(report["measurement"], "measurement")
    host = as_object(report["host"], "host")

    def ceiling(workload: str, people: str) -> str:
        entry = as_object(ceilings[workload], workload)
        workers = int(as_number(entry.get("workers", 1), "workers"))
        return describe_ceiling(entry, f"{people} on {workers} worker{'s' * (workers != 1)}")

    platform = f" ({host['platform']})" if host.get("platform") else ""
    estimate = "at least" if projection.get("lowerBound") is True else "about"

    def bounded(workload: str) -> list[str]:
        limit = as_object(ceilings[workload], workload).get("limit")
        return [f"    bounded by: {limit}"] if isinstance(limit, str) and limit else []

    lines = [
        "",
        f"Host {report['label']}: {host['logicalCpus']} CPUs, {host['cpuModel']}, "
        + f"{host['memoryMib']} MiB{platform}",
        f"  meetings of {measurement['meetingSize']}: {ceiling('meetings', 'participants')}",
        *bounded("meetings"),
        f"  largest meeting: {ceiling('large-meeting', 'participants')}",
        *bounded("large-meeting"),
        f"  largest webinar: {ceiling('webinar', 'viewers')}",
        *bounded("webinar"),
        f"Projected to {projection['appCpus']} app CPUs: {estimate} "
        + f"{projection['meetingParticipants']} participants in meetings "
        + f"({projection['meetingParticipantsPerCore']} per core"
        + (
            f"; memory holds fewer than the {projection['cpuParticipants']} the CPUs carry)"
            if projection.get("limitedBy") == "memory"
            else ")"
        )
        + f", {projection['egressMbpsPerParticipant']} Mbit/s each",
        "Settings:",
        *(f"  {line}" for line in as_list(report["recommendations"], "recommendations")),
    ]
    large = as_object(ceilings["large-meeting"], "large-meeting").get("ceiling")
    if isinstance(large, int) and large > FULL_MEDIA_ROOM_LIMIT:
        lines.append(
            f"  note: beyond {FULL_MEDIA_ROOM_LIMIT} publishers each browser shows only 32 peers "
            + "(MAX_CONSUMERS_PER_PARTICIPANT=64)"
        )
    cost = report.get("cost")
    if isinstance(cost, dict):
        entry = cast("dict[str, object]", cost)
        dollars = as_number(entry["dollarsPer1000ParticipantHours"], "cost")
        per_dollar = as_number(entry["participantsPerMonthlyDollar"], "cost")
        lines.append(
            f"Cost: ${dollars:.3f} per 1,000 participant-hours at full use "
            + f"({entry['limitedBy']}-bound, {per_dollar:.1f} participants per monthly dollar)"
        )
    return lines


def comparison_row(report: Mapping[str, object]) -> tuple[float, str]:
    """One report as a ranked comparison row."""
    host = as_object(report["host"], "host")
    projection = as_object(report["projection"], "projection")
    ceilings = as_object(report["ceilings"], "ceilings")

    def shown(entry: Mapping[str, object]) -> str:
        """Return a ceiling as a table cell; `≥` marks a lower bound."""
        value = entry.get("ceiling")
        if value is None:
            return "-"
        return f"≥{value}" if entry.get("bound") in LOWER_BOUND_REASONS else str(value)

    room, webinar = (shown(as_object(ceilings[key], key)) for key in ("large-meeting", "webinar"))
    participants = projection["meetingParticipants"]
    meet = f"≥{participants}" if projection.get("lowerBound") is True else str(participants)
    cost = report.get("cost")
    per_1000, price = math.inf, "-"
    if isinstance(cost, dict):
        entry = cast("dict[str, object]", cost)
        per_1000 = as_number(entry["dollarsPer1000ParticipantHours"], "cost")
        price = f"${as_number(as_object(entry['prices'], 'prices')['monthly'], 'price'):g}"
    dollars = f"{per_1000:.3f}" if math.isfinite(per_1000) else "-"
    row = (
        f"{report['label']!s:<24} {host['logicalCpus']!s:>4} {str(host['cpuModel'])[:28]:<28} "
        f"{price:>8} {meet:>6} {room:>5} {webinar:>6} "
        f"{projection['egressMbpsPerParticipant']!s:>6} {dollars:>9}"
    )
    return per_1000, row


COMPARISON_HEADER: Final = (
    f"{'host':<24} {'cpus':>4} {'model':<28} {'price':>8} {'meet':>6} {'room':>5} {'webin':>6} "
    f"{'Mb/s':>6} {'$/1k ph':>9}"
)


def compare(paths: Sequence[str]) -> int:
    """Rank hosts by cost per 1,000 participant-hours (the `compare` command)."""
    rows = sorted(comparison_row(read_json(Path(path))) for path in paths)
    print(COMPARISON_HEADER)  # noqa: T201 -- intentional report output.
    for _, row in rows:
        print(row)  # noqa: T201 -- intentional report output.
    return 0


def parser() -> argparse.ArgumentParser:
    """Build the command-line interface."""
    root = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    commands = root.add_subparsers(dest="command", required=True)
    # Unset options must not replace the typed defaults: a subparser copies its
    # namespace, defaults included, over the caller's.
    run = commands.add_parser("run", help="calibrate this host", argument_default=argparse.SUPPRESS)
    for flag, help_text in (
        ("--server-image", "production server image"),
        ("--generator-image", "load-test image (Dockerfile target loadtest)"),
    ):
        _ = run.add_argument(flag, required=True, help=help_text)
    _ = run.add_argument("--engine", choices=("docker", "podman"), help="default: auto")
    _ = run.add_argument("--output", help="report directory (default: results/capacity.<time>)")
    _ = run.add_argument("--label", help="this host's name in comparisons")
    _ = run.add_argument("--workloads", nargs="+", choices=WORKLOADS)
    _ = run.add_argument("--meeting-size", type=int, help="people in an everyday meeting (5)")
    _ = run.add_argument(
        "--server-cpus",
        type=float,
        help="server quota (default: a quarter of the host, two for large-meeting)",
    )
    _ = run.add_argument("--generator-cpus", type=float, help="generator quota (default: the rest)")
    _ = run.add_argument("--app-cpus", type=float, help="CPUs the app will get (host CPUs - 1)")
    _ = run.add_argument(
        "--app-memory-mib",
        type=int,
        help="memory the app will get (the host's less a quarter, at least 1 GiB)",
    )
    _ = run.add_argument(
        "--worker-threshold",
        type=float,
        help="the worker guard to run with, measured and recommended (0.7)",
    )
    _ = run.add_argument("--quick", action="store_true", help="shorter windows, fewer steps")
    _ = run.add_argument("--capture", choices=("720p", "1080p"), help="browsers' camera (720p)")
    _ = run.add_argument("--speakers", type=int, help="people talking at once per room (1)")
    _ = run.add_argument("--viewport-width", type=int, help="browser width in CSS px (1440)")
    _ = run.add_argument("--pixel-ratio", type=float, help="browser device pixel ratio (2)")
    _ = run.add_argument("--layout", choices=("classic", "modern"), help="web client layout")
    _ = run.add_argument("--monthly-price", type=float, help="the host's price a month, dollars")
    _ = run.add_argument(
        "--egress-price-per-gb", type=float, help="dollars per GB beyond the allowance"
    )
    _ = run.add_argument("--included-egress-tb", type=float, help="egress allowance, TB a month")
    _ = run.add_argument("--port-mbps", type=float, help="the host's network port, Mbit/s")
    compare_command = commands.add_parser("compare", help="rank calibration reports by cost")
    _ = compare_command.add_argument("reports", nargs="+")
    return root


def stop(signal_number: int, _frame: FrameType | None) -> NoReturn:
    """Exit through the normal path, so running steps remove their containers."""
    raise SystemExit(128 + signal_number)


def main(argv: Sequence[str] | None = None) -> int:
    """Entry point."""
    _ = signal.signal(signal.SIGTERM, stop)
    options = parser().parse_args(argv, namespace=Options())
    try:
        return calibrate(options) if options.command == "run" else compare(options.reports)
    except CapacityError as error:
        print(f"capacity: {error}", file=sys.stderr)  # noqa: T201 -- intentional error output.
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
