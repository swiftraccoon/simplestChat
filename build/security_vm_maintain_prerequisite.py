"""Own one fixed signed-musl complete launcher prerequisite in a fresh marked guest.

Reuse the existing artifact, distribution and VM owners. The operational launcher
remains unchanged; source-built GNU is only a passive ELF inspector.
"""

from __future__ import annotations

import argparse
import hashlib
import math
import os
import shlex
import signal
import socket
import sys
import time
import tomllib
import uuid
from pathlib import Path
from typing import TYPE_CHECKING, cast, final, override

import caddy_probe_guest as caddy
import security_vm as vm
import security_vm_fetch as fetch
import security_vm_maintain_prerequisite_guest as guest
import security_vm_ops as ops
import security_vm_ops_guest as ops_guest
import security_vm_receive as receive
import security_vm_stage as stage_vm
import yaml
from release_json import json_value, object_value, string_value
from security_context import ROOT, executable
from security_tools import ToolError, require, write_private

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping
    from typing import BinaryIO, NoReturn


WORKFLOW = "Signed musl maintenance launcher prerequisite KVM runtime"
SECONDS = 1800
FINALIZATION = 120
SETUP = 510
NATIVE = 1050
OBSERVATION = 120
DEBT = NATIVE + OBSERVATION
MAX_RECORD = 131072
FILE_MODE = 0o600
NEW_PATHS = (
    ".github/workflows/maintenance-launcher-runtime.yml",
    "build/security_vm_maintain_prerequisite.py",
    "build/security_vm_maintain_prerequisite_guest.py",
    "ops/ansible/security-vm-maintain-prerequisite.yml",
    "ops/ansible/tests/test_security_vm_maintain_prerequisite.py",
    "docs/maintenance-launcher-runtime-proof.md",
)
HOST_FILES = (
    "ops/ansible/group_vars/benchmark_hosts.yml",
    "ops/ansible/tasks/docker.yml",
    "ops/ansible/tasks/runtime-binaries.yml",
    "ops/ansible/tasks/runtime-verify.yml",
    "ops/ansible/templates/docker.sources.j2",
    "ops/ansible/templates/docker.preferences.j2",
    "ops/ansible/templates/containerd-runtime.conf.j2",
    "ops/ansible/requirements.txt",
)
SOURCE_INPUTS = tuple(
    dict.fromkeys((*receive.SOURCE_INPUTS, *NEW_PATHS, *sorted(guest.GUEST_SOURCES), *HOST_FILES))
)


@final
class Control(fetch.Control):
    """Keep the original fixture clock through setup, one native call and finalizers."""

    setup = True

    @property
    @override
    def end(self) -> float:
        """Keep the original1800 end; never borrow a service/helper deadline."""
        return self.started + SECONDS

    @property
    def whole_body_end(self) -> float:
        """Retain the original120-second VM/input finalization reserve."""
        return self.end - FINALIZATION

    @property
    @override
    def body_end(self) -> float:
        """Apply the setup ceiling until its last live admission fence."""
        return min(self.started + SETUP, self.whole_body_end) if self.setup else self.whole_body_end

    def advance(self) -> None:
        """Require a full1050 envelope and120 observation seconds BEFORE widening."""
        self.check()
        require(self.setup, "maintain_vm_phase")
        require(self.whole_body_end - time.monotonic() >= DEBT, "maintain_vm_native_debt")
        self.check()
        self.setup = False


@final
class Options(receive.Options):
    """Allow only exact signed Ops selectors and one closed fixture case."""

    scenario: str = ""


def admit(environment: Mapping[str, str]) -> None:
    """Confine all effects to this exact optional free main workflow."""
    require(
        sys.platform == "linux"
        and environment.get("GITHUB_ACTIONS") == "true"
        and environment.get("RUNNER_ENVIRONMENT") == "github-hosted"
        and environment.get("GITHUB_REPOSITORY") == fetch.REPOSITORY
        and environment.get("GITHUB_REF") == "refs/heads/main"
        and environment.get("GITHUB_WORKFLOW") == WORKFLOW,
        "maintain_vm_disposable_ci",
    )


def clipped(control: Control, cap: int) -> float:
    """Keep every setup role inside the live original phase."""
    control.check()
    remaining = min(cap, control.body_end - time.monotonic())
    require(remaining > 0, "maintain_vm_deadline")
    return remaining


def ssh(host: receive.Host, work: Path, port: int) -> list[str]:
    """Use only the retained original SSH executable and fixed guest helper."""
    tool = host.fixed_tools["ssh"]
    tool.check(host.control)
    return [
        str(tool.path),
        *vm.ssh_options(work),
        "-i",
        str(work / "client"),
        "-p",
        str(port),
        "fixture@127.0.0.1",
        shlex.join(
            [
                "/usr/bin/sudo",
                "-n",
                "/usr/bin/python3",
                "-I",
                "-B",
                "/root/simplestchat-vm-fixture/security_vm_maintain_prerequisite_guest.py",
            ]
        ),
    ]


def package_version(source: fetch.Held, control: Control) -> str:
    """Parse Cargo metadata from its retained original rather than reopening it."""
    source.check(control)
    data = source.stream.read(2**20 + 1)
    require(len(data) == source.size <= 2**20, "maintain_vm_source_manifest")
    value = cast("object", tomllib.loads(data.decode())["workspace"]["package"]["version"])
    require(isinstance(value, str), "maintain_vm_source_manifest")
    source.check(control)
    return cast("str", value)


def fixture_config(original: fetch.Held, control: Control) -> tuple[str, str]:
    """Read the two pinned choices with the existing mature YAML/byte adapters."""
    original.check(control)
    data = original.stream.read(2**20 + 1)
    require(len(data) == original.size <= 2**20, "maintain_vm_config_bound")
    value = object_value(json_value(cast("object", yaml.safe_load(data))))
    image = string_value(value["scpub_postgres_image"])
    package = string_value(value["scbench_docker_engine_version"])
    require("@sha256:" in image and ":" in package and "-" in package, "maintain_vm_config_profile")
    version = package.split(":", 1)[1].split("-", 1)[0]
    original.check(control)
    return image, version


def source_bytes(original: fetch.Held, control: Control) -> bytes:
    """Read bounded chunks from the retained source FD under the same original control."""
    original.check(control)
    _ = original.stream.seek(0)
    chunks: list[bytes] = []
    total = 0
    while True:
        control.check()
        chunk = original.stream.read(min(65536, 2**20 + 1 - total))
        control.check()
        if not chunk:
            break
        total += len(chunk)
        require(total <= 2**20, "maintain_vm_source_bound")
        chunks.append(chunk)
    require(total == original.size, "maintain_vm_source_bound")
    original.check(control)
    return b"".join(chunks)


def source_bundle(
    host: receive.Host,
    git: fetch.Held,
    revision: str,
    sources: Mapping[str, fetch.Held],
    owners: fetch.Owners,
) -> tuple[fetch.Held, str]:
    """Run only the fixed clean-HEAD bundle roles with the retained original Git executable."""
    path = host.output / "maintain-source.bundle"
    tree = ""
    for boundary in ("before", "after"):
        for role, arguments in (
            ("head", ["rev-parse", "HEAD"]),
            ("tree", ["rev-parse", "HEAD^{tree}"]),
            ("clean", ["status", "--porcelain", "--untracked-files=all"]),
        ):
            git.check(host.control)
            _, data = host.run(
                "source-" + role + "-" + boundary,
                [str(git.path), "-C", str(ROOT), *arguments],
                timeout=clipped(cast("Control", host.control), 30),
            )
            git.check(host.control)
            if role == "head":
                require(data == (revision + "\n").encode(), "maintain_vm_source_head")
            elif role == "tree":
                value = guest.digest(data.decode("ascii").removesuffix("\n"), 40)
                require(data == (value + "\n").encode(), "maintain_vm_source_tree")
                require(not tree or tree == value, "maintain_vm_source_tree")
                tree = value
            else:
                require(not data, "maintain_vm_source_clean")
        for original in sources.values():
            original.check(host.control)
        if boundary == "before":
            require(not path.exists() and not path.is_symlink(), "maintain_vm_source_bundle")
            git.check(host.control)
            _ = host.run(
                "source-bundle",
                [str(git.path), "-C", str(ROOT), "bundle", "create", str(path), "HEAD"],
                timeout=clipped(cast("Control", host.control), 120),
            )
            git.check(host.control)
    control = cast("Control", host.control)
    held = fetch.Held.open(path, guest.MAX_BUNDLE, control)
    owners.inputs.append(held)
    require(
        held.stamp[2] & 0o7777 == FILE_MODE and os.fstat(held.stream.fileno()).st_nlink == 1,
        "maintain_vm_source_bundle",
    )
    held.check(control)
    control.check()
    return held, tree


def timing(report: dict[str, object], name: str, control: Control) -> None:
    """Record closed relative checkpoints, never treat timing as work authority."""
    require(
        name
        in {
            "authentication",
            "boot",
            "install",
            "source-bundle",
            "server-authentication",
            "fixture-setup",
            "guest-setup",
            "ready",
            "native",
            "body-end",
        },
        "maintain_vm_timing_role",
    )
    elapsed = time.monotonic() - control.started
    marks = cast("dict[str, object]", report["elapsedSeconds"])
    marks[name] = round(elapsed, 3) if math.isfinite(elapsed) and 0 <= elapsed <= SECONDS else None


def failure_kind(error: BaseException | None) -> str | None:
    """Expose only a closed first-cause category, not raw private exception text."""
    if error is None:
        return None
    if isinstance(error, OSError):
        return "io"
    if type(error) is ToolError:
        arguments = cast("tuple[object, ...]", error.args)
        if len(arguments) == 1 and type(arguments[0]) is str:
            code = arguments[0]
            if code in {"fetch_vm_deadline", "maintain_vm_deadline"}:
                return "deadline"
            if code == "fetch_vm_cancelled":
                return "signal"
    return "refused"


def run(  # noqa: C901, PLR0912, PLR0915 -- Ordered owners.
    options: Options,
    control: Control,
) -> dict[str, object]:
    """Keep original authenticated bytes and all peers until the final signed VM result."""
    report: dict[str, object] = {
        "schemaVersion": 1,
        "scope": "SignedMuslMaintenanceLauncherPrerequisiteKvm",
        "scenario": options.scenario,
        "passed": False,
        "failure": "guard",
        "checkpoint": "guard",
        "cleanupPassed": False,
        "failureKind": None,
        "elapsedSeconds": {},
        "opsVerifierCalls": 0,
        "opsCases": [],
        "maintainProof": None,
        "serverVerifierCalls": 0,
        "serverCurrentMetadataApiCalls": 0,
        "authenticatedServerDistribution": False,
        "authenticatedOpsDistribution": False,
        "nativeTarget": "x86_64-unknown-linux-musl",
        "controllerBinaryUse": "source-built-gnu-passive-ELF-inspection-only",
        "guestClientWholeGroupIndependentlyObserved": False,
    }
    owners = fetch.Owners()
    tools: fetch.NativeTools | None = None
    tree: receive.OpsTree | None = None
    server_tree: stage_vm.ServerTree | None = None
    archive: ops.Input | None = None
    loan: BinaryIO | None = None
    session: receive.Session | None = None
    host: receive.Host | None = None
    first: BaseException | None = None
    try:
        admit(os.environ)
        require(options.scenario in guest.CASES, "maintain_vm_case")
        output = options.output
        require(
            output.is_absolute() and output.resolve() == output and not output.exists(),
            "maintain_vm_refused",
        )
        require(os.environ.get("GH_TOKEN"), "maintain_vm_token")
        revision = guest.digest(os.environ.get("GITHUB_SHA"), 40)
        vm.preflight(output)
        output.mkdir(mode=0o700)
        host = receive.Host(output, control)
        sources: dict[str, fetch.Held] = {}
        for name in SOURCE_INPUTS:
            original = fetch.Held.open(ROOT / name, 2**20, control)
            owners.inputs.append(original)
            sources[name] = original
        report["sourceSha256"] = {name: value.sha256 for name, value in sources.items()}
        programs: list[Path] = []
        for path in (
            ROOT / "target/x86_64-unknown-linux-gnu/release/simplestchat-ops",
            ROOT / "target/debug/xtask",
        ):
            original = fetch.Held.open(path.resolve(), fetch.MAX_BINARY, control)
            owners.inputs.append(original)
            programs.append(original.path)
        binary, controller = programs
        for name in fetch.TOOL_ROLES:
            original = fetch.Held.open(
                Path(executable(name)).resolve(), fetch.MAX_BINARY, control, system=True
            )
            owners.inputs.append(original)
            host.fixed_tools[name] = original
        host.env["ANSIBLE_SSH_EXECUTABLE"] = str(host.fixed_tools["ssh"].path)
        git = fetch.Held.open(
            Path(executable("git")).resolve(), fetch.MAX_BINARY, control, system=True
        )
        owners.inputs.append(git)
        owners.work = fetch.Work.create(output)
        work = owners.work.path
        tools = fetch.NativeTools.create(owners.work, host.fixed_tools, control)
        credential = receive.credential_environment(host, tools)
        selection = {
            "repository": fetch.REPOSITORY,
            "revision": revision,
            "packageVersion": package_version(sources["Cargo.toml"], control),
            "target": "x86_64-unknown-linux-musl",
            "protocolVersion": 1,
            "buildRunId": options.run_id,
            "runAttempt": options.run_attempt,
        }
        request = receive.encoded(
            {
                "schemaVersion": 1,
                "selection": selection,
                "artifactId": options.artifact_id,
                "output": str(output / "ops-acquired"),
                "authentication": "gh-token",
            }
        )
        path = output / "ops-request.json"
        write_private(path, request, mode=0o600)
        original_request = fetch.Held.open(path, MAX_RECORD, control)
        owners.inputs.append(original_request)
        report["failure"] = report["checkpoint"] = "authentication"
        tools.check(control)
        _ = host.command(
            "authenticate-ops",
            [
                str(controller),
                "ops-artifact",
                "acquire",
                "--request",
                str(path),
                "--request-sha256",
                hashlib.sha256(request).hexdigest(),
            ],
            credential,
            clipped(control, 300),
            (0,),
        )
        report["opsVerifierCalls"] = 3
        original_request.check(control)
        tools.check(control)
        tree = receive.OpsTree(output / "ops-acquired/input", control)
        retained = receive.ops_payloads(tree, owners)
        receive.current_ops_verification(host, tree, retained, controller, selection, credential)
        tools.check(control)
        report.update(
            {
                "opsVerifierCalls": 6,
                "authenticatedOpsDistribution": True,
                "opsArtifactId": options.artifact_id,
                "opsSelection": selection,
            }
        )
        timing(report, "authentication", control)
        report["failure"] = report["checkpoint"] = "server-authentication"
        acquired = output / "server-acquired"
        _ = host.command(
            "authenticate-server",
            [
                sys.executable,
                "-B",
                str(ROOT / "build/security_vm_release.py"),
                "--repository",
                fetch.REPOSITORY,
                "--output",
                str(acquired),
            ],
            credential,
            clipped(control, 300),
            (0,),
        )
        tools.check(control)
        report["serverVerifierCalls"] = 4
        selected_original = fetch.Held.open(acquired / "selection.json", 16384, control)
        owners.inputs.append(selected_original)
        selected = fetch.selection(selected_original.path, revision)
        require(selected["ciRunId"] == options.run_id, "maintain_vm_same_run")
        server_tree = stage_vm.ServerTree(acquired / "artifact", control)
        pins: dict[str, fetch.Held] = {}
        for name in sorted(stage_vm.SERVER_NAMES):
            original = fetch.Held.open(
                server_tree.path / name,
                2 * 1024**3 if name in {"image.tar", "artifact.zip"} else 64 * 1024**2,
                control,
            )
            owners.inputs.append(original)
            pins[name] = original
        receive.server_attempt(
            receive.private_record(pins["release-predicate.json"].path),
            options.run_id,
            options.run_attempt,
        )
        stage_vm.reverify_server(
            host, tools, binary, server_tree, pins, selected, options.run_attempt, credential
        )
        report["serverVerifierCalls"] = 8
        report["serverCurrentMetadataApiCalls"] = 2
        report["authenticatedServerDistribution"] = True
        selected_original.check(control)
        stage_vm.check_server(server_tree, pins)
        timing(report, "server-authentication", control)
        report["failure"] = report["checkpoint"] = "source-bundle"
        bundle, source_tree = source_bundle(host, git, revision, sources, owners)
        timing(report, "source-bundle", control)
        run_id = uuid.uuid4().hex
        report["runId"] = run_id
        archive, loan = receive.transfer(
            host, owners, retained, selection, binary, output, run_id, options.artifact_id
        )
        pin = vm.CloudImage.read()
        report["cloudImageSha512"] = pin.sha512
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = cast("tuple[str, int]", listener.getsockname())[1]
        report["failure"] = report["checkpoint"] = "boot"
        vm.prepare_disk(host, work, pin)
        vm.create_seed(host, work, port, run_id)
        active = owners.guest = vm.Guest(
            host,
            vm.qemu_command(work, port, run_id),
            qmp_path=work / "qmp.sock",
            lifetime_deadline=control.end,
        )
        host.guest = active
        vm.boot(host, active, work, port)
        timing(report, "boot", control)
        report["failure"] = report["checkpoint"] = "install"
        ops.stage(cast("ops.Host", cast("object", host)), active, work, port, archive.options)
        installed = ops.exercise(
            cast("ops.Host", cast("object", host)), active, work, port, archive
        )
        require(installed["passed"] is True, "maintain_vm_ops33")
        report["opsCases"], report["install"] = installed["cases"], installed["install"]
        timing(report, "install", control)
        receive.check_ops(tree, retained)
        tools.check(control)
        inventory = receive.prepared_inventory(owners, work, port, run_id, revision, control)
        pg_image, engine_version = fixture_config(sources[HOST_FILES[0]], control)
        values = output / "fixture-input-vars.json"
        write_private(
            values,
            receive.encoded(
                {
                    "vm_maintain_bundle_path": str(bundle.path),
                    "vm_maintain_bundle_sha256": bundle.sha256,
                    "vm_current_artifact_dir": str(server_tree.path),
                }
            ),
            mode=0o600,
        )
        values_original = fetch.Held.open(values, MAX_RECORD, control)
        owners.inputs.append(values_original)
        report["failure"] = report["checkpoint"] = "fixture-setup"
        _, data = host.run(
            "maintain-fixture-setup",
            [
                executable("ansible-playbook"),
                "-i",
                str(inventory),
                str(ROOT / "ops/ansible/security-vm-maintain-prerequisite.yml"),
                "--extra-vars",
                "@" + str(values),
            ],
            timeout=clipped(control, 300),
        )
        _ = vm.recap(data, unchanged=False)
        bundle.check(control)
        values_original.check(control)
        stage_vm.check_server(server_tree, pins)
        timing(report, "fixture-setup", control)
        report["failure"] = report["checkpoint"] = "guest-setup"
        session = receive.Session(ssh(host, work, port), host.env, control)
        control.check()
        sample = time.monotonic()
        expected: dict[str, object] = {
            "schemaVersion": 1,
            "scope": "MaintainLauncherPrerequisiteRequest",
            "postgresImage": pg_image,
            "engineVersion": engine_version,
            "caddyImage": caddy.IMAGE,
            "runId": run_id,
            "revision": revision,
            "scenario": options.scenario,
            "setupSeconds": math.floor(control.started + SETUP - sample),
            "bodySeconds": math.floor(control.whole_body_end - sample),
            "endSeconds": math.floor(control.end - sample),
            "sourcePins": {name: sources[name].sha256 for name in sorted(guest.GUEST_SOURCES)},
            "helperPins": {
                name: sources[
                    "build/security_vm_ops_guest.py"
                    if name == "security_vm_ops_guest.py"
                    else "ops/ansible/files/" + name
                ].sha256
                for name in set(ops_guest.BASE | ops_guest.FETCH | ops_guest.FORCE)
                | {"security_vm_ops_guest.py"}
            },
            "binary": {
                "sha256": retained["simplestchat-ops"].sha256,
                "bytes": retained["simplestchat-ops"].size,
            },
            "manifest": {"sha256": pins["release.json"].sha256, "bytes": pins["release.json"].size},
            "archive": {"sha256": pins["image.tar"].sha256, "bytes": pins["image.tar"].size},
            "sourceBundle": {"sha256": bundle.sha256, "bytes": bundle.size},
            "sourceTree": source_tree,
        }
        control.check()
        session.send(expected)
        first_frame = session.line(clipped(control, 300))
        if first_frame.get("scope") == "MaintainLauncherPrerequisiteGuest":
            report["maintainProof"] = guest.final_projection(first_frame, expected)
            require(None, "maintain_vm_setup_unproven")
        ready = guest.exact(first_frame, {"schemaVersion", "scope", "runId", "ready"})
        require(
            type(ready["schemaVersion"]) is int and ready["ready"] is True,
            "maintain_vm_ready_types",
        )
        require(
            ready
            == {
                "schemaVersion": 1,
                "scope": "MaintainLauncherPrerequisiteReady",
                "runId": run_id,
                "ready": True,
            },
            "maintain_vm_ready",
        )
        timing(report, "guest-setup", control)
        receive.check_ops(tree, retained)
        stage_vm.check_server(server_tree, pins)
        bundle.check(control)
        values_original.check(control)
        git.check(control)
        tools.check(control)
        control.advance()
        timing(report, "ready", control)
        host.deadline = control.body_end
        require(control.whole_body_end - time.monotonic() >= DEBT, "maintain_vm_native_debt")
        control.check()
        session.send({"schemaVersion": 1, "runId": run_id, "go": True})
        report["failure"] = report["checkpoint"] = "native"
        final = guest.final_projection(session.line(clipped(control, DEBT)), expected)
        timing(report, "native", control)
        report["maintainProof"] = final
        require(final["passed"] is True, "maintain_vm_trial_unproven")
        stage_vm.check_server(server_tree, pins)
        bundle.check(control)
        git.check(control)
        receive.check_ops(tree, retained)
        tools.check(control)
        control.check()
        report["failure"] = None
        report["checkpoint"] = "complete"
    except BaseException as error:  # noqa: BLE001 -- Finalize every known peer after any failure.
        first = error
    finally:
        report["failureKind"] = failure_kind(first)
        first = fetch.settle((lambda: timing(report, "body-end", control),), first)
        # Observe the LIVE body before finalizers may use the original final end.
        try:
            control.check()
        except BaseException as error:  # noqa: BLE001 -- Keep earlier body identity.
            first = first or error
        control.settling = True
        if host is not None:
            host.deadline = control.end
        actions: list[Callable[[], None]] = []
        if session is not None:

            def close_session() -> None:
                raw = session.finish()
                report["guestRawExit"] = raw
                if report["failure"] is None:
                    require(raw == 0, "maintain_vm_guest_exit")

            actions.extend((close_session, session.close))
        if tools is not None:
            actions.append(lambda: tools.check(control))
        if archive is not None:
            actions.extend(
                (
                    lambda: archive.check(cast("ops_guest.Control", cast("object", control))),
                    archive.close,
                )
            )
        if loan is not None:
            actions.append(loan.close)
        if server_tree is not None:
            actions.append(server_tree.finish)
        if tree is not None:
            actions.append(tree.finish)
        cleanup = fetch.settle(actions)
        owned_cleanup = owners.finish(control, report)
        first = first or cleanup or owned_cleanup
        if report["failureKind"] is None:
            report["failureKind"] = failure_kind(first)
        report["cleanupPassed"] = (
            cleanup is None
            and owned_cleanup is None
            and all(original.stream.closed for original in owners.inputs)
        )
        if first is not None and report["failure"] is None:
            report["failure"] = "finalization"
        report["passed"] = first is None and control.status == 0
    return report


class Parser(argparse.ArgumentParser):
    """Keep selector text out of public diagnostics."""

    @override
    def error(self, message: str) -> NoReturn:
        """Refuse syntax with a fixed safe message."""
        _ = message
        self.exit(2, "Maintenance launcher VM selectors are invalid.\n")


def main() -> int:  # noqa: C901 -- One original handler/mask/output restoration boundary.
    """Keep handler/umask/output and original-clock fences after all owned cleanup."""
    control = Control()
    parser = Parser(allow_abbrev=False)
    for name in ("run-id", "run-attempt", "artifact-id"):
        _ = parser.add_argument("--" + name, required=True, type=int)
    _ = parser.add_argument("--scenario", required=True, choices=sorted(guest.CASES))
    _ = parser.add_argument("--output", required=True, type=Path)
    options = parser.parse_args(namespace=Options())
    if not all(
        0 < getattr(options, name) <= receive.SAFE
        for name in ("run_id", "run_attempt", "artifact_id")
    ):
        return 2
    mask = os.umask(0o077)
    handlers: dict[signal.Signals, object] = {}
    status = 1
    try:
        for number in (signal.SIGINT, signal.SIGTERM):
            handlers[number] = signal.signal(number, control.signal)
        report = run(options, control)
        data = receive.encoded(report)
        require(len(data) <= MAX_RECORD, "maintain_vm_summary_bound")
        if options.output.is_dir():
            write_private(options.output / "summary.json", data, mode=0o600)
        require(sys.stdout.buffer.write(data) == len(data), "maintain_vm_stdout")
        _ = sys.stdout.buffer.flush()
        status = 0 if report["passed"] is True else 1
    except (OSError, ValueError, ToolError, RuntimeError):
        _ = sys.stderr.write("Signed musl maintenance launcher VM proof is unproven.\n")
    finally:
        actions: list[Callable[[], None]] = []
        for number, handler in handlers.items():

            def restore(number: signal.Signals = number, handler: object = handler) -> None:
                _ = signal.signal(
                    number, cast("int | Callable[[int, object], None] | None", handler)
                )

            actions.append(restore)

        def restore_mask() -> None:
            _ = os.umask(mask)

        actions.append(restore_mask)
        if fetch.settle(actions) is not None:
            status = 1
    try:
        control.check()
    except (OSError, ValueError, ToolError):
        status = 1
    return control.status or status


if __name__ == "__main__":
    raise SystemExit(main())
