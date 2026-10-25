"""Own one fixed signed-musl backup-verification trial in a fresh marked guest.

This optional keeper composes existing authentication/install and VM owners. It
has no server artifact, application deployment, host/root or command selector.
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

import security_vm as vm
import security_vm_fetch as fetch
import security_vm_ops as ops
import security_vm_receive as receive
import security_vm_restore_guest as guest
import yaml
from release_json import json_value, object_value, string_value
from security_context import ROOT, executable
from security_tools import ToolError, require, write_private

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping
    from typing import BinaryIO, NoReturn

    import security_vm_ops_guest as ops_guest

WORKFLOW = "Signed musl backup verification KVM runtime"
SECONDS = 1800
FINALIZATION = 120
SETUP = 360
NATIVE = 1200
OBSERVATION = 120
DEBT = NATIVE + OBSERVATION
MAX_RECORD = 131072
MAX_STATUS = 255
NEW_PATHS = (
    ".github/workflows/release-backup-runtime.yml",
    "build/security_vm_restore.py",
    "build/security_vm_restore_guest.py",
    "ops/ansible/security-vm-restore.yml",
    "ops/ansible/tests/test_security_vm_restore.py",
)
MIGRATIONS = tuple(
    sorted(str(path.relative_to(ROOT)) for path in (ROOT / "migrations").glob("*.sql"))
)
GUEST_FILES = (
    "build/security_vm_restore_guest.py",
    "build/security_vm_stage_guest.py",
    "ops/ansible/files/restore_verify.py",
    "ops/ansible/files/restore-verify.sql",
    "ops/ansible/files/monitoring-schema.sql",
    "ops/ansible/templates/public-runtime-grants.sql.j2",
    *MIGRATIONS,
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
    dict.fromkeys((*receive.SOURCE_INPUTS, *NEW_PATHS, *GUEST_FILES, *HOST_FILES))
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
        """Require a full1200 envelope and120 observation seconds BEFORE widening."""
        self.check()
        require(self.setup, "restore_vm_phase")
        require(self.whole_body_end - time.monotonic() >= DEBT, "restore_vm_native_debt")
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
        "restore_vm_disposable_ci",
    )


def clipped(control: Control, cap: int) -> float:
    """Keep every setup role inside the live original phase."""
    control.check()
    remaining = min(cap, control.body_end - time.monotonic())
    require(remaining > 0, "restore_vm_deadline")
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
                "/root/simplestchat-vm-fixture/security_vm_restore_guest.py",
            ]
        ),
    ]


def projection(value: object, expected: Mapping[str, object]) -> dict[str, object]:
    """Reject arbitrary guest output before constructing any public summary."""
    result = guest.exact(
        value,
        {
            "schemaVersion",
            "scope",
            "runId",
            "scenario",
            "revision",
            "passed",
            "failure",
            "nativeRawExit",
            "nativePassed",
            "finalStatus",
            "checkpoint",
            "cleanupPassed",
            "native",
            "sourcePins",
        },
    )
    require(
        result["schemaVersion"] == 1 and type(result["schemaVersion"]) is int, "restore_vm_refused"
    )
    for name in ("runId", "scenario", "revision"):
        require(result[name] == expected[name], "restore_vm_guest_binding")
    require(result["scope"] == "RestoreVerificationGuest", "restore_vm_refused")
    require(
        type(result["passed"]) is bool and type(result["cleanupPassed"]) is bool,
        "restore_vm_refused",
    )
    require(
        isinstance(result["checkpoint"], str) and result["checkpoint"] in guest.CHECKPOINTS,
        "restore_vm_refused",
    )
    require(
        result["failure"] is None
        or (isinstance(result["failure"], str) and result["failure"] in guest.FAILURES),
        "restore_vm_refused",
    )
    require(
        result["nativePassed"] is None or type(result["nativePassed"]) is bool, "restore_vm_refused"
    )
    require(
        result["nativeRawExit"] is None
        or (
            type(result["nativeRawExit"]) is int
            and -MAX_STATUS <= result["nativeRawExit"] <= MAX_STATUS
        ),
        "restore_vm_refused",
    )
    require(
        type(result["finalStatus"]) is int and result["finalStatus"] in (0, 124, 130, 143),
        "restore_vm_guest_status",
    )
    raw = result["nativeRawExit"]
    require(
        result["nativePassed"] is None if raw is None else result["nativePassed"] is (raw == 0),
        "restore_vm_native_status_join",
    )
    require(result["sourcePins"] == expected["sourcePins"], "restore_vm_guest_source")
    native = result["native"]
    if native is not None:
        native = guest.native_projection(native, cast("str", expected["scenario"]))
        require(native["engineVersion"] == expected["engineVersion"], "restore_vm_engine")
        require(raw == native["nativeRawExit"] and raw is not None, "restore_vm_native_status_join")
        result["native"] = native
    if result["passed"] is True:
        require(result["finalStatus"] == 0 and raw is not None, "restore_vm_completed_status")
        require(result["cleanupPassed"] is True and result["failure"] is None, "restore_vm_refused")
        require(result["checkpoint"] == "complete" and native is not None, "restore_vm_refused")
    return result


def package_version(source: fetch.Held, control: Control) -> str:
    """Parse Cargo metadata from its retained original rather than reopening it."""
    source.check(control)
    data = source.stream.read(2**20 + 1)
    require(len(data) == source.size <= 2**20, "restore_vm_source_manifest")
    value = cast("object", tomllib.loads(data.decode())["workspace"]["package"]["version"])
    require(isinstance(value, str), "restore_vm_source_manifest")
    source.check(control)
    return cast("str", value)


def fixture_config(original: fetch.Held, control: Control) -> tuple[str, str]:
    """Read the two pinned choices with the existing mature YAML/byte adapters."""
    original.check(control)
    data = original.stream.read(2**20 + 1)
    require(len(data) == original.size <= 2**20, "restore_vm_config_bound")
    value = object_value(json_value(cast("object", yaml.safe_load(data))))
    image = string_value(value["scpub_postgres_image"])
    package = string_value(value["scbench_docker_engine_version"])
    require("@sha256:" in image and ":" in package and "-" in package, "restore_vm_config_profile")
    version = package.split(":", 1)[1].split("-", 1)[0]
    original.check(control)
    return image, version


def run(  # noqa: C901, PLR0912, PLR0915 -- Ordered owners.
    options: Options,
    control: Control,
) -> dict[str, object]:
    """Keep original authenticated bytes and all peers until the final signed VM result."""
    report: dict[str, object] = {
        "schemaVersion": 1,
        "scope": "SignedMuslBackupVerificationKvm",
        "scenario": options.scenario,
        "passed": False,
        "failure": "guard",
        "checkpoint": "guard",
        "cleanupPassed": False,
        "opsVerifierCalls": 0,
        "opsCases": [],
        "restoreProof": None,
        "authenticatedOpsDistribution": False,
        "nativeTarget": "x86_64-unknown-linux-musl",
        "controllerBinaryUse": "source-built-gnu-passive-ELF-inspection-only",
        "guestClientWholeGroupIndependentlyObserved": False,
    }
    owners = fetch.Owners()
    tools: fetch.NativeTools | None = None
    tree: receive.OpsTree | None = None
    archive: ops.Input | None = None
    loan: BinaryIO | None = None
    session: receive.Session | None = None
    host: receive.Host | None = None
    first: BaseException | None = None
    try:
        admit(os.environ)
        require(options.scenario in guest.CASES, "restore_vm_case")
        output = options.output
        require(
            output.is_absolute() and output.resolve() == output and not output.exists(),
            "restore_vm_refused",
        )
        require(os.environ.get("GH_TOKEN"), "restore_vm_token")
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
            host, vm.qemu_command(work, port, run_id), qmp_path=work / "qmp.sock"
        )
        host.guest = active
        vm.boot(host, active, work, port)
        report["failure"] = report["checkpoint"] = "install"
        ops.stage(cast("ops.Host", cast("object", host)), active, work, port, archive.options)
        installed = ops.exercise(
            cast("ops.Host", cast("object", host)), active, work, port, archive
        )
        require(installed["passed"] is True, "restore_vm_ops33")
        report["opsCases"], report["install"] = installed["cases"], installed["install"]
        receive.check_ops(tree, retained)
        tools.check(control)
        inventory = receive.prepared_inventory(owners, work, port, run_id, revision, control)
        report["failure"] = report["checkpoint"] = "fixture-setup"
        _, data = host.run(
            "restore-fixture-setup",
            [
                executable("ansible-playbook"),
                "-i",
                str(inventory),
                str(ROOT / "ops/ansible/security-vm-restore.yml"),
            ],
            timeout=clipped(control, 300),
        )
        _ = vm.recap(data, unchanged=False)
        pg_image, engine_version = fixture_config(sources[HOST_FILES[0]], control)
        expected = {
            "schemaVersion": 1,
            "scope": "RestoreVerificationRequest",
            "postgresImage": pg_image,
            "engineVersion": engine_version,
            "runId": run_id,
            "revision": revision,
            "scenario": options.scenario,
            "setupSeconds": math.floor(control.started + SETUP - time.monotonic()),
            "bodySeconds": math.floor(control.whole_body_end - time.monotonic()),
            "sourcePins": {name: sources[name].sha256 for name in GUEST_FILES},
            "binary": {
                "sha256": retained["simplestchat-ops"].sha256,
                "bytes": retained["simplestchat-ops"].size,
            },
        }
        session = receive.Session(ssh(host, work, port), host.env, control)
        session.send(expected)
        first_frame = session.line(clipped(control, 300))
        if first_frame.get("scope") == "RestoreVerificationGuest":
            report["restoreProof"] = projection(first_frame, expected)
            require(None, "restore_vm_setup_unproven")
        ready = guest.exact(first_frame, {"schemaVersion", "scope", "runId", "ready"})
        require(
            type(ready["schemaVersion"]) is int and ready["ready"] is True, "restore_vm_ready_types"
        )
        require(
            ready
            == {
                "schemaVersion": 1,
                "scope": "RestoreVerificationReady",
                "runId": run_id,
                "ready": True,
            },
            "restore_vm_ready",
        )
        control.advance()
        host.deadline = control.body_end
        require(control.whole_body_end - time.monotonic() >= DEBT, "restore_vm_native_debt")
        session.send({"schemaVersion": 1, "runId": run_id, "go": True})
        report["failure"] = report["checkpoint"] = "restore"
        final = projection(session.line(clipped(control, DEBT)), expected)
        report["restoreProof"] = final
        require(final["passed"] is True, "restore_vm_trial_unproven")
        receive.check_ops(tree, retained)
        tools.check(control)
        control.check()
        report["failure"] = None
        report["checkpoint"] = "complete"
    except BaseException as error:  # noqa: BLE001 -- Finalize every known peer after any failure.
        first = error
    finally:
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
                    require(raw == 0, "restore_vm_guest_exit")

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
        if tree is not None:
            actions.append(tree.finish)
        cleanup = fetch.settle(actions)
        owned_cleanup = owners.finish(control, report)
        first = first or cleanup or owned_cleanup
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
        self.exit(2, "Restore VM selectors are invalid.\n")


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
        require(len(data) <= MAX_RECORD, "restore_vm_summary_bound")
        if options.output.is_dir():
            write_private(options.output / "summary.json", data, mode=0o600)
        require(sys.stdout.buffer.write(data) == len(data), "restore_vm_stdout")
        _ = sys.stdout.buffer.flush()
        status = 0 if report["passed"] is True else 1
    except (OSError, ValueError, ToolError, RuntimeError):
        _ = sys.stderr.write("Signed musl restore VM proof is unproven.\n")
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
