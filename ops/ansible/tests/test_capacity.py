"""Offline calibration logic: parsers, plans, judging, search and cost; no containers."""

from __future__ import annotations

import io
import json
import math
import os
import tempfile
import unittest
from contextlib import redirect_stdout
from dataclasses import dataclass, field, replace
from pathlib import Path
from typing import override

from test_support import ROOT

# isort: split

import capacity

CPU_STAT_WITH_QUOTA = """usage_usec 5000000
user_usec 4000000
system_usec 1000000
nr_periods 100
nr_throttled 5
throttled_usec 1000
"""
PROC_STAT = """cpu  100 5 50 1000 20 0 3 7 0 0
cpu0 50 2 25 500 10 0 1 3 0 0
cpu1 50 3 25 500 10 0 2 4 0 0
intr 12345
"""
SNMP = """Ip: Forwarding DefaultTTL
Ip: 1 64
Udp: InDatagrams NoPorts InErrors OutDatagrams RcvbufErrors SndbufErrors
Udp: 1000 0 12 900 12 0
"""
# The header row is skipped: only numbered socket rows count.
UDP_HEADER = (
    "   sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid"
    + "  timeout inode ref pointer drops\n"
)
UDP_TABLE = (
    UDP_HEADER
    + """  1: 00000000:9C40 00000000:0000 07 00000000:00000000 00:00000000 00000000 10001 0 1 2 0 7
  2: 00000000:9C41 00000000:0000 07 00000000:00000000 00:00000000 00000000 10001 0 2 2 0 3
  3: 0100007F:C1A7 00000000:0000 07 00000000:00000000 00:00000000 00000000 10001 0 3 2 0 100
"""
)


def step(**changes: object) -> capacity.StepResult:
    """Return a healthy meetings step with explicit changes."""
    result = capacity.StepResult("meetings", 20, 20, 4, "2026-09-26T00:00:00+00:00")
    result.generator_passed = True
    result.validated_consumers = 160
    result.worker_load_peak = {0: 0.40, 1: 0.35}
    result.worker_load_mean = {0: 0.30, 1: 0.28}
    result.sent_datagrams = 100_000
    result.namespace_datagrams = 1_000_000
    result.memory_limit_bytes = 1_000
    result.memory_peak_bytes = 500
    result.load_scrapes = 5
    for key, value in changes.items():
        setattr(result, key, value)
    return result


def trial(size: int, *, passed: bool, load: float = 0.3, generator: float = 0.0) -> capacity.Trial:
    """Return a valid trial."""
    return capacity.Trial(
        size,
        passed=passed,
        valid=True,
        busiest_worker=load,
        mean_worker=load,
        generator_cores=generator,
    )


MEETINGS = capacity.Search("meetings", 5, 20, 5, 6, 5)
LARGE = capacity.Search("large-meeting", 5, 8, 1, 6, 2)
LIMITS = capacity.Limits()


class ParserTests(unittest.TestCase):
    """Kernel, cgroup and metrics text parse into the figures the judge uses."""

    def test_cgroup_cpu_stat_and_throttling_share(self) -> None:
        """Quota counters parse; a delta of periods gives the throttled share."""
        before = capacity.parse_cpu_stat(CPU_STAT_WITH_QUOTA)
        self.assertEqual(before, capacity.CpuStat(5_000_000, 100, 5))
        after = capacity.CpuStat(6_000_000, 200, 25)
        self.assertEqual(capacity.throttled_share(before, after), 0.2)
        unlimited = capacity.parse_cpu_stat("usage_usec 10\nuser_usec 5\n")
        self.assertEqual((unlimited.periods, unlimited.throttled), (0, 0))
        self.assertEqual(capacity.throttled_share(unlimited, unlimited), 0.0)
        with self.assertRaises(capacity.CapacityError):
            _ = capacity.parse_cpu_stat("nr_periods 3\n")

    def test_proc_stat_counts_idle_iowait_steal_and_cpus(self) -> None:
        """The aggregate line gives total, idle (with iowait) and steal ticks."""
        times = capacity.parse_proc_stat(PROC_STAT)
        self.assertEqual(times, capacity.HostTimes(1185, 1020, 7))
        self.assertEqual(capacity.count_cpus(PROC_STAT), 2)
        with self.assertRaises(capacity.CapacityError):
            _ = capacity.parse_proc_stat("intr 1\n")

    def test_host_descriptions(self) -> None:
        """CPU model names come from x86 or ARM fields; memory from MemTotal."""
        self.assertEqual(
            capacity.parse_cpu_model("processor\t: 0\nmodel name\t: AMD EPYC 7R13 Processor\n"),
            "AMD EPYC 7R13 Processor",
        )
        self.assertEqual(capacity.parse_cpu_model("Hardware\t: BCM2835\n"), "BCM2835")
        self.assertEqual(capacity.parse_cpu_model("flags : fpu\n"), "unknown")

    def test_arm_cores_are_named_from_their_codes(self) -> None:
        """ARM reports implementer and part codes; the cores clouds rent get names."""
        graviton2 = (
            "processor\t: 0\nCPU implementer\t: 0x41\nCPU architecture: 8\nCPU part\t: 0xd0c\n"
        )
        self.assertEqual(capacity.parse_cpu_model(graviton2), "ARM Neoverse-N1")
        apple_vm = "CPU implementer\t: 0x61\nCPU part\t: 0x000\n"
        self.assertEqual(capacity.parse_cpu_model(apple_vm), "Apple CPU part 0x000")
        unknown = "CPU implementer\t: 0x99\nCPU part\t: 0x123\n"
        self.assertEqual(capacity.parse_cpu_model(unknown), "CPU implementer 0x99 part 0x123")

    def test_platform_names_the_machine(self) -> None:
        """DMI vendor and product: a cloud's instance type on some, empty where absent."""
        self.assertEqual(capacity.parse_platform("Amazon EC2\nm7g.large\n"), "Amazon EC2 m7g.large")
        self.assertEqual(capacity.parse_platform("\n"), "")
        self.assertEqual(capacity.parse_meminfo_kib("MemTotal:       16384000 kB\n"), 16_384_000)

    def test_udp_counters_sum_only_worker_sockets(self) -> None:
        """Namespace totals parse, and only the workers' ports count as socket drops."""
        counters = capacity.parse_udp(SNMP, UDP_TABLE, [40_000, 40_001])
        self.assertEqual(counters, capacity.UdpCounters(1000, 12, 10))
        with self.assertRaises(capacity.CapacityError):
            _ = capacity.parse_udp("Ip: 1\n", UDP_TABLE, [40_000])

    def test_prometheus_and_worker_loads(self) -> None:
        """Samples parse by full name; worker gauges become a load per worker."""
        metrics = capacity.parse_prometheus(
            "# HELP x y\n"
            + 'simplestchat_media_worker_cpu{worker="0"} 0.5\n'
            + 'simplestchat_media_worker_cpu{worker="1"} 0.25\n'
            + "simplestchat_joins_refused_saturated_total 3\n"
            + "not a sample\n"
        )
        self.assertEqual(metrics["simplestchat_joins_refused_saturated_total"], 3.0)
        self.assertEqual(capacity.worker_loads(metrics), {0: 0.5, 1: 0.25})


class PlanTests(unittest.TestCase):
    """Each workload becomes the generator command a browser workload needs."""

    def test_meetings_spread_rooms_of_the_meeting_size(self) -> None:
        """Twenty participants in meetings of five: four rooms, half a join a second per worker."""
        plan = capacity.StepPlan("meetings", 20, 5, 30, 60)
        self.assertEqual((plan.clients, plan.rooms, plan.ramp_seconds), (20, 4, 40))
        args = plan.generator_args(capacity.Browser(), "label", ("server", "generator"))
        options = dict(zip(args[::2], args[1::2], strict=True))
        self.assertEqual(options["--profile"], "browser")
        self.assertEqual(options["--rooms"], "4")
        self.assertEqual(options["--publish-ratio"], "1.000000000000")
        self.assertEqual(options["--source-addresses"], str(capacity.SOURCE_ADDRESSES))
        self.assertEqual(options["--layout"], "classic")
        self.assertEqual(options["--pixel-ratio"], "2")

    def test_a_webinar_has_exactly_one_presenter(self) -> None:
        """The audience plus one presenter, whatever the audience size."""
        for audience in (25, 100, 999, 1500):
            plan = capacity.StepPlan("webinar", audience, 5, 30, 60)
            args = plan.generator_args(capacity.Browser(), "label", ("s", "g"))
            ratio = float(dict(zip(args[::2], args[1::2], strict=True))["--publish-ratio"])
            self.assertEqual(plan.clients, audience + 1)
            self.assertEqual(math.ceil(plan.clients * ratio), 1)
        self.assertEqual(capacity.StepPlan("webinar", 100, 5, 30, 60).ramp_seconds, 26)
        # Meetings spread their handshakes over the workers; one room does not.
        self.assertEqual(capacity.StepPlan("meetings", 600, 5, 30, 60, 2).ramp_seconds, 600)
        self.assertEqual(capacity.StepPlan("large-meeting", 60, 5, 30, 60, 2).ramp_seconds, 40)
        self.assertEqual(capacity.StepPlan("large-meeting", 8, 5, 30, 60).ramp_seconds, 10)

    def test_forwarded_streams_follow_browser_subscriptions(self) -> None:
        """Browsers consume every peer's audio and video, up to 64 consumers each."""
        self.assertEqual(capacity.forwarded_streams("meetings", 20, 5), 160.0)
        self.assertEqual(capacity.forwarded_streams("large-meeting", 10, 5), 180.0)
        self.assertEqual(capacity.forwarded_streams("large-meeting", 40, 5), 2560.0)
        self.assertEqual(capacity.forwarded_streams("webinar", 100, 5), 200.0)


class JudgeTests(unittest.TestCase):
    """A step passes only if every participant was served within every limit."""

    def test_a_healthy_step_passes(self) -> None:
        """No failure, no reason."""
        self.assertEqual(
            capacity.judge(step(), LIMITS), capacity.Verdict(passed=True, valid=True, reasons=())
        )

    def test_each_limit_fails_the_step_with_its_reason(self) -> None:
        """Every limit names itself when broken."""
        cases = {
            "worker": step(worker_load_peak={0: 0.72}),
            "dropped": step(socket_drops=200),
            "lost media": step(failed_consumers=3),
            "refused": step(failed_connections=2, refused_joins=2),
            "memory": step(memory_peak_bytes=900),
            "server exited": step(server_exit="oom"),
            "own checks": step(generator_passed=False, generator_errors=4),
        }
        for words, result in cases.items():
            verdict = capacity.judge(result, LIMITS)
            self.assertFalse(verdict.passed, words)
            self.assertTrue(verdict.valid, words)
            self.assertIn(words, " ".join(verdict.reasons))

    def test_a_throttled_generator_or_an_error_makes_a_step_invalid(self) -> None:
        """Then the generator, not the server, set the pace."""
        throttled = capacity.judge(step(generator_throttled=0.2), LIMITS)
        self.assertEqual((throttled.passed, throttled.valid), (False, False))
        self.assertIn("generator was throttled", throttled.reasons[0])
        broken = capacity.judge(step(error="server did not start"), LIMITS)
        self.assertEqual(
            broken, capacity.Verdict(passed=False, valid=False, reasons=("server did not start",))
        )

    def test_a_generator_out_of_memory_makes_a_step_invalid(self) -> None:
        """Its clients died with it; the server was not what stopped them."""
        with tempfile.TemporaryDirectory() as directory:
            result = step(generator_out_of_memory=True, generator_passed=False)
            capacity.conclude(
                result, (None, None, None), Path(directory) / "load_test_summary.json"
            )
        verdict = capacity.judge(result, LIMITS)
        self.assertEqual((verdict.passed, verdict.valid), (False, False))
        self.assertIn("generator ran out of memory", verdict.reasons[0])
        passed = capacity.Verdict(passed=True, valid=True, reasons=())
        ceiling = capacity.ceiling_of(
            "meetings", [(step(), passed), (result, verdict)], 2.0, workers=2
        )
        self.assertEqual((ceiling.ceiling, ceiling.bound), (20, "generator-limited"))

    def test_video_starved_at_the_viewer_cap_is_named(self) -> None:
        """More tiles than a viewer's 3 Mbit/s carries is not a CPU limit."""
        room = {"workload": "large-meeting", "size": 58, "clients": 58, "rooms": 1}
        starved = step(**room, failed_consumers=296, failed_video_consumers=296, viewers_at_cap=58)
        (reason,) = capacity.judge(starved, LIMITS).reasons
        self.assertIn("296 video consumers lost media", reason)
        self.assertIn("3 Mbit/s", reason)
        mixed = step(**room, failed_consumers=4, failed_video_consumers=2, viewers_at_cap=58)
        self.assertEqual(capacity.judge(mixed, LIMITS).reasons, ("4 consumers lost media",))

    def test_the_cap_is_not_blamed_where_tiles_fit_under_it(self) -> None:
        """Estimates sit at the cap on any healthy network; four tiles never fill it."""
        meeting = step(
            size=240,
            clients=240,
            rooms=48,
            failed_consumers=1,
            failed_video_consumers=1,
            viewers_at_cap=196,
        )
        self.assertEqual(capacity.judge(meeting, LIMITS).reasons, ("1 consumers lost media",))

    def test_an_exhausted_or_saturated_server_fails_the_step(self) -> None:
        """Quota throttling stalls every worker at once, whatever each worker's load."""
        throttled = capacity.judge(step(server_throttled=0.3), LIMITS)
        self.assertFalse(throttled.passed)
        self.assertIn("CPU quota ran out in 30% of periods", throttled.reasons[0])
        saturated = capacity.judge(step(server_saturated_scrapes=1), LIMITS)
        self.assertIn("reported itself CPU-saturated", saturated.reasons[0])

    def test_a_generator_throttled_while_clients_joined_makes_a_step_invalid(self) -> None:
        """Clients it lost during setup would read as the server's failure."""
        verdict = capacity.judge(step(generator_setup_throttled=0.2, failed_connections=3), LIMITS)
        self.assertFalse(verdict.valid)
        self.assertIn("while clients joined", verdict.reasons[0])

    def test_derived_figures(self) -> None:
        """Drop share, busiest worker, mean load and wire egress."""
        result = step(
            socket_drops=50,
            received_packets=1000,
            received_payload_bytes=938_000,
            measurement_seconds=8.0,
        )
        self.assertEqual(result.drop_share, 0.0005)
        self.assertEqual(result.busiest_worker, 0.40)
        self.assertAlmostEqual(result.mean_worker, 0.29)
        # (938,000 + 1,000 * 62) bytes * 8 / 8 s = 1 Mbit/s.
        self.assertAlmostEqual(result.egress_mbps, 1.0)


def generator_summary(*, passed: bool, failures: list[str]) -> dict[str, object]:
    """Return a complete generator summary as `load_test_summary.json` holds it."""
    return {
        "run": {"completed": True, "passed": passed, "failureReasons": failures},
        "totalErrors": 0,
        "failedConnections": 0,
        "failedConsumers": 0,
        "validatedConsumers": 160,
        "totalPacketsSent": 90_000,
        "totalDatagramsSent": 95_000,
        "measurement": {"durationMs": 60_000, "packetsReceived": 1000, "bytesReceived": 938_000},
        "receiveMediaReady": {"p99Ms": 812},
        "videoStart": {"p99Ms": 1450},
    }


class SummaryTests(unittest.TestCase):
    """The generator's summary fills a step's delivery figures and its failure reasons."""

    def test_a_complete_summary(self) -> None:
        """Delivery, the measurement window and readiness latency are copied."""
        result = capacity.StepResult("meetings", 20, 20, 4, "")
        capacity.read_summary(result, generator_summary(passed=True, failures=[]))
        self.assertTrue(result.generator_passed)
        self.assertEqual(result.validated_consumers, 160)
        self.assertEqual((result.received_packets, result.measurement_seconds), (1000, 60.0))
        self.assertEqual(result.receive_ready_p99_ms, 812.0)
        self.assertEqual(result.video_start_p99_ms, 1450.0)
        self.assertEqual(result.sent_datagrams, 95_000)
        self.assertEqual(capacity.judge(result, LIMITS).reasons, ())

    def test_a_deadline_marker_fails_the_step_with_the_generator_reason(self) -> None:
        """The hard-deadline summary has no measurement; the server was too slow to serve."""
        result = step()
        marker_reason = "Hard deadline exceeded; detailed metrics unavailable"
        marker = {
            "run": {
                "completed": False,
                "passed": False,
                "failureReasons": [marker_reason],
            }
        }
        capacity.read_summary(result, marker)
        verdict = capacity.judge(result, LIMITS)
        self.assertEqual((verdict.passed, verdict.valid), (False, True))
        (reason,) = verdict.reasons
        self.assertEqual(reason, "the generator's checks failed: " + marker_reason)

    def test_many_generator_failures_are_counted_not_listed(self) -> None:
        """A report names the first failure and counts the rest."""
        failures = [
            f"client-{index}: expected 8 validated consumers, observed 3" for index in range(40)
        ]
        result = step()
        capacity.read_summary(result, generator_summary(passed=False, failures=failures))
        self.assertLessEqual(len(result.generator_failures), 5)
        (reason,) = capacity.judge(result, LIMITS).reasons
        self.assertIn("client-0: expected 8", reason)
        self.assertIn("(and 39 more)", reason)


CGROUP_FILES = {
    "/sys/fs/cgroup/cpu.stat": CPU_STAT_WITH_QUOTA,
    "/proc/stat": PROC_STAT,
    "/sys/fs/cgroup/memory.current": "500\n",
}


@dataclass(frozen=True, slots=True)
class FakeEngine(capacity.Engine):
    """Containers as files; a stopped container refuses reads, as podman does."""

    files: dict[str, str] = field(default_factory=dict[str, str])
    stopped: frozenset[str] = frozenset()

    @override
    def read(self, container: str, path: str) -> str:
        if container in self.stopped:
            message = "podman exec failed: container state improper"
            raise capacity.CapacityError(message)
        if path not in self.files:
            message = f"podman exec failed: cat: {path}: No such file or directory"
            raise capacity.CapacityError(message)
        return self.files[path]

    @override
    def running(self, container: str) -> bool:
        return container not in self.stopped


def watch(engine: capacity.Engine) -> capacity.Watch:
    """Return a watch over two workers' ten-second window starting at zero."""
    return capacity.Watch(engine, "sfu", "gen", 0, "token", (0.0, 10.0), step(), workers=2)


class HostWarningTests(unittest.TestCase):
    """Conditions that would make a host's figures low are said before measuring."""

    def test_clamped_buffers_and_a_busy_host_are_named(self) -> None:
        """The sysctl to run, and how busy the host already was."""
        host: dict[str, object] = {"idleBusyShare": 0.3, "logicalCpus": 8, "rmemMax": 212_992}
        warnings = capacity.host_warnings(host)
        self.assertEqual(len(warnings), 2)
        self.assertIn("30% busy", warnings[0])
        self.assertIn("sysctl -w net.core.rmem_max=2097152", warnings[1])
        quiet: dict[str, object] = {"idleBusyShare": 0.01, "logicalCpus": 8, "rmemMax": 2**21}
        self.assertEqual(capacity.host_warnings(quiet), [])


class MonitorTests(unittest.TestCase):
    """The window ends before the generator does, and stopped containers are not errors."""

    def test_the_cpu_window_closes_before_the_generator_exits(self) -> None:
        """The generator's clock starts first and it exits as its own window closes."""
        plan = capacity.StepPlan("meetings", 40, 5, 20, 30)
        start, end = capacity.step_window(100.0, plan)
        self.assertEqual(start, 100.0 + plan.ramp_seconds + 20)
        self.assertEqual(end, start + 30 - capacity.WINDOW_END_GUARD_SECONDS)

    def test_marks_are_read_while_both_containers_run(self) -> None:
        """Setup, start and end marks come from the cgroup and host counters."""
        observed = watch(FakeEngine("fake", files=CGROUP_FILES))
        observed.marks(-5.0)
        observed.marks(0.0)
        observed.marks(10.0)
        self.assertIsNotNone(observed.setup_mark)
        self.assertIsNotNone(observed.start_mark)
        self.assertIsNotNone(observed.end_mark)

    def test_a_stopped_container_is_left_to_the_liveness_check(self) -> None:
        """No exception: the liveness check reports which container stopped."""
        engine = FakeEngine("fake", files=CGROUP_FILES, stopped=frozenset({"gen"}))
        observed = watch(engine)
        observed.marks(-5.0)
        observed.marks(0.0)
        observed.memory(0.0)
        self.assertIsNone(observed.start_mark)
        self.assertFalse(observed.alive(0.0))

    def test_scrapes_count_only_with_every_worker_and_record_saturation(self) -> None:
        """A scrape missing a worker's gauge says nothing about that worker."""
        observed = watch(FakeEngine("fake", files=CGROUP_FILES))
        worker = 'simplestchat_media_worker_cpu{worker="%d"}'
        observed.record_scrape({worker % 0: 0.4, worker % 1: 0.3})
        observed.record_scrape({worker % 0: 0.5})
        observed.record_scrape(
            {worker % 0: 0.2, worker % 1: 0.6, "simplestchat_cpu_saturated": 1.0}
        )
        observed.finish()
        self.assertEqual(observed.result.load_scrapes, 2)
        self.assertEqual(observed.result.worker_load_peak, {0: 0.4, 1: 0.6})
        self.assertEqual(observed.result.server_saturated_scrapes, 1)

    def test_an_unreadable_counter_while_both_run_is_an_error(self) -> None:
        """Otherwise the step would lose its CPU figures without saying so."""
        observed = watch(FakeEngine("fake"))
        with self.assertRaises(capacity.CapacityError):
            observed.marks(0.0)


def mark(at: float, server_usec: int, generator: capacity.CpuStat) -> capacity.Mark:
    """Return CPU readings at one edge of a window."""
    return capacity.Mark(
        at, capacity.CpuStat(server_usec, 100, 0), generator, capacity.HostTimes(1000, 400, 10)
    )


class ConclusionTests(unittest.TestCase):
    """A finished step takes its CPU figures from the marks and its delivery from the summary."""

    def write_summary(self, directory: str, *, passed: bool) -> Path:
        """Write a generator summary into a step directory."""
        path = Path(directory) / "load_test_summary.json"
        _ = path.write_text(json.dumps(generator_summary(passed=passed, failures=[])))
        return path

    def test_marks_give_cores_and_throttling(self) -> None:
        """Two seconds of server CPU per second, and a generator throttled in 10% of periods."""
        start = capacity.Mark(
            0.0,
            capacity.CpuStat(0, 0, 0),
            capacity.CpuStat(0, 0, 0),
            capacity.HostTimes(0, 0, 0),
        )
        end = mark(10.0, 20_000_000, capacity.CpuStat(5_000_000, 100, 10))
        with tempfile.TemporaryDirectory() as directory:
            result = capacity.StepResult("meetings", 20, 20, 4, "")
            result.load_scrapes = 5
            setup = capacity.Mark(
                -5.0,
                capacity.CpuStat(0, 0, 0),
                capacity.CpuStat(0, 0, 0),
                capacity.HostTimes(0, 0, 0),
            )
            capacity.conclude(
                result, (setup, start, end), self.write_summary(directory, passed=True)
            )
        self.assertEqual((result.server_cores, result.generator_cores), (2.0, 0.5))
        self.assertEqual(result.generator_throttled, 0.1)
        self.assertEqual((result.host_steal, result.host_busy), (0.01, 0.6))
        self.assertTrue(result.generator_passed)
        self.assertEqual(result.error, "")

    def test_a_step_without_worker_loads_is_invalid(self) -> None:
        """The workers' loads are the primary signal; a step that never read them proves nothing."""
        end = mark(10.0, 20_000_000, capacity.CpuStat(5_000_000, 100, 0))
        with tempfile.TemporaryDirectory() as directory:
            result = step(load_scrapes=1)
            capacity.conclude(result, (end, end, end), self.write_summary(directory, passed=True))
        self.assertIn("per-worker loads", result.error)
        self.assertFalse(capacity.judge(result, LIMITS).valid)

    def test_a_pass_without_cpu_readings_is_invalid(self) -> None:
        """Without the window's marks nobody knows whether the generator kept pace."""
        with tempfile.TemporaryDirectory() as directory:
            result = step(generator_passed=False)
            capacity.conclude(
                result, (None, None, None), self.write_summary(directory, passed=True)
            )
        verdict = capacity.judge(result, LIMITS)
        self.assertFalse(verdict.valid)
        self.assertIn("throttling is unknown", verdict.reasons[0])

    def test_a_failure_without_cpu_readings_is_invalid_unless_the_server_exited(self) -> None:
        """A throttled generator can fail a step by itself; a crashed server cannot hide."""
        with tempfile.TemporaryDirectory() as directory:
            failing = step()
            capacity.conclude(
                failing, (None, None, None), self.write_summary(directory, passed=False)
            )
            self.assertFalse(capacity.judge(failing, LIMITS).valid)
            crashed = step(server_exit="the server stopped during the run")
            capacity.conclude(
                crashed, (None, None, None), self.write_summary(directory, passed=False)
            )
            verdict = capacity.judge(crashed, LIMITS)
        self.assertEqual((verdict.passed, verdict.valid), (False, True))

    def test_client_results_count_starved_video_and_viewers_at_the_cap(self) -> None:
        """Per-client results say which consumers failed and where estimates sat."""
        clients: list[dict[str, object]] = [
            {
                "lastAvailableBitrate": 3_000_000,
                "consumerDelivery": [
                    {"isAudio": False, "passed": False},
                    {"isAudio": True, "passed": True},
                ],
            },
            {
                "lastAvailableBitrate": 1_200_000,
                "consumerDelivery": [{"isAudio": True, "passed": False}],
            },
            {"consumerDelivery": []},
        ]
        with tempfile.TemporaryDirectory() as directory:
            summary = self.write_summary(directory, passed=False)
            _ = (Path(directory) / "load_test_results.json").write_text(json.dumps(clients))
            result = step()
            capacity.conclude(result, (None, None, None), summary)
        self.assertEqual((result.failed_video_consumers, result.viewers_at_cap), (1, 1))

    def test_a_missing_or_broken_summary_is_an_error(self) -> None:
        """The generator crashed, or its summary was cut short."""
        with tempfile.TemporaryDirectory() as directory:
            missing = step()
            capacity.conclude(missing, (None, None, None), Path(directory) / "absent.json")
            self.assertEqual(missing.error, "the generator wrote no summary")
            broken = Path(directory) / "load_test_summary.json"
            _ = broken.write_text('{"run": ')
            truncated = step()
            capacity.conclude(truncated, (None, None, None), broken)
            self.assertIn("unusable", truncated.error)


class DropShareTests(unittest.TestCase):
    """Worker drops are a share of every datagram the clients sent the workers."""

    def test_the_clients_own_receive_traffic_does_not_dilute_drops(self) -> None:
        """The shared namespace also counts every packet the clients receive."""
        result = step(socket_drops=200, sent_datagrams=100_000, namespace_datagrams=10_000_000)
        self.assertEqual(result.drop_share, 0.002)
        self.assertIn("dropped", capacity.judge(result, LIMITS).reasons[0])

    def test_drops_without_a_sent_count_fail(self) -> None:
        """A summary without totals cannot excuse drops."""
        self.assertEqual(step(socket_drops=5, sent_datagrams=0).drop_share, 1.0)
        self.assertEqual(step(socket_drops=0, sent_datagrams=0).drop_share, 0.0)

    def test_a_generator_without_the_datagram_count_is_named(self) -> None:
        """Counting RTP alone made a webinar's feedback drops exceed 100 %."""
        summary = generator_summary(passed=True, failures=[])
        del summary["totalDatagramsSent"]
        with self.assertRaisesRegex(capacity.CapacityError, "rebuild the load-test image"):
            capacity.read_summary(step(), summary)


class SearchTests(unittest.TestCase):
    """The search projects upward, bisects, and stops once the ceiling is bracketed."""

    def test_the_first_size_is_the_probe(self) -> None:
        """No trials: try the first size."""
        self.assertEqual(capacity.next_size(MEETINGS, [], LIMITS, 2.0), 20)

    def test_a_light_pass_projects_toward_the_guard(self) -> None:
        """Spread load is projected from the mean worker plus imbalance, in meeting steps."""
        # 0.7 guard x 0.95 margin / (0.1 mean x 1.15 imbalance) = 5.78 times the 160
        # streams at 20: 925 streams, 115 participants (eight streams each).
        light = [trial(20, passed=True, load=0.1)]
        self.assertEqual(capacity.next_size(MEETINGS, light, LIMITS, 2.0), 115)

    def test_projection_grows_at_least_fifteen_percent(self) -> None:
        """A pass close to the guard still moves up."""
        self.assertEqual(
            capacity.next_size(LARGE, [trial(20, passed=True, load=0.69)], LIMITS, 2.0), 23
        )

    def test_the_generator_quota_caps_the_projection(self) -> None:
        """At 0.5 generator cores for 20, a 2-core quota (1.7 usable) allows 65, not 115."""
        light = [trial(20, passed=True, load=0.1, generator=0.5)]
        self.assertEqual(capacity.next_size(MEETINGS, light, LIMITS, 2.0), 65)
        # A generator already near its quota cannot grow: 20 again is a repeat, so stop.
        saturated = [trial(20, passed=True, load=0.1, generator=1.8)]
        self.assertIsNone(capacity.next_size(MEETINGS, saturated, LIMITS, 2.0))

    def test_bisection_and_stopping(self) -> None:
        """Between a pass and a fail, try the middle; stop within ten percent."""
        bracket = [trial(20, passed=True), trial(40, passed=False)]
        self.assertEqual(capacity.next_size(MEETINGS, bracket, LIMITS, 2.0), 30)
        tight = [trial(20, passed=True), trial(22, passed=False)]
        self.assertIsNone(capacity.next_size(LARGE, tight, LIMITS, 2.0))

    def test_failures_shrink_until_the_minimum(self) -> None:
        """Only failures: 60 percent of the smallest failure, never below the minimum."""
        self.assertEqual(capacity.next_size(MEETINGS, [trial(40, passed=False)], LIMITS, 2.0), 20)
        self.assertIsNone(capacity.next_size(MEETINGS, [trial(5, passed=False)], LIMITS, 2.0))

    def test_an_invalid_step_or_the_step_budget_stops_the_search(self) -> None:
        """A throttled generator measured itself, not the server; six steps is the budget."""
        invalid = [
            capacity.Trial(
                20,
                passed=False,
                valid=False,
                busiest_worker=0.3,
                mean_worker=0.3,
                generator_cores=1.9,
            )
        ]
        self.assertIsNone(capacity.next_size(MEETINGS, invalid, LIMITS, 2.0))
        budget = [trial(size, passed=True) for size in (10, 20, 30, 40, 50, 60)]
        self.assertIsNone(capacity.next_size(MEETINGS, budget, LIMITS, 2.0))


def projected(participants: int, mbps: float) -> capacity.Projection:
    """Return a CPU-bound projection of `participants` at `mbps` each."""
    return capacity.Projection(
        per_core=0.0,
        cpu_participants=participants,
        participants=participants,
        limited_by="cpu",
        memory_limit_mib=0,
        lower_bound=False,
        mbps_per_participant=mbps,
    )


class CeilingAndCostTests(unittest.TestCase):
    """Ceilings summarize trials; the cost model prices participant-hours."""

    def test_ceiling_bounds(self) -> None:
        """Measured only when a valid failure brackets it; otherwise a lower bound and why."""
        passed = capacity.Verdict(passed=True, valid=True, reasons=())
        failed = capacity.Verdict(passed=False, valid=True, reasons=("worker",))
        invalid = capacity.Verdict(passed=False, valid=False, reasons=("generator",))

        def bound(
            *trials: tuple[capacity.StepResult, capacity.Verdict],
        ) -> tuple[int | None, capacity.Bound]:
            ceiling = capacity.ceiling_of("meetings", trials, 2.0, workers=2)
            return ceiling.ceiling, ceiling.bound

        self.assertEqual(capacity.ceiling_of("webinar", [], 2.0, workers=2).bound, "not-run")
        self.assertEqual(bound((step(), failed)), (None, "below-first-size"))
        self.assertEqual(bound((step(error="no summary"), invalid)), (None, "interrupted"))
        self.assertEqual(bound((step(), passed), (step(size=40), failed)), (20, "measured"))
        # The failure just above a measured ceiling says what bounds it.
        measured = capacity.ceiling_of(
            "meetings", [(step(), passed), (step(size=40), failed)], 2.0, workers=2
        )
        self.assertEqual(measured.limit, "worker")
        # Bisection can end on an invalid step beneath a bracketing failure.
        throttled = step(size=30, generator_throttled=0.2)
        self.assertEqual(
            bound((step(), passed), (step(size=40), failed), (throttled, invalid)),
            (20, "measured"),
        )
        self.assertEqual(
            bound((step(), passed), (step(size=40, generator_throttled=0.2), invalid)),
            (20, "generator-limited"),
        )
        self.assertEqual(
            bound((step(), passed), (step(size=40, error="podman exec failed"), invalid)),
            (20, "interrupted"),
        )
        # Passing throughout: no room for the generator's next step, or no steps left.
        self.assertEqual(bound((step(generator_cores=1.6), passed)), (20, "generator-limited"))
        self.assertEqual(bound((step(generator_cores=0.5), passed)), (20, "step-limited"))
        self.assertEqual(
            capacity.ceiling_of("meetings", [(step(), passed)], 2.0, workers=2).workers, 2
        )

    def test_lower_bounds_read_as_at_least(self) -> None:
        """A ceiling without an upper bracket says so, and why."""
        describe = capacity.describe_ceiling
        self.assertEqual(describe({"ceiling": 80, "bound": "measured"}), "80")
        self.assertEqual(
            describe({"ceiling": 600, "bound": "generator-limited"}),
            "at least 600 (the generator ran out of CPU first)",
        )
        self.assertEqual(
            describe({"ceiling": 600, "bound": "step-limited"}),
            "at least 600 (the search ran out of steps)",
        )
        self.assertEqual(
            describe({"ceiling": 20, "bound": "interrupted"}),
            "at least 20 (a later step could not run; see its step.json)",
        )
        self.assertEqual(
            describe({"ceiling": None, "bound": "interrupted"}),
            "not measured (a step could not run; see its step.json)",
        )

    def test_each_workload_gets_a_shape_the_generator_can_saturate(self) -> None:
        """Per-core workloads give the server a quarter of the host; one room needs two workers."""
        shape = capacity.shape_for
        self.assertEqual(shape("meetings", 8, deployment_workers=7), capacity.Shape(2.0, 2, 5.8))
        self.assertEqual(shape("meetings", 4, deployment_workers=3), capacity.Shape(1.0, 1, 2.8))
        self.assertEqual(shape("webinar", 16, deployment_workers=15), capacity.Shape(4.0, 4, 11.8))
        self.assertEqual(
            shape("large-meeting", 4, deployment_workers=3), capacity.Shape(2.0, 2, 1.8)
        )
        self.assertEqual(
            shape("large-meeting", 2, deployment_workers=1), capacity.Shape(1.0, 1, 0.8)
        )
        self.assertEqual(shape("meetings", 1, deployment_workers=1), capacity.Shape(1.0, 1, 0.5))
        # One room is measured with no more workers than the deployment will run.
        self.assertEqual(
            shape("large-meeting", 4, deployment_workers=1), capacity.Shape(1.0, 1, 2.8)
        )
        # An operator's quotas win over the defaults.
        self.assertEqual(
            shape("meetings", 8, deployment_workers=7, server_cpus=3.0, generator_cpus=4.5),
            capacity.Shape(3.0, 3, 4.5),
        )

    def test_the_deployment_leaves_the_host_its_reserves(self) -> None:
        """All but one CPU and a quarter of the memory (at least 1 GiB) go to the app."""
        deployment = capacity.deployment_for(8, 8192, app_cpus=None, app_memory_mib=None)
        self.assertEqual((deployment.app_cpus, deployment.workers), (7.0, 7))
        self.assertEqual(deployment.memory_mib, 6144)
        small = capacity.deployment_for(2, 2048, app_cpus=None, app_memory_mib=None)
        self.assertEqual((small.app_cpus, small.memory_mib), (1.0, 1024))
        chosen = capacity.deployment_for(
            8, 8192, app_cpus=2.5, app_memory_mib=3000, worker_threshold=0.85
        )
        self.assertEqual(
            (chosen.workers, chosen.memory_mib, chosen.worker_threshold), (2, 3000, 0.85)
        )

    def test_cost_is_cpu_or_network_bound(self) -> None:
        """Monthly price over participant-hours at full use, plus egress beyond the allowance."""
        cpu = capacity.cost_of(projected(200, 1.0), capacity.Prices(monthly=14.6))
        self.assertEqual((cpu.participants, cpu.limited_by), (200, "cpu"))
        # $14.60 / (200 * 730 hours) * 1000.
        self.assertAlmostEqual(cpu.dollars_per_1000_participant_hours, 0.1)
        self.assertAlmostEqual(cpu.egress_gb_per_participant_hour, 0.45)
        network = capacity.cost_of(
            projected(200, 1.0), capacity.Prices(monthly=14.6, port_mbps=100)
        )
        self.assertEqual((network.participants, network.limited_by), (80, "network"))
        billed = capacity.cost_of(
            projected(10, 1.0), capacity.Prices(monthly=7.3, egress_per_gb=0.01)
        )
        # 10 * 730 hours * 0.45 GB * $0.01 = $32.85 of egress on top of $7.30.
        self.assertAlmostEqual(billed.dollars_per_1000_participant_hours, 40.15 / 7.3)

    def test_recommendations(self) -> None:
        """Workers from the app's CPUs, 90% room limits, scaled memory and the sysctls."""
        ceilings: dict[capacity.Workload, capacity.Ceiling] = {
            "meetings": capacity.Ceiling(
                "meetings", 100, "measured", 0.6, 80.0, 400 * capacity.MIB, workers=2
            ),
            "large-meeting": capacity.Ceiling("large-meeting", 30, "measured", 0.65, 0.0, 0),
            "webinar": capacity.Ceiling("webinar", None, "not-run", 0.0, 0.0, 0),
        }
        deployment = capacity.Deployment(app_cpus=4.5, memory_mib=16_384)
        projection = capacity.project(ceilings, deployment)
        # 50 participants per worker on four workers; 4 MiB each, doubled.
        self.assertEqual((projection.participants, projection.limited_by), (200, "cpu"))
        self.assertEqual(projection.memory_limit_mib, 1792)
        self.assertEqual(
            capacity.recommendations(ceilings, projection, deployment, 212_992),
            [
                "SIMPLESTCHAT_CPUS=4.5",
                "MEDIA_WORKERS=4",
                "MAX_PARTICIPANTS_PER_ROOM=27",
                "SIMPLESTCHAT_MEMORY_LIMIT=1792m",
                "host sysctl: net.core.rmem_max=2097152 net.core.wmem_max=2097152 "
                + "(the workers ask for 1 MiB socket buffers)",
            ],
        )
        bare = capacity.Deployment(app_cpus=1.0, memory_mib=1024, worker_threshold=0.85)
        self.assertEqual(
            capacity.recommendations({}, capacity.project({}, bare), bare, 2 * capacity.MIB),
            ["SIMPLESTCHAT_CPUS=1", "MEDIA_WORKERS=1", "CPU_SATURATION_WORKER_UTILIZATION=0.85"],
        )

    def test_a_run_without_meetings_sizes_nothing(self) -> None:
        """Only meetings scale to a deployment; a webinar alone sets no memory limit."""
        ceilings: dict[capacity.Workload, capacity.Ceiling] = {
            "meetings": capacity.Ceiling("meetings", None, "not-run", 0.0, 0.0, 0),
            "webinar": capacity.Ceiling(
                "webinar", 175, "measured", 0.48, 200.0, 180 * capacity.MIB, workers=1
            ),
        }
        deployment = capacity.Deployment(app_cpus=2.0, memory_mib=2048)
        projection = capacity.project(ceilings, deployment)
        self.assertEqual((projection.participants, projection.memory_limit_mib), (0, 0))
        self.assertEqual(
            capacity.recommendations(ceilings, projection, deployment, 2 * capacity.MIB),
            ["SIMPLESTCHAT_CPUS=2", "MEDIA_WORKERS=2"],
        )

    def test_memory_bounds_the_projection(self) -> None:
        """2,000 MiB for 240 on two workers: seven workers on 8 GiB cannot hold 840."""
        ceilings: dict[capacity.Workload, capacity.Ceiling] = {
            "meetings": capacity.Ceiling(
                "meetings", 240, "measured", 0.6, 300.0, 2000 * capacity.MIB, workers=2
            ),
        }
        deployment = capacity.deployment_for(8, 8192, app_cpus=None, app_memory_mib=None)
        projection = capacity.project(ceilings, deployment)
        self.assertEqual(projection.cpu_participants, 840)
        self.assertEqual((projection.participants, projection.limited_by), (368, "memory"))
        self.assertEqual(projection.memory_limit_mib, 6144)
        cost = capacity.cost_of(projection, capacity.Prices(monthly=10.0))
        self.assertEqual((cost.participants, cost.limited_by), (368, "memory"))


def report(
    label: str, monthly: float | None, meetings: int | None, bound: capacity.Bound = "measured"
) -> dict[str, object]:
    """Return a report as `build_report` writes it for a synthetic calibration."""
    options = capacity.Options(label=label, monthly_price=monthly)
    context = capacity.Context(
        engine=capacity.Engine("/nonexistent/docker"),
        run_id="test",
        server_image="server",
        generator_image="generator",
        revisions=("s", "g"),
        shape=capacity.Shape(2.0, 2, 1.8),
        worker_threshold=0.7,
        server_memory="2048m",
        generator_memory="1024m",
        browser=capacity.Browser(),
        output=Path(),
        log=lambda _message: None,
    )
    host: dict[str, object] = {
        "cpuModel": "Test CPU",
        "logicalCpus": 4,
        "memoryMib": 8192,
        "rmemMax": 2 * capacity.MIB,
    }
    measured = capacity.Measured(
        host=host,
        images={},
        context=context,
        limits=capacity.Limits(),
        deployment=capacity.Deployment(app_cpus=3.0, memory_mib=6144),
        shapes=dict.fromkeys(capacity.WORKLOADS, capacity.Shape(2.0, 2, 1.8)),
    )
    passing = step(size=meetings or 0, received_packets=0, measurement_seconds=0.0)
    ceilings: dict[capacity.Workload, capacity.Ceiling] = {
        workload: capacity.Ceiling(workload, None, "not-run", 0.0, 0.0, 0)
        for workload in capacity.WORKLOADS
    }
    ceilings["meetings"] = capacity.Ceiling("meetings", meetings, bound, 0.6, 50.0, 0, workers=2)
    ceilings["large-meeting"] = capacity.Ceiling(
        "large-meeting", 40, "measured", 0.6, 0.0, 0, workers=2
    )
    verdict = capacity.Verdict(passed=True, valid=True, reasons=())
    steps: dict[capacity.Workload, list[tuple[capacity.StepResult, capacity.Verdict]]] = (
        {"meetings": [(passing, verdict)]} if meetings is not None else {}
    )
    return capacity.build_report(options, measured, steps, ceilings)


class ServerCommandTests(unittest.TestCase):
    """The measured server runs with the guard the judge applies."""

    def test_the_worker_threshold_reaches_the_server(self) -> None:
        """Judging at 0.85 while the server refuses at 0.7 would measure the default."""
        context = capacity.Context(
            engine=capacity.Engine("/nonexistent/docker"),
            run_id="test",
            server_image="server",
            generator_image="generator",
            revisions=("s", "g"),
            shape=capacity.Shape(2.0, 2, 1.8),
            worker_threshold=0.85,
            server_memory="2048m",
            generator_memory="1024m",
            browser=capacity.Browser(),
            output=Path(),
            log=print,
        )
        command = capacity.server_command(context, "sfu", 4000, "token")
        self.assertIn("CPU_SATURATION_WORKER_UTILIZATION=0.85", command)
        self.assertIn("MEDIA_WORKERS=2", command)
        # Experiment settings reach the server; the wiring stays the tool's.
        extra = replace(context, server_env=(("MEDIA_KEYFRAME_REQUEST_DELAY_MS", "1000"),))
        self.assertIn(
            "MEDIA_KEYFRAME_REQUEST_DELAY_MS=1000",
            capacity.server_command(extra, "sfu", 4000, "token"),
        )

    def test_server_settings_are_parsed_and_the_wiring_is_protected(self) -> None:
        """KEY=VALUE pairs, upper-case names, and never a key the tool sets itself."""
        self.assertEqual(
            capacity.parse_server_env(["MEDIA_KEYFRAME_REQUEST_DELAY_MS=1000", "RUST_LOG=debug"]),
            (("MEDIA_KEYFRAME_REQUEST_DELAY_MS", "1000"), ("RUST_LOG", "debug")),
        )
        for invalid in ("NOEQUALS", "=1", "lower=1", "MEDIA_WORKERS=4", "METRICS_TOKEN=x"):
            with self.assertRaises(capacity.CapacityError, msg=invalid):
                _ = capacity.parse_server_env([invalid])


class ReportTests(unittest.TestCase):
    """Reports carry the projection and cost; `compare` ranks them by cost."""

    def test_report_projects_per_core_and_prices_it(self) -> None:
        """Participants per core times the app's cores, with the cost of each."""
        built = report("small", 10.0, 100)
        projection = capacity.as_object(built["projection"], "projection")
        self.assertEqual(projection["meetingParticipantsPerCore"], 50.0)
        # A ceiling holds for the join rate it was measured at, so the report keeps it.
        measurement = capacity.as_object(built["measurement"], "measurement")
        rates = capacity.as_object(measurement["joinsPerSecond"], "joinsPerSecond")
        self.assertEqual(rates, {"meetings": 1.0, "large-meeting": 1.5, "webinar": 4.0})
        self.assertEqual(projection["meetingParticipants"], 150)
        self.assertEqual(projection["egressMbpsPerParticipant"], 0.5)
        cost = capacity.as_object(built["cost"], "cost")
        self.assertEqual(cost["limitedBy"], "cpu")
        lines = "\n".join(capacity.summary_lines(built))
        self.assertIn("meetings of 5: 100 participants", lines)
        self.assertIn("largest webinar: not measured", lines)
        self.assertIn("beyond 33 publishers", lines)
        self.assertIn("Cost: $", lines)
        self.assertIsNone(report("unpriced", None, 100)["cost"])

    def test_a_lower_bound_carries_into_the_projection_and_comparison(self) -> None:
        """A generator-limited ceiling projects to at least so many participants."""
        built = report("bounded", 10.0, 100, bound="generator-limited")
        projection = capacity.as_object(built["projection"], "projection")
        self.assertTrue(projection["lowerBound"] is True)
        self.assertIn("at least 150 participants", "\n".join(capacity.summary_lines(built)))
        _, row = capacity.comparison_row(built)
        self.assertIn("≥150", row)
        exact = capacity.as_object(report("exact", 10.0, 100)["projection"], "projection")
        self.assertTrue(exact["lowerBound"] is False)

    def test_a_run_without_meetings_projects_nothing(self) -> None:
        """A webinar-only run says it cannot size the deployment instead of projecting 0."""
        built = report("webinar-only", 10.0, None, bound="not-run")
        self.assertIsNone(built["cost"])
        lines = "\n".join(capacity.summary_lines(built))
        self.assertNotIn("Projected to", lines)
        self.assertIn("No projection: meetings were not measured", lines)
        _, row = capacity.comparison_row(built)
        self.assertNotIn("≥0", row)

    def test_compare_ranks_cheapest_first(self) -> None:
        """The host with the lowest cost per 1,000 participant-hours comes first."""
        with tempfile.TemporaryDirectory() as directory:
            paths: list[str] = []
            for label, monthly, meetings in (("dear", 40.0, 100), ("cheap", 5.0, 100)):
                path = Path(directory) / f"{label}.json"
                _ = path.write_text(json.dumps(report(label, monthly, meetings)), encoding="utf-8")
                paths.append(str(path))
            output = io.StringIO()
            with redirect_stdout(output):
                self.assertEqual(capacity.compare(paths), 0)
        rows = output.getvalue().splitlines()
        self.assertTrue(rows[0].startswith("host"))
        self.assertTrue(rows[1].startswith("cheap"))
        self.assertTrue(rows[2].startswith("dear"))
        # A workload that was not run shows as a dash, not as Python's None.
        self.assertNotIn("None", output.getvalue())
        self.assertEqual(rows[1].split()[-3], "-")


class CommandLineTests(unittest.TestCase):
    """Unset options keep the typed defaults; the script is executable."""

    def test_defaults_survive_the_run_subcommand(self) -> None:
        """A subparser must not overwrite typed defaults with None."""
        options = capacity.parser().parse_args(
            ["run", "--server-image", "s", "--generator-image", "g"], namespace=capacity.Options()
        )
        self.assertEqual(options.command, "run")
        self.assertEqual(options.meeting_size, 5)
        self.assertEqual(options.worker_threshold, 0.7)
        self.assertEqual(options.workloads, list(capacity.WORKLOADS))
        self.assertFalse(options.quick)
        self.assertEqual((options.capture, options.layout), ("720p", "classic"))
        self.assertIsNone(options.monthly_price)
        chosen = capacity.parser().parse_args(
            [
                "run",
                "--server-image",
                "s",
                "--generator-image",
                "g",
                "--workloads",
                "webinar",
                "--quick",
                "--monthly-price",
                "6.8",
            ],
            namespace=capacity.Options(),
        )
        self.assertEqual(
            (chosen.workloads, chosen.quick, chosen.monthly_price), (["webinar"], True, 6.8)
        )

    def test_a_single_size_can_be_repeated(self) -> None:
        """`--first-size` and `--steps` rerun one size with the same instrumentation."""
        options = capacity.parser().parse_args(
            [
                "run",
                "--server-image",
                "s",
                "--generator-image",
                "g",
                "--workloads",
                "webinar",
                "--first-size",
                "545",
                "--steps",
                "1",
            ],
            namespace=capacity.Options(),
        )
        self.assertEqual((options.first_size, options.steps), (545, 1))
        shape = capacity.Shape(2.0, 2, 5.8)
        webinar = capacity.search_for("webinar", shape, 5, 1, first=545)
        self.assertEqual((webinar.first, webinar.max_steps), (545, 1))
        # Meetings run whole rooms, so the size rounds down to the meeting size.
        self.assertEqual(capacity.search_for("meetings", shape, 5, 6, first=53).first, 50)
        self.assertEqual(capacity.search_for("meetings", shape, 5, 6).first, 20)

    def test_compare_takes_report_paths(self) -> None:
        """`compare` collects its positional reports."""
        options = capacity.parser().parse_args(
            ["compare", "a.json", "b.json"], namespace=capacity.Options()
        )
        self.assertEqual(options.reports, ["a.json", "b.json"])

    def test_the_script_is_executable(self) -> None:
        """Operators run it directly on a host."""
        self.assertTrue(os.access(ROOT / "build" / "capacity.py", os.X_OK))


if __name__ == "__main__":
    _ = unittest.main()
