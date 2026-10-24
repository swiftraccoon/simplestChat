"""Compose authenticated originals and one actual signed-musl host deploy KVM proof.

Only this manual disposable fixture performs old/current DB-Caddy setup and an
owned candidate stop. Production Python/callers/bootstrap remain unchanged.
"""

from __future__ import annotations

import argparse
import hashlib
import math
import os
import re
import shlex
import signal
import socket
import sys
import time
import tomllib
import uuid
from pathlib import Path
from typing import TYPE_CHECKING, NoReturn, cast, final, override

import security_vm as vm
import security_vm_deploy_guest as probe
import security_vm_fetch as fetch_vm
import security_vm_ops as ops_vm
import security_vm_ops_guest as ops_guest
import security_vm_receive as receive
import security_vm_stage as stage
from security_context import ROOT, executable
from security_tools import ToolError, require, write_private

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping, Sequence
    from typing import BinaryIO

WORKFLOW = "Signed musl host deploy KVM runtime"
MAX_RECORD = 131072
FIXTURE_SECONDS = 4500
FINALIZATION_SECONDS = 180
SETUP_SECONDS = 600
MINIMUM_SETUP = 30
NATIVE_SECONDS = 1800
OBSERVATION_SECONDS = 120
CONTINUE_MINIMUM = 2 * NATIVE_SECONDS + OBSERVATION_SECONDS
NEW_PATHS = (
    ".github/workflows/release-deploy-runtime.yml",
    "build/security_vm_deploy.py",
    "build/security_vm_deploy_guest.py",
    "ops/ansible/security-vm-deploy.yml",
    "ops/ansible/tests/test_security_vm_deploy.py",
    "docs/release-deploy-runtime-proof.md",
)
PROJECT_FILES = (
    "build/release_container_fixture.py",
    "build/release_container_harness.py",
    "docker-compose.yml",
    "Caddyfile",
    *tuple(
        "ops/ansible/templates/public-" + name + ".j2"
        for name in (
            "app.env",
            "migration.env",
            "proxy.env",
            "compose.yml",
            "pg_hba.conf",
            "init-database.sql",
            "runtime-grants.sql",
            "postgres-admin-password",
        )
    ),
)
GUEST_SOURCES = (*PROJECT_FILES, "build/security_vm_stage_guest.py")
SOURCE_INPUTS = tuple(dict.fromkeys((*stage.SOURCE_INPUTS, *NEW_PATHS, *PROJECT_FILES)))


@final
class Control(fetch_vm.Control):
    """One entry clock with a strict initial setup ceiling, never a reset."""

    setup: bool = True

    @property
    @override
    def end(self) -> float:
        """Keep the original fixture final deadline."""
        return self.started + FIXTURE_SECONDS

    @property
    def setup_end(self) -> float:
        """Keep the strict initial setup deadline."""
        return self.started + SETUP_SECONDS

    @property
    def whole_body_end(self) -> float:
        """Reserve the original fixture finalization window."""
        return self.end - FINALIZATION_SECONDS

    @property
    @override
    def body_end(self) -> float:
        """Clip work to the current phase without restarting the clock."""
        return min(self.setup_end, self.whole_body_end) if self.setup else self.whole_body_end

    def release_setup(self) -> None:
        """Admit both full native envelopes before leaving setup."""
        self.check()
        require(self.setup and time.monotonic() < self.setup_end, "deploy_vm_setup_ceiling")
        require(
            self.whole_body_end - time.monotonic() >= CONTINUE_MINIMUM,
            "deploy_vm_two_full_envelopes",
        )
        self.check()
        self.setup = False


@final
class Options(argparse.Namespace):
    """Carry only the closed manual workflow selectors."""

    run_id: int = 0
    run_attempt: int = 0
    artifact_id: int = 0
    output: Path = Path()
    server_artifact_id: int = 0
    previous_revision: str = ""
    previous_run_id: int = 0
    previous_run_attempt: int = 0
    previous_artifact_id: int = 0


def admit(environment: Mapping[str, str]) -> None:
    """Require the deliberate free same-head manual GitHub boundary."""
    require(
        sys.platform == "linux"
        and environment.get("GITHUB_ACTIONS") == "true"
        and environment.get("RUNNER_ENVIRONMENT") == "github-hosted"
        and environment.get("GITHUB_REPOSITORY") == fetch_vm.REPOSITORY
        and environment.get("GITHUB_REF") == "refs/heads/main"
        and environment.get("GITHUB_WORKFLOW") == WORKFLOW,
        "deploy_vm_disposable_ci",
    )


def budget(control: fetch_vm.Control, cap: int = 300) -> int:
    """Clip a fixed role to the same original fixture clock."""
    return stage.budget(control, cap)


def payload_pins(pins: Mapping[str, fetch_vm.Held]) -> dict[str, object]:
    """Project only the held authenticated payload identities."""
    return {
        name: {"sha256": pins[name].sha256, "bytes": pins[name].size}
        for name in ("release.json", "image.tar")
    }


def manifest_map(original: fetch_vm.Held, control: fetch_vm.Control) -> dict[str, object]:
    """Read the packaged schema from the original authenticated file."""
    original.check(control)
    _ = original.stream.seek(0)
    value = probe.record(original.stream.read(2**20))
    original.check(control)
    require(isinstance(value.get("migrations"), dict), "deploy_vm_migration_map")
    return cast("dict[str, object]", value["migrations"])


def acquire_previous(  # noqa: PLR0913, PLR0917 -- Fixed old-selection owner roles.
    host: receive.Host,
    tools: fetch_vm.NativeTools,
    binary: Path,
    owners: fetch_vm.Owners,
    trees: list[stage.ServerTree],
    options: Options,
    credential: Mapping[str, str],
) -> tuple[stage.ServerTree, dict[str, fetch_vm.Held]]:
    """Old A is explicit fresh API/signatures, never the current checkout identity."""
    n = budget(host.control)
    parent = options.output / "previous-acquired"
    tools.check(host.control)
    _, data = host.command(
        "authenticate-previous-server",
        [
            str(binary),
            "release",
            "verify",
            "--repository",
            fetch_vm.REPOSITORY,
            "--revision",
            options.previous_revision,
            "--artifact-id",
            str(options.previous_artifact_id),
            "--ci-run",
            str(options.previous_run_id),
            "--output-parent",
            str(parent),
            "--timeout-seconds",
            str(n),
            "--finalization-seconds",
            "60",
        ],
        credential,
        n + 10,
        (0,),
    )
    tools.check(host.control)
    value = probe.exact(probe.record(data), {"revision", "directory", "image.tar", "release.json"})
    require(
        value["revision"] == options.previous_revision and isinstance(value["directory"], str),
        "deploy_vm_previous_selection",
    )
    directory = Path(cast("str", value["directory"]))
    require(
        directory.is_absolute()
        and directory.resolve() == directory
        and directory.is_relative_to(parent),
        "deploy_vm_previous_directory",
    )
    tree = stage.ServerTree(directory, host.control)
    trees.append(tree)
    pins: dict[str, fetch_vm.Held] = {}
    for name in sorted(stage.SERVER_NAMES):
        original = fetch_vm.Held.open(
            directory / name,
            2 * 1024**3 if name in {"image.tar", "artifact.zip"} else 64 * 1024**2,
            host.control,
        )
        owners.inputs.append(original)
        pins[name] = original
    for name in ("image.tar", "release.json"):
        require(value[name] == pins[name].sha256, "deploy_vm_previous_original")
    receive.server_attempt(
        receive.private_record(pins["release-predicate.json"].path),
        options.previous_run_id,
        options.previous_run_attempt,
    )
    selected = {
        "revision": options.previous_revision,
        "artifactId": options.previous_artifact_id,
        "ciRunId": options.previous_run_id,
    }
    stage.reverify_server(
        host, tools, binary, tree, pins, selected, options.previous_run_attempt, credential
    )
    return tree, pins


def prepared_projection(
    value: Mapping[str, object], expected: Mapping[str, object]
) -> dict[str, object]:
    """Reject unknown fields and bind the passive prepared facts."""
    result = probe.exact(value, probe.PREPARED_FIELDS)
    for name in ("runId", "revision", "previousRevision"):
        require(result[name] == expected[name], "deploy_vm_prepared_identity")
    binary = cast("dict[str, object]", expected["binary"])
    require(
        result["kind"] == "prepared"
        and result["schemaVersion"] == 1
        and result["binarySha256"] == binary["sha256"]
        and result["passed"] is True,
        "deploy_vm_prepared_observation",
    )
    require(
        isinstance(result["firstImage"], str)
        and re.fullmatch(r"sha256:[a-f0-9]{64}", result["firstImage"]),
        "deploy_vm_prepared_image",
    )
    for name in ("dependencySha256", "migrationSha256"):
        _ = probe.digest(result[name])
    return result


def guest_projection(
    value: Mapping[str, object], expected: Mapping[str, object], prepared: Mapping[str, object]
) -> dict[str, object]:
    """Validate the closed completed guest observation."""
    return probe.public_result(value, expected, prepared)


def ssh(host: receive.Host, work: Path, port: int, arguments: Sequence[str]) -> list[str]:
    """Choose only the original SSH tool and fixed guest entry point."""
    original = host.fixed_tools["ssh"]
    original.check(host.control)
    return [
        str(original.path),
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
                "/root/simplestchat-vm-fixture/security_vm_deploy_guest.py",
                *arguments,
            ]
        ),
    ]


def run(  # noqa: C901, PLR0912, PLR0915 -- Ordered owners.
    options: Options, control: Control
) -> dict[str, object]:
    """Compose fixed existing owners, preserving every first failure and cleanup verdict."""
    report: dict[str, object] = {
        "schemaVersion": 1,
        "scope": "SignedMuslHostDeployKvm",
        "passed": False,
        "cleanupPassed": False,
        "failure": "guard",
        "opsVerifierCalls": 0,
        "currentServerVerifierCalls": 0,
        "nativeValidatorIsolationIndependentlyInspected": False,
        "currentServerMetadataApiCalls": 0,
        "verifierCountsAreCompletedSuccessfulRolesOnly": True,
        "guestPrivateEvidenceRetained": False,
        "guestFailureDiagnostic": None,
        "guestFailureDiagnosticStatus": "not-requested",
    }
    owners = fetch_vm.Owners()
    tools: fetch_vm.NativeTools | None = None
    ops_tree: receive.OpsTree | None = None
    server_tree: stage.ServerTree | None = None
    server_trees: list[stage.ServerTree] = []
    session: receive.Session | None = None
    archive: ops_vm.Input | None = None
    loan: BinaryIO | None = None
    host: receive.Host | None = None
    work: Path | None = None
    port = 0
    run_id: str | None = None
    first: BaseException | None = None
    try:
        admit(os.environ)
        output = options.output
        require(
            output.is_absolute() and output.resolve() == output and not output.exists(),
            "deploy_vm_output",
        )
        require(os.environ.get("GH_TOKEN"), "deploy_vm_token")
        revision = probe.digest(os.environ.get("GITHUB_SHA"), 40)
        vm.preflight(output)
        output.mkdir(mode=0o700)
        host = receive.Host(output, control)
        source: dict[str, fetch_vm.Held] = {}
        for name in SOURCE_INPUTS:
            original = fetch_vm.Held.open(ROOT / name, 2**20, control)
            owners.inputs.append(original)
            source[name] = original
        report["sourceSha256"] = {name: original.sha256 for name, original in source.items()}
        programs: list[Path] = []
        for path in (
            ROOT / "target/x86_64-unknown-linux-gnu/release/simplestchat-ops",
            ROOT / "target/debug/xtask",
        ):
            original = fetch_vm.Held.open(path.resolve(), fetch_vm.MAX_BINARY, control)
            owners.inputs.append(original)
            programs.append(original.path)
        binary, controller = programs
        for name in fetch_vm.TOOL_ROLES:
            original = fetch_vm.Held.open(
                Path(executable(name)).resolve(), fetch_vm.MAX_BINARY, control, system=True
            )
            owners.inputs.append(original)
            host.fixed_tools[name] = original
        host.env["ANSIBLE_SSH_EXECUTABLE"] = str(host.fixed_tools["ssh"].path)
        owners.work = fetch_vm.Work.create(output)
        work = owners.work.path
        tools = fetch_vm.NativeTools.create(owners.work, host.fixed_tools, control)
        credential = receive.credential_environment(host, tools)
        cargo = source["Cargo.toml"]
        cargo.check(control)
        package = cast(
            "object",
            tomllib.loads(cargo.stream.read(2**20).decode())["workspace"]["package"]["version"],
        )
        require(isinstance(package, str), "deploy_vm_source_package")
        cargo.check(control)
        selection = {
            "repository": fetch_vm.REPOSITORY,
            "revision": revision,
            "packageVersion": package,
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
        request_path = output / "ops-request.json"
        write_private(request_path, request, mode=0o600)
        request_original = fetch_vm.Held.open(request_path, MAX_RECORD, control)
        owners.inputs.append(request_original)
        report["failure"] = "ops-authentication"
        _ = host.command(
            "authenticate-ops",
            [
                str(controller),
                "ops-artifact",
                "acquire",
                "--request",
                str(request_path),
                "--request-sha256",
                hashlib.sha256(request).hexdigest(),
            ],
            credential,
            budget(control) + 10,
            (0,),
        )
        report["opsVerifierCalls"] = 3
        request_original.check(control)
        tools.check(control)
        ops_tree = receive.OpsTree(output / "ops-acquired/input", control)
        retained = receive.ops_payloads(ops_tree, owners)
        receive.current_ops_verification(
            host, ops_tree, retained, controller, selection, credential
        )
        report["opsVerifierCalls"] = 6
        tools.check(control)
        report.update(
            {
                "authenticatedOpsDistribution": True,
                "opsArtifactId": options.artifact_id,
                "opsSelection": selection,
            }
        )
        report["failure"] = "server-authentication"
        acquired = output / "server-acquired"
        _ = host.command(
            "authenticate-server",
            [
                sys.executable,
                "-B",
                str(ROOT / "build/security_vm_release.py"),
                "--repository",
                fetch_vm.REPOSITORY,
                "--output",
                str(acquired),
            ],
            credential,
            budget(control) + 10,
            (0,),
        )
        tools.check(control)
        report["currentServerVerifierCalls"] = 4
        selected_original = fetch_vm.Held.open(acquired / "selection.json", 16384, control)
        owners.inputs.append(selected_original)
        selected = fetch_vm.selection(selected_original.path, revision)
        require(
            selected["ciRunId"] == options.run_id
            and selected["artifactId"] == options.server_artifact_id,
            "deploy_vm_same_run",
        )
        server_tree = stage.ServerTree(acquired / "artifact", control)
        server_trees.append(server_tree)
        pins: dict[str, fetch_vm.Held] = {}
        for name in sorted(stage.SERVER_NAMES):
            original = fetch_vm.Held.open(
                server_tree.path / name,
                2 * 1024**3 if name in {"image.tar", "artifact.zip"} else 64 * 1024**2,
                control,
            )
            owners.inputs.append(original)
            pins[name] = original
        predicate = receive.private_record(pins["release-predicate.json"].path)
        receive.server_attempt(predicate, options.run_id, options.run_attempt)
        stage.reverify_server(
            host, tools, binary, server_tree, pins, selected, options.run_attempt, credential
        )
        report["currentServerVerifierCalls"] = 8
        report["currentServerMetadataApiCalls"] = 2
        selected_original.check(control)
        report.update(
            {
                "authenticatedServerDistribution": True,
                "revision": revision,
                "serverArtifactId": selected["artifactId"],
                "serverCiRunId": selected["ciRunId"],
                "serverZipSha256": pins["artifact.zip"].sha256,
            }
        )
        previous_tree, previous_pins = acquire_previous(
            host, tools, binary, owners, server_trees, options, credential
        )
        current_map = manifest_map(pins["release.json"], control)
        require(
            current_map == manifest_map(previous_pins["release.json"], control),
            "deploy_vm_same_packaged_schema",
        )
        report["previousServerVerifierCalls"] = 8
        report["previousServerMetadataApiCalls"] = 2
        report["previousRevision"] = options.previous_revision
        report["previousServerArtifactId"] = options.previous_artifact_id
        report["previousServerCiRunId"] = options.previous_run_id
        run_id = uuid.uuid4().hex
        report["runId"] = run_id
        receive.check_ops(ops_tree, retained)
        archive, loan = receive.transfer(
            host, owners, retained, selection, binary, output, run_id, options.artifact_id
        )
        pin = vm.CloudImage.read()
        report["cloudImageSha512"] = pin.sha512
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = cast("tuple[str, int]", listener.getsockname())[1]
        report["failure"] = "boot"
        vm.prepare_disk(host, work, pin)
        vm.create_seed(host, work, port, run_id)
        guest = owners.guest = vm.Guest(
            host, vm.qemu_command(work, port, run_id), qmp_path=work / "qmp.sock"
        )
        host.guest = guest
        vm.boot(host, guest, work, port)
        report["failure"] = "authenticated-install"
        ops_vm.stage(cast("ops_vm.Host", cast("object", host)), guest, work, port, archive.options)
        receive.check_ops(ops_tree, retained)
        installed = ops_vm.exercise(
            cast("ops_vm.Host", cast("object", host)), guest, work, port, archive
        )
        require(installed["passed"] is True, "deploy_vm_ops_install")
        report["opsCases"], report["install"] = installed["cases"], installed["install"]
        inventory = receive.prepared_inventory(owners, work, port, run_id, revision, control)
        setup_started = time.monotonic()
        report["failure"] = "cold-docker-setup"
        stage.check_server(server_tree, pins)
        source_vars = output / "deploy-vars.json"
        write_private(
            source_vars,
            receive.encoded(
                {
                    "vm_current_artifact_dir": str(server_tree.path),
                    "vm_previous_artifact_dir": str(previous_tree.path),
                    "vm_previous_revision": options.previous_revision,
                }
            ),
            mode=0o600,
        )
        original_vars = fetch_vm.Held.open(source_vars, MAX_RECORD, control)
        owners.inputs.append(original_vars)
        _, data = host.run(
            "deploy-docker-setup",
            [
                executable("ansible-playbook"),
                "-i",
                str(inventory),
                str(ROOT / "ops/ansible/security-vm-deploy.yml"),
                "--extra-vars",
                "@" + str(source_vars),
            ],
            timeout=budget(control, 600),
        )
        _ = vm.recap(data, unchanged=False)
        report["coldDockerSetupSeconds"] = time.monotonic() - setup_started
        stage.check_server(server_tree, pins)
        receive.check_ops(ops_tree, retained)
        tools.check(control)
        setup_seconds = math.floor(control.setup_end - time.monotonic() - 10)
        body_seconds = math.floor(control.whole_body_end - time.monotonic() - 20)
        require(
            setup_seconds >= MINIMUM_SETUP and body_seconds >= CONTINUE_MINIMUM,
            "deploy_vm_setup_admission",
        )
        expected = {
            "schemaVersion": 1,
            "runId": run_id,
            "revision": revision,
            "previousRevision": options.previous_revision,
            "setupSeconds": setup_seconds,
            "bodySeconds": body_seconds,
            "guestSha256": source["build/security_vm_deploy_guest.py"].sha256,
            "sourcePins": {name: source[name].sha256 for name in GUEST_SOURCES},
            "helperPins": {
                name: source[
                    "build/security_vm_ops_guest.py"
                    if name == "security_vm_ops_guest.py"
                    else "ops/ansible/files/" + name
                ].sha256
                for name in (
                    set(ops_guest.BASE | ops_guest.FETCH | ops_guest.FORCE)
                    | {"security_vm_ops_guest.py"}
                )
            },
            "binary": {
                "sha256": retained["simplestchat-ops"].sha256,
                "bytes": retained["simplestchat-ops"].size,
            },
            "current": payload_pins(pins),
            "previous": payload_pins(previous_pins),
        }
        report["failure"] = "prepare-signed-old-runtime"
        tools.check(control)
        stage.check_server(server_tree, pins)
        stage.check_server(previous_tree, previous_pins)
        session = receive.Session(ssh(host, work, port, ()), host.env, control)
        session.send(expected)
        prepared = prepared_projection(session.line(control.setup_end - time.monotonic()), expected)
        control.release_setup()
        host.deadline = control.body_end
        control.check()
        require(
            control.body_end - time.monotonic() >= CONTINUE_MINIMUM, "deploy_vm_two_full_envelopes"
        )
        for original in source.values():
            original.check(control)
        receive.check_ops(ops_tree, retained)
        stage.check_server(server_tree, pins)
        stage.check_server(previous_tree, previous_pins)
        report["failure"] = "native-deploy-positive-and-owned-rollback"
        control.check()
        require(
            control.body_end - time.monotonic() >= CONTINUE_MINIMUM, "deploy_vm_two_full_envelopes"
        )
        session.send({"schemaVersion": 1, "kind": "continue", "runId": run_id})
        observed = session.line(control.body_end - time.monotonic() - 20)
        report["deployProof"] = guest_projection(observed, expected, prepared)
        require(session.finish() == 0, "deploy_vm_guest_terminal")
        stage.check_server(server_tree, pins)
        stage.check_server(previous_tree, previous_pins)
        receive.check_ops(ops_tree, retained)
        report["failure"] = None
    except BaseException as error:  # noqa: BLE001 -- Keep first; settle all.
        first = error
    finally:
        body_was_positive = first is None
        first = fetch_vm.settle((control.check,), first)
        control.settling = True
        if body_was_positive and first is not None and report["failure"] is None:
            report["failure"] = "body-completion"
        session_error = fetch_vm.settle((session.close,)) if session is not None else None
        first = first or session_error
        if host is not None:
            host.deadline = control.end
        evidence_error: BaseException | None = None
        if (
            host is not None
            and owners.guest is not None
            and work is not None
            and run_id is not None
        ):
            try:
                _ = host.run(
                    "deploy-private-evidence",
                    ssh(host, work, port, ("--private-evidence", run_id)),
                    timeout=min(20, control.end - time.monotonic()),
                )
                report["guestPrivateEvidenceRetained"] = True
            except BaseException as error:  # noqa: BLE001 -- Keep evidence failure; settle all.
                evidence_error = error
        if (
            first is not None
            and host is not None
            and owners.guest is not None
            and work is not None
            and run_id is not None
        ):
            report["guestFailureDiagnosticStatus"] = "unavailable"
            try:
                _, diagnostic = host.run(
                    "deploy-failure-diagnostic",
                    ssh(host, work, port, ("--failure-diagnostic", run_id)),
                    timeout=min(20, control.end - time.monotonic()),
                )
                report["guestFailureDiagnostic"] = probe.failure_projection(
                    probe.record(diagnostic), run_id
                )
                report["guestFailureDiagnosticStatus"] = "available"
            except BaseException:  # noqa: BLE001 -- Still attempt every original finalizer.
                # Diagnostics never replace the already-latched body failure or skip finalizers.
                report["guestFailureDiagnosticStatus"] = "unavailable"
        actions: list[Callable[[], None]] = []
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
        if ops_tree is not None:
            actions.append(ops_tree.finish)
        actions.extend(tree.finish for tree in server_trees)
        cleanup = fetch_vm.settle(actions)
        owned_cleanup = owners.finish(control, report)
        cleanup = session_error or cleanup or owned_cleanup
        report["allOriginalInputStreamsClosed"] = all(
            original.stream.closed for original in owners.inputs
        )
        if not report["allOriginalInputStreamsClosed"]:
            cleanup = cleanup or ToolError("deploy_vm_original_close")
        first = first or evidence_error or cleanup
        report["cleanupPassed"] = cleanup is None
        if first is not None and report["failure"] is None:
            report["failure"] = "cleanup"
        report["passed"] = first is None and not control.status
    return report


class Parser(argparse.ArgumentParser):
    """Do not echo arbitrary external selector spellings."""

    @override
    def error(self, message: str) -> NoReturn:
        """Return a fixed syntax diagnostic without private input."""
        _ = message
        self.exit(2, "Deploy VM artifact selectors are invalid.\n")


def main() -> int:  # noqa: C901 -- Fixed selectors and all restores.
    """Return positive only after original VM, stdout, handler, umask and clock fences."""
    control = Control()
    parser = Parser(allow_abbrev=False)
    for name in (
        "run-id",
        "run-attempt",
        "artifact-id",
        "server-artifact-id",
        "previous-run-id",
        "previous-run-attempt",
        "previous-artifact-id",
    ):
        _ = parser.add_argument("--" + name, required=True, type=int)
    _ = parser.add_argument("--previous-revision", required=True)
    _ = parser.add_argument("--output", required=True, type=Path)
    options = parser.parse_args(namespace=Options())
    if not all(
        0 < getattr(options, name) <= receive.SAFE
        for name in (
            "run_id",
            "run_attempt",
            "artifact_id",
            "server_artifact_id",
            "previous_run_id",
            "previous_run_attempt",
            "previous_artifact_id",
        )
    ):
        return 2
    if re.fullmatch(r"[a-f0-9]{40}", options.previous_revision) is None:
        return 2
    if options.previous_revision == os.environ.get("GITHUB_SHA"):
        return 2
    mask = os.umask(0o077)
    handlers: dict[signal.Signals, object] = {}
    status = 1
    try:
        for number in (signal.SIGINT, signal.SIGTERM):
            handlers[number] = signal.signal(number, control.signal)
        observed = run(options, control)
        data = receive.encoded(observed)
        require(len(data) <= MAX_RECORD, "deploy_vm_summary_bound")
        write_private(options.output / "summary.json", data, mode=0o600)
        require(sys.stdout.buffer.write(data) == len(data), "deploy_vm_stdout")
        _ = sys.stdout.buffer.flush()
        status = 0 if observed["passed"] is True else 1
    except (OSError, ValueError, ToolError, RuntimeError):
        _ = sys.stderr.write("Signed musl host deploy VM proof is unproven.\n")
    finally:
        actions: list[Callable[[], None]] = []
        for number, handler in handlers.items():

            def restore(number: signal.Signals = number, handler: object = handler) -> None:
                """Retain the fixed restore role contract."""
                _ = signal.signal(
                    number, cast("int | Callable[[int, object], None] | None", handler)
                )

            actions.append(restore)

        def restore_mask() -> None:
            """Retain the fixed restore mask role contract."""
            _ = os.umask(mask)

        actions.append(restore_mask)
        if fetch_vm.settle(actions) is not None:
            status = 1
    try:
        control.check()
    except (OSError, ValueError, ToolError):
        status = 1
    return control.status or status


if __name__ == "__main__":
    raise SystemExit(main())
