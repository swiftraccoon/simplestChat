"""Run unchanged VPS controller/worker policy through an owned inert host adapter.

The adapter never starts subprocesses, SSH, Ansible, systemd, Docker or an engine.
It supplies those external observations while actual reference functions own the
request, unit, verdict, recovery, archive and evidence transitions. It is not
authentication, physical lease/process settlement or host-runtime evidence.
"""

from __future__ import annotations

import argparse
import io
import json
import os
import subprocess
import sys
import tempfile
import uuid
from contextlib import ExitStack, contextmanager, redirect_stdout
from pathlib import Path
from typing import TYPE_CHECKING, cast, final
from unittest.mock import patch

from test_support import ROOT

# isort: split
import bounded_process
import release_build as build
import vps_capacity_controller as controller
import vps_capacity_remote as remote

if TYPE_CHECKING:
    from collections.abc import Callable, Generator, Sequence
    from typing import BinaryIO

SCENARIOS = (
    "success",
    "quality-failure",
    "lost-start-ack",
    "recover-clean",
    "recover-unknown",
    "missing-outcome",
    "unit-gc",
    "build-failure",
)
REVISION = "a" * 40
RUN = "b" * 32
INNER_RUN = "c" * 16
SERVER = "sha256:" + "d" * 64
GENERATOR = "sha256:" + "e" * 64
OWNED = "1" * 64
UNKNOWN = "2" * 64
UNIT = "simplestchat-capacity-" + RUN + ".service"
MAX_OUTPUT = 8 * 1024**2


def save(path: Path, value: dict[str, object]) -> None:
    """Use the unchanged reference's private atomic record writer."""
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    remote.save(path, value)


@final
class Child:
    """An inert external capacity child; no PID is ever signalled by this adapter."""

    def __init__(self) -> None:
        """Keep actual in-memory pipe peers for the worker's finalizers."""
        self.returncode: int | None = None
        self.pid = 400001
        self.stdout = io.BytesIO()
        self.stderr = io.BytesIO()


@final
class Host:
    """Supply deterministic external observations to actual unmodified policy calls."""

    def __init__(self, base: Path, scenario: str) -> None:
        """Create only private owned fixture source, inventory and evidence files."""
        self.base = base
        self.scenario = scenario
        self.local = base / "checkout"
        self.root = base / "host"
        self.state = base / "state"
        self.source = self.root / "sources" / REVISION
        self.evidence = self.root / "results" / ("capacity-controller." + RUN)
        self.fixed = base / "fixed-host-files"
        self.actions: list[str] = []
        self.destructive: list[list[str]] = []
        self.capacity_arguments: list[str] = []
        self.systemd_properties: list[str] = []
        self.status_requests: list[dict[str, object]] = []
        self.containers: list[str] = []
        self.lease = False
        self.started = False
        self.stopped = False
        self.child: Child | None = None
        self.unit = self.unit_fields("not-found", "inactive", "dead", "0")
        self.helper = (ROOT / "build/vps_capacity_remote.py").read_text()
        self.process_helper = (ROOT / "ops/ansible/files/bounded_process.py").read_text()
        self.fixed.mkdir(mode=0o700)
        for directory in (
            self.local,
            self.root,
            self.state,
            self.root / "sources",
            self.root / "artifacts",
            self.root / "results",
            self.source,
        ):
            directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        for checkout in (self.local, self.source):
            (checkout / "build").mkdir(mode=0o700)
            (checkout / "ops/ansible/files").mkdir(mode=0o700, parents=True)
            _ = (checkout / "build/vps_capacity_remote.py").write_text(self.helper)
            _ = (checkout / "ops/ansible/files/bounded_process.py").write_text(self.process_helper)
        template = "[Service]\nType=oneshot\n"
        (self.source / "ops/ansible/templates").mkdir(mode=0o700)
        _ = (self.source / "ops/ansible/templates/image-build.service.j2").write_text(template)
        _ = (self.fixed / remote.BUILD_UNIT).write_text(template)
        _ = (self.fixed / "build-images").write_text(
            f"revision='{REVISION}'\nsource_root='{self.source}'\n"
        )
        (self.fixed / "build-images").chmod(0o755)
        self.images: dict[str, object] = {
            "schemaVersion": 1,
            "passed": True,
            "revision": REVISION,
            "serverImage": SERVER,
            "generatorImage": GENERATOR,
        }
        save(self.root / "artifacts" / REVISION / "images.json", self.images)
        private = base / "private"
        private.mkdir(mode=0o700)
        for name in ("identity", "known_hosts"):
            _ = (private / name).write_bytes(b"inert transport fixture\n")
            (private / name).chmod(0o600)
        inventory: dict[str, object] = {
            "benchmark_hosts": {
                "hosts": {
                    "fixture_vps": {
                        "ansible_host": "fixture.invalid",
                        "ansible_user": "debian",
                        "ansible_ssh_private_key_file": str(private / "identity"),
                        "scbench_ssh_known_hosts_file": str(private / "known_hosts"),
                        "scpub_enabled": False,
                    }
                }
            }
        }
        self.inventory = self.local / "inventory.yml"
        save(self.inventory, inventory)

    @staticmethod
    def unit_fields(load: str, active: str, sub: str, pid: str) -> dict[str, object]:
        """Return only fixture systemctl properties, never a successful policy receipt."""
        return {
            "LoadState": load,
            "ActiveState": active,
            "SubState": sub,
            "MainPID": pid,
            "Result": "success",
            "ExecMainCode": "exited",
            "ExecMainStatus": "0",
        }

    def options(self, recovery: str | None = None) -> controller.Options:
        """Call the real public option parser with the bounded thirty-speaker workload."""
        args = [
            "--inventory",
            str(self.inventory),
            "--limit",
            "fixture_vps",
            "--revision",
            REVISION,
            "--audio-only",
            "--first-size",
            "30",
            "--meeting-size",
            "30",
            "--speakers",
            "30",
            "--chat-interval-ms",
            "30000",
            "--runtime-seconds",
            "120" if recovery is None else "7200",
            "--server-cpus",
            "1",
            "--generator-cpus",
            "4.8",
            "--port-mbps",
            "2000",
        ]
        args.extend(["--prepare", "--build-images"] if recovery is None else ["--" + recovery, RUN])
        return controller.options(args)

    def path(self, value: str) -> Path:
        """Map the three fixed absolute fixture files; no user path is evaluated."""
        mapping = {
            "/etc/simplestchat-public/compose.public.yml": self.fixed / "absent-public.yml",
            "/usr/local/libexec/simplestchat-bench/build-images": self.fixed / "build-images",
            "/etc/systemd/system": self.fixed,
        }
        if value not in mapping:
            message = "unexpected fixed fixture path"
            raise AssertionError(message)
        return mapping[value]

    def local_command(
        self, _runner: build.Runner, argv: Sequence[str], **_kwargs: object
    ) -> tuple[int, str]:
        """Supply fixed Git/Ansible observations, keeping real validation functions in path."""
        if list(argv) == ["git", "rev-parse", "--verify", "HEAD"]:
            return 0, REVISION
        if list(argv) == ["git", "status", "--porcelain=v1", "--untracked-files=all"]:
            return 0, ""
        if list(argv[:4]) == ["git", "check-ignore", "--quiet", "--"]:
            return 0, ""
        if Path(argv[0]).name == "ansible-playbook":
            remote.require(self.lease and "source,benchmark" in argv, "fixture_lease_not_held")
            self.actions.append("ansible")
            return 0, ""
        message = "unexpected local fixture command"
        raise AssertionError(message)

    def command(self, argv: list[str], *, timeout: int = 30, allow_failure: bool = False) -> str:
        """Supply only exact fixed host-tool observations; unknown commands fail the test."""
        remote.require(timeout > 0 and type(allow_failure) is bool, "fixture_command_contract")
        if argv[:3] == ["/usr/bin/git", "-C", str(self.source)]:
            if argv[3:] == ["rev-parse", "HEAD"]:
                return REVISION
            if argv[3:] == ["status", "--porcelain=v1", "--untracked-files=all"]:
                return ""
        if argv[0] == "/usr/bin/systemctl":
            return self.systemctl(argv)
        if argv[0] == "/usr/bin/systemd-run":
            self.systemd_properties = [arg for arg in argv if arg.startswith("--property=")]
            return self.start_unit()
        if argv[:2] == ["/usr/bin/journalctl", "--unit"]:
            return "inert unit journal"
        if argv[: len(remote.DOCKER)] == remote.DOCKER:
            return self.docker(argv)
        message = "unexpected host fixture command"
        raise AssertionError(message)

    def systemctl(self, argv: list[str]) -> str:
        """Supply fixed systemd observations; real collection decides stop/GC ordering."""
        if argv[:2] == ["/usr/bin/systemctl", "show"]:
            unit = argv[2]
            state = (
                self.unit if unit == UNIT else self.unit_fields("loaded", "inactive", "dead", "0")
            )
            if unit == remote.BUILD_UNIT and self.scenario == "build-failure":
                state = dict(state, Result="exit-code", ExecMainStatus="1")
            return "\n".join(key + "=" + str(value) for key, value in state.items())
        if argv[:2] == ["/usr/bin/systemctl", "list-units"]:
            return (
                UNIT + " loaded active running Fixture" if self.started and not self.stopped else ""
            )
        if argv == ["/usr/bin/systemctl", "start", remote.BUILD_UNIT]:
            return ""
        if argv == ["/usr/bin/systemctl", "stop", UNIT]:
            remote.require((self.evidence / "unit-result.json").is_file(), "fixture_unsaved_unit")
            remote.require((self.evidence / "collection.json").is_file(), "fixture_unsaved_collect")
            self.stopped = True
            self.unit = self.unit_fields("not-found", "inactive", "dead", "0")
            return ""
        message = "unexpected systemctl fixture command"
        raise AssertionError(message)

    def start_unit(self) -> str:
        """Run the real reference worker inline at the inert systemd boundary."""
        self.started = True
        self.unit = self.unit_fields("loaded", "active", "running", "77")
        status = remote.run_worker(remote.read_json(self.evidence / "request.json"))
        self.unit = self.unit_fields("loaded", "active", "exited", "0")
        self.unit.update(
            Result="success" if status == 0 else "exit-code", ExecMainStatus=str(status)
        )
        if self.scenario == "missing-outcome":
            (self.evidence / "outcome.json").unlink()
            save(
                self.state / "current.json",
                {"schemaVersion": 1, "run": RUN, "finalized": False},
            )
            self.unit.update(Result="timeout", ExecMainStatus="137")
        return ""

    def docker(self, argv: list[str]) -> str:
        """Supply exact image/container observations for unchanged ownership policy."""
        if argv[: len(remote.DOCKER) + 1] == [*remote.DOCKER, "ps"]:
            if "--filter" in argv:
                label = argv[argv.index("--filter") + 1]
                return (
                    OWNED
                    if label == "label=simplestchat.capacity.run=" + INNER_RUN
                    and OWNED in self.containers
                    else ""
                )
            return "\n".join(self.containers) if "--all" in argv else ""
        if argv[: len(remote.DOCKER) + 2] == [*remote.DOCKER, "image", "inspect"]:
            return json.dumps(
                [
                    {
                        "Id": argv[-1],
                        "Config": {
                            "User": "10001:10001",
                            "Labels": {"org.opencontainers.image.revision": REVISION},
                        },
                    }
                ]
            )
        if argv[: len(remote.DOCKER) + 1] == [*remote.DOCKER, "inspect"]:
            identity = argv[-1]
            remote.require(identity in self.containers, "fixture_unknown_inspection")
            return json.dumps(
                [
                    {
                        "Id": identity,
                        "Name": "/capacity-gen-fixture" if identity == OWNED else "/foreign",
                        "Image": GENERATOR,
                        "Config": {
                            "Image": GENERATOR,
                            "Env": ["PRIVATE_DO_NOT_PROJECT"],
                            "Labels": {"simplestchat.capacity.run": INNER_RUN}
                            if identity == OWNED
                            else {},
                        },
                        "State": {
                            "Status": "exited",
                            "Running": False,
                            "ExitCode": 0,
                            "OOMKilled": False,
                            "StartedAt": "inert",
                            "FinishedAt": "inert",
                        },
                    }
                ]
            )
        if argv[: len(remote.DOCKER) + 1] in ([*remote.DOCKER, "stop"], [*remote.DOCKER, "rm"]):
            identity = argv[-1]
            remote.require(
                identity == OWNED and identity in self.containers, "fixture_unowned_cleanup"
            )
            self.destructive.append(argv[len(remote.DOCKER) :])
            if argv[len(remote.DOCKER)] == "rm":
                self.containers.remove(identity)
            return identity
        message = "unexpected docker fixture command"
        raise AssertionError(message)

    def popen(self, argv: Sequence[str], **_kwargs: object) -> Child:
        """Record real policy-generated engine argv without creating a process."""
        remote.require(
            list(argv[:3]) == ["/usr/bin/python3", "-B", str(self.source / "build/capacity.py")],
            "fixture_unexpected_child",
        )
        self.capacity_arguments = list(argv[3:])
        self.child = Child()
        return self.child

    def stop(self, child: Child, *, grace: float = 5) -> None:
        """Keep unexpected fake-child cleanup from making any real signal syscall."""
        remote.require(child is self.child and grace > 0, "fixture_unowned_child")
        child.returncode = -15

    def pump(
        self, child: Child, output: BinaryIO, _error: BinaryIO, _limits: bounded_process.Limits
    ) -> int:
        """Supply external calibration/ownership observations to actual worker assessment."""
        request = remote.read_json(self.evidence / "request.json")
        good = self.scenario != "quality-failure"
        report: dict[str, object] = {
            "schemaVersion": 1,
            "measurement": {
                "meetingSize": request["meetingSize"],
                "browser": {
                    key: request[key] for key in ("audioOnly", "speakers", "chatIntervalMs")
                },
            },
            "images": {
                role: {"id": image, "revision": REVISION}
                for role, image in (("server", SERVER), ("generator", GENERATOR))
            },
            "steps": [
                {
                    "workload": request["workload"],
                    "size": request["firstSize"],
                    "valid": True,
                    "passed": good,
                    "privateObservation": float("inf"),
                }
            ],
            "ceilings": {"meetings": {"size": 30, "lowerBound": True}},
            "projection": {"lowerBound": True},
        }
        save(self.evidence / "workload/calibration.json", report)
        containers: list[dict[str, object]] = []
        if self.scenario in ("recover-clean", "recover-unknown"):
            self.containers = [OWNED] + ([UNKNOWN] if self.scenario == "recover-unknown" else [])
            containers.append({"name": "capacity-gen-fixture", "image": GENERATOR, "id": OWNED})
        save(
            self.evidence / "workload/ownership.json",
            {"runId": INNER_RUN, "containers": containers},
        )
        remote.require(output.write(b"inert external capacity child\n") > 0, "fixture_log_write")
        child.returncode = 0
        return 0

    def call(
        self, _transport: controller.Transport, request: dict[str, object], *, timeout: int = 60
    ) -> dict[str, object]:
        """Dispatch through actual serve/start/worker/collect policy, not supplied success JSON."""
        remote.require(timeout > 0, "fixture_timeout")
        action = remote.text(request["action"])
        self.actions.append(action)
        if action == "status":
            self.status_requests.append(
                {
                    "unit": "build" if request.get("build") is True else "capacity",
                    "runtimeSeconds": request["runtimeSeconds"],
                }
            )
        output = io.StringIO()
        with redirect_stdout(output):
            remote.serve(request, self.helper, self.process_helper)
        if action == "start" and self.scenario in (
            "lost-start-ack",
            "recover-clean",
            "recover-unknown",
        ):
            message = "fixture_lost_start_ack"
            raise remote.CapacityControlError(message)
        return remote.obj(cast("object", json.loads(output.getvalue())))

    @contextmanager
    def reserve(
        self, _transport: controller.Transport, _request: dict[str, object]
    ) -> Generator[Callable[[], bool]]:
        """Keep the actual canonical flock/preflight across inert Ansible observation."""
        self.actions.append("reserve")
        with remote.workload_lock():
            _ = remote.preflight()
            self.lease = True
            try:
                yield lambda: self.lease
            finally:
                self.lease = False
                self.actions.append("release")

    def download(
        self, _transport: controller.Transport, request: dict[str, object], destination: Path
    ) -> None:
        """Use the actual bounded remote tar/gzip producer and actual local extractor."""
        self.actions.append("archive")
        output = io.TextIOWrapper(io.BytesIO(), encoding="utf-8")
        with redirect_stdout(output):
            remote.stream_archive(request)
        _ = output.flush()
        with destination.open("xb") as target:
            _ = target.write(output.buffer.getvalue())
        output.close()

    @contextmanager
    def installed(self) -> Generator[None]:
        """Adapt only fixed process/root observations; leave reference policy unchanged."""
        protected = remote.protected
        lstat = Path.lstat

        def metadata(path: Path) -> os.stat_result:
            info = lstat(path)
            fields = list(info)
            fields[4:6] = [0, 0]
            return os.stat_result(fields)

        def root_protected(path: Path, *, directory: bool = False, private: bool = True) -> None:
            with patch.object(Path, "lstat", autospec=True, side_effect=metadata):
                protected(path, directory=directory, private=private)

        with ExitStack() as stack:
            for name, value in (("ROOT", self.root), ("STATE", self.state)):
                _ = stack.enter_context(patch.object(remote, name, value))
            _ = stack.enter_context(patch("vps_capacity_remote.Path", side_effect=self.path))
            _ = stack.enter_context(patch.object(remote, "protected", side_effect=root_protected))
            _ = stack.enter_context(patch.object(remote, "command", side_effect=self.command))
            _ = stack.enter_context(patch.object(os, "geteuid", return_value=0))
            _ = stack.enter_context(patch.object(os, "kill", return_value=None))
            _ = stack.enter_context(
                patch.object(build.Runner, "run", autospec=True, side_effect=self.local_command)
            )
            _ = stack.enter_context(
                patch.object(controller.Transport, "call", autospec=True, side_effect=self.call)
            )
            _ = stack.enter_context(
                patch.object(
                    controller.Transport, "reserve", autospec=True, side_effect=self.reserve
                )
            )
            _ = stack.enter_context(
                patch.object(
                    controller.Transport, "download", autospec=True, side_effect=self.download
                )
            )
            _ = stack.enter_context(
                patch("vps_capacity_remote.subprocess.Popen", side_effect=self.popen)
            )
            _ = stack.enter_context(patch.object(bounded_process, "pump", side_effect=self.pump))
            _ = stack.enter_context(patch.object(bounded_process, "stop", side_effect=self.stop))
            _ = stack.enter_context(
                patch.object(
                    subprocess,
                    "run",
                    side_effect=AssertionError("unexpected real subprocess boundary"),
                )
            )
            _ = stack.enter_context(patch.object(uuid, "uuid4", return_value=uuid.UUID(hex=RUN)))
            yield

    def observe(self, report: dict[str, object]) -> dict[str, object]:
        """Project stable policy facts; private extended-number records stay JSON strings."""
        result = remote.obj(report.get("result", {}))
        outcome = remote.obj(result.get("outcome", {}))
        cleanup = remote.obj(result.get("cleanup", {}))
        recovery = remote.obj(report.get("recovery", {}))
        workload = dict(remote.obj(report["request"]))
        legacy_action = workload.pop("action", None)
        remote.require(legacy_action in (None, "start"), "fixture_legacy_action")
        # The semantic projection above removes only the old transport's action
        # selector. Keep actual saved bytes too; these are legacy evidence data,
        # not the new protocol's original W or an acquisition capability.
        request_file = self.evidence / "request.json"
        outcome_file = self.evidence / "outcome.json"
        return {
            "passed": report["passed"],
            "remoteOutcome": report["remoteOutcome"],
            "failureClass": report.get("failureClass"),
            "request": workload,
            "legacyTransportAction": legacy_action,
            "legacySavedRequestJson": request_file.read_text() if request_file.exists() else None,
            "legacySavedOutcomeJson": outcome_file.read_text() if outcome_file.exists() else None,
            "unit": report.get("unit"),
            "outcomePassed": outcome.get("passed"),
            "outcomeFinalized": outcome.get("finalized"),
            "cleanupClean": cleanup.get("clean"),
            "recoveryRemoved": recovery.get("removed"),
            "recoveryFinalized": recovery.get("finalized"),
            "privateOutcomeJson": json.dumps(outcome, sort_keys=True, separators=(",", ":")),
        }


def run_scenario(scenario: str) -> dict[str, object]:
    """Return deterministic actual-reference policy observations for one finite scenario."""
    if scenario not in SCENARIOS:
        message = "unknown oracle scenario"
        raise ValueError(message)
    with tempfile.TemporaryDirectory(prefix="vps-policy-oracle-") as directory:
        host = Host(Path(directory).resolve(), scenario)
        with host.installed():
            first = controller.execute(host.options(), host.local)
            runs = [host.observe(first)]
            if scenario in ("recover-clean", "recover-unknown", "unit-gc"):
                # A new output parent keeps the reference's fresh-evidence check real.
                _ = (host.local / "results").rename(host.local / "first-results")
                second = controller.execute(
                    host.options("collect" if scenario == "unit-gc" else "recover"), host.local
                )
                runs.append(host.observe(second))
        current = (
            remote.read_json(host.state / "current.json")
            if (host.state / "current.json").exists()
            else None
        )
        observation = {
            "actions": host.actions,
            "runs": runs,
            "destructive": host.destructive,
            "capacityArguments": host.capacity_arguments,
            "systemdProperties": host.systemd_properties,
            "legacyStatusRequests": host.status_requests,
            "childPipesClosed": host.child.stdout.closed and host.child.stderr.closed
            if host.child is not None
            else None,
            "current": current,
            "containers": host.containers,
            "innerRun": remote.read_json(host.evidence / "workload/ownership.json").get("runId")
            if (host.evidence / "workload/ownership.json").exists()
            else None,
            "retainedFiles": sorted(
                str(path.relative_to(host.evidence))
                for path in host.evidence.rglob("*")
                if path.is_file()
            ),
        }
        # Only fixed fixture roots are normalized, never arbitrary payload strings.
        text = json.dumps(observation, sort_keys=True, separators=(",", ":"), allow_nan=False)
        text = text.replace(str(host.root), "<host>").replace(str(host.local), "<checkout>")
        return {
            "schemaVersion": 1,
            "scenario": scenario,
            "observation": remote.obj(cast("object", json.loads(text))),
        }


def main(arguments: Sequence[str] | None = None) -> int:
    """Emit one bounded deterministic JSON result; all scenarios are inert."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("--scenario", required=True, choices=SCENARIOS)
    args = parser.parse_args(arguments)
    scenario = cast("str", args.scenario)
    output = json.dumps(run_scenario(scenario), sort_keys=True, separators=(",", ":")) + "\n"
    if len(output.encode()) > MAX_OUTPUT:
        message = "oracle output bound"
        raise ValueError(message)
    _ = sys.stdout.write(output)
    return 0


if __name__ == "__main__":
    sys.exit(main())
