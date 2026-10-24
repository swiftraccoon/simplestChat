"""Compose signed original inputs, real musl Stage and independent free-KVM proof.

The existing authenticated keepers own distribution/install and the VM. This
module adds only fixed Stage orchestration; it has no host/command/image selector.
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
import stat
import sys
import time
import tomllib
import uuid
from pathlib import Path
from typing import TYPE_CHECKING, NoReturn, cast, final, override

import security_vm as vm
import security_vm_fetch as fetch_vm
import security_vm_ops as ops_vm
import security_vm_ops_guest as ops_guest
import security_vm_receive as receive
import security_vm_stage_guest as probe
from security_context import ROOT, executable
from security_tools import ToolError, require, write_private

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping, Sequence
    from typing import BinaryIO

WORKFLOW = "Signed musl release stage KVM runtime"
MAX_RECORD = 131072
MINIMUM_WORK = 100
DIRECTORY_MODE = 0o700
PROTECTED_FILES = 4
SERVER_NAMES = frozenset(
    {
        "artifact.zip",
        "image.tar",
        "release.json",
        "outcome.json",
        "source.json",
        "sbom.spdx.json",
        "image-security.json",
        "runtime-proof.json",
        "vex.openvex.json",
        "release-predicate.json",
        "release-attestation.jsonl",
    }
)
NEW_PATHS = (
    "build/security_vm_stage.py",
    "build/security_vm_stage_guest.py",
    "ops/ansible/security-vm-stage.yml",
    ".github/workflows/release-stage-runtime.yml",
    "ops/ansible/tests/test_security_vm_stage.py",
)
SOURCE_INPUTS = tuple(
    dict.fromkeys(
        (
            *receive.SOURCE_INPUTS,
            *NEW_PATHS,
            "ops/ansible/tasks/docker.yml",
            "ops/ansible/tasks/runtime-binaries.yml",
            "ops/ansible/tasks/runtime-verify.yml",
            "ops/ansible/group_vars/benchmark_hosts.yml",
            "ops/ansible/templates/docker.sources.j2",
            "ops/ansible/templates/docker.preferences.j2",
            "ops/ansible/templates/containerd-runtime.conf.j2",
        )
    )
)


def admit(environment: Mapping[str, str]) -> None:
    """Only this exact manual free GitHub main workflow can start a real guest."""
    require(
        sys.platform == "linux"
        and environment.get("GITHUB_ACTIONS") == "true"
        and environment.get("RUNNER_ENVIRONMENT") == "github-hosted"
        and environment.get("GITHUB_REPOSITORY") == fetch_vm.REPOSITORY
        and environment.get("GITHUB_REF") == "refs/heads/main"
        and environment.get("GITHUB_WORKFLOW") == WORKFLOW,
        "stage_vm_disposable_ci",
    )


def budget(control: fetch_vm.Control, cap: int = 900) -> int:
    """Clip current auth/guest work to the original body, retaining a settlement tail."""
    control.check()
    seconds = min(cap, math.floor(control.body_end - time.monotonic() - 40))
    require(seconds >= MINIMUM_WORK, "stage_vm_remaining_budget")
    return seconds


@final
class ServerTree:
    """Hold the original eleven-member signed server namespace before current verification."""

    def __init__(self, path: Path, control: fetch_vm.Control) -> None:
        """Bind exact regular-directory identity/member set, not saved JSON authority."""
        self.path, self.control = path, control
        control.check()
        self.descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC)
        try:
            self.original = os.fstat(self.descriptor)
            require(
                path.resolve() == path
                and self.original.st_uid == os.getuid()
                and stat.S_IMODE(self.original.st_mode) == DIRECTORY_MODE,
                "stage_vm_server_directory",
            )
            self.check()
        except BaseException as error:
            failure = fetch_vm.settle((lambda: os.close(self.descriptor),), error)
            self.descriptor = -1
            if failure is not None:
                raise failure from None
            raise

    def check(self) -> None:
        """No member exchange or same-byte directory replacement is accepted."""
        self.control.check()
        require(
            self.descriptor >= 0
            and ops_guest.stamp(os.fstat(self.descriptor)) == ops_guest.stamp(self.original)
            and ops_guest.stamp(self.path.lstat()) == ops_guest.stamp(self.original)
            and frozenset(os.listdir(self.descriptor)) == SERVER_NAMES,  # noqa: PTH208
            "stage_vm_server_namespace",
        )

    def finish(self) -> None:
        """Attempt the original close even after a failed final namespace witness."""
        if self.descriptor < 0:
            return
        descriptor = self.descriptor
        first: BaseException | None = None
        try:
            self.check()
        except BaseException as error:  # noqa: BLE001 -- Original close is unconditional.
            first = error
        finally:
            first = fetch_vm.settle((lambda: os.close(descriptor),), first)
            self.descriptor = -1
        if first is not None:
            raise first


def check_server(tree: ServerTree, pins: Mapping[str, fetch_vm.Held]) -> None:
    """Fence the same eleven originals around crypto, transfer and candidate execution."""
    tree.check()
    require(frozenset(pins) == SERVER_NAMES, "stage_vm_server_members")
    for original in pins.values():
        original.check(tree.control)
    tree.check()


def current_server_metadata(  # noqa: PLR0913, PLR0917 -- Fixed role pins.
    host: receive.Host,
    tools: fetch_vm.NativeTools,
    selected: Mapping[str, object],
    attempt: int,
    pins: Mapping[str, fetch_vm.Held],
    credential: Mapping[str, str],
) -> dict[str, object]:
    """Two actual checked GH metadata roles precede cached four-subject crypto."""
    replies: list[dict[str, object]] = []
    endpoints = (
        f"repos/{fetch_vm.REPOSITORY}/actions/artifacts/{selected['artifactId']}",
        f"repos/{fetch_vm.REPOSITORY}/actions/runs/{selected['ciRunId']}",
    )
    for name, endpoint in zip(("artifact", "run"), endpoints, strict=True):
        tools.check(host.control)
        gh = host.fixed_tools["gh"]
        gh.check(host.control)
        _, data = host.command(
            "stage-api-" + name,
            [
                str(gh.path),
                "api",
                "--hostname",
                "github.com",
                "-H",
                "Accept: application/vnd.github+json",
                "-H",
                "X-GitHub-Api-Version: 2022-11-28",
                endpoint,
            ],
            credential,
            min(20, host.control.body_end - time.monotonic()),
            (0,),
        )
        gh.check(host.control)
        tools.check(host.control)
        replies.append(probe.record(data))
    artifact, run = replies
    association = cast("dict[str, object]", artifact.get("workflow_run"))
    require(
        type(artifact.get("id")) is int
        and artifact["id"] == selected["artifactId"]
        and artifact.get("expired") is False
        and artifact.get("name") == "simplestchat-production-" + str(selected["revision"])
        and type(artifact.get("size_in_bytes")) is int
        and artifact["size_in_bytes"] == pins["artifact.zip"].size
        and artifact.get("digest") == "sha256:" + pins["artifact.zip"].sha256
        and type(association.get("id")) is int
        and association["id"] == selected["ciRunId"]
        and association.get("head_sha") == selected["revision"]
        and type(association.get("repository_id")) is int
        and 0 < cast("int", association["repository_id"]) <= receive.SAFE
        and type(association.get("head_repository_id")) is int
        and association["repository_id"] == association["head_repository_id"],
        "stage_vm_current_artifact",
    )
    for key in ("repository", "head_repository"):
        repository = cast("dict[str, object]", run.get(key))
        require(
            isinstance(repository.get("full_name"), str)
            and str(repository["full_name"]).lower() == fetch_vm.REPOSITORY.lower(),
            "stage_vm_current_repository",
        )
    require(
        type(run.get("id")) is int
        and run["id"] == selected["ciRunId"]
        and type(run.get("run_attempt")) is int
        and run["run_attempt"] == attempt
        and run.get("head_branch") == "main"
        and run.get("head_sha") == selected["revision"]
        and run.get("name") == "CI"
        and run.get("path") == ".github/workflows/ci.yml"
        and run.get("event") == "push"
        and run.get("status") == "completed"
        and run.get("conclusion") == "success",
        "stage_vm_current_run",
    )
    return {
        "schemaVersion": 1,
        "repository": fetch_vm.REPOSITORY,
        "artifactId": selected["artifactId"],
        "buildRunId": selected["ciRunId"],
        "ciRunId": selected["ciRunId"],
        "runAttempt": attempt,
        "revision": selected["revision"],
        "artifactZipBytes": pins["artifact.zip"].size,
        "zipSha256": pins["artifact.zip"].sha256,
    }


def reverify_server(  # noqa: PLR0913, PLR0917 -- Fixed source/auth roles.
    host: receive.Host,
    tools: fetch_vm.NativeTools,
    binary: Path,
    tree: ServerTree,
    pins: Mapping[str, fetch_vm.Held],
    selected: Mapping[str, object],
    attempt: int,
    credential: Mapping[str, str],
) -> None:
    """Require current two-API identity then actual cached4 crypto on held originals."""
    check_server(tree, pins)
    metadata = current_server_metadata(host, tools, selected, attempt, pins, credential)
    check_server(tree, pins)
    n = budget(host.control)
    _, data = host.command(
        "authenticate-retained-server",
        [
            str(binary),
            "release",
            "attestation",
            "verify",
            "--directory",
            str(tree.path),
            "--selection-json",
            receive.encoded(metadata).decode().strip(),
            "--timeout-seconds",
            str(n),
            "--finalization-seconds",
            "60",
        ],
        credential,
        n + 10,
        (0,),
    )
    value = probe.exact(probe.record(data), set(metadata) | {"attestation"})
    require(
        all(value[key] == expected for key, expected in metadata.items()),
        "stage_vm_server_crypto_result",
    )
    proof = probe.exact(value["attestation"], {"predicate", "bundleSha256", "verificationSha256"})
    require(
        proof["predicate"] == receive.private_record(pins["release-predicate.json"].path)
        and proof["bundleSha256"] == pins["release-attestation.jsonl"].sha256,
        "stage_vm_server_crypto_binding",
    )
    hashes = probe.exact(
        proof["verificationSha256"],
        {"image.tar", "sbom.spdx.json", "runtime-proof.json", "vex.openvex.json"},
    )
    for sha in hashes.values():
        _ = probe.digest(sha)
    tools.check(host.control)
    check_server(tree, pins)


def guest_projection(data: bytes, request: Mapping[str, object]) -> dict[str, object]:
    """Project only exact fixed nonsecret facts bound to original controller input pins."""
    fields = {
        "schemaVersion",
        "scope",
        "runId",
        "revision",
        "passed",
        "firstExitStatus",
        "repeatExitStatus",
        "staged",
        "manifestSha256",
        "imageId",
        "nativeActorId",
        "nativeActorLifecycleObserved",
        "nativeValidatorIsolationIndependentlyInspected",
        "firstEventCount",
        "cumulativeEventCount",
        "repeatStagedOriginalUnchanged",
        "repeatSkippedLoadAndValidator",
        "migrationMapMatched",
        "knownContainersAbsent",
        "protectedFilesUnchanged",
        "workloadLockReacquired",
        "separateProbe",
        "commands",
    }
    value = probe.exact(probe.record(data), fields)
    require(
        type(value["schemaVersion"]) is int
        and value["schemaVersion"] == 1
        and value["scope"] == "SignedMuslStageGuest"
        and value["runId"] == request["runId"]
        and value["revision"] == request["revision"]
        and value["passed"] is True
        and type(value["firstExitStatus"]) is int
        and value["firstExitStatus"] == 0
        and type(value["repeatExitStatus"]) is int
        and value["repeatExitStatus"] == 0,
        "stage_vm_guest_result",
    )
    for field in (
        "nativeActorLifecycleObserved",
        "repeatStagedOriginalUnchanged",
        "repeatSkippedLoadAndValidator",
        "migrationMapMatched",
        "knownContainersAbsent",
        "workloadLockReacquired",
    ):
        require(value[field] is True, "stage_vm_guest_fact")
    require(
        value["nativeValidatorIsolationIndependentlyInspected"] is False,
        "stage_vm_isolation_qualification",
    )
    require(
        value["manifestSha256"] == cast("Mapping[str, object]", request["manifest"])["sha256"],
        "stage_vm_manifest_binding",
    )
    _ = probe.digest(value["nativeActorId"])
    require(
        isinstance(value["imageId"], str) and re.fullmatch("sha256:[a-f0-9]{64}", value["imageId"]),
        "stage_vm_image_id",
    )
    staged = probe.exact(value["staged"], {"sha256", "bytes", "device", "inode"})
    _ = probe.digest(staged["sha256"])
    for key in ("bytes", "device", "inode"):
        _ = probe.integer(staged[key], 1 if key == "bytes" else 0, receive.SAFE)
    for key in ("firstEventCount", "cumulativeEventCount"):
        _ = probe.integer(value[key], 4, 255)
    require(
        cast("int", value["cumulativeEventCount"]) >= cast("int", value["firstEventCount"]),
        "stage_vm_event_anchor",
    )
    _ = probe.integer(value["commands"], 1, 40)
    require(
        type(value["protectedFilesUnchanged"]) is int
        and value["protectedFilesUnchanged"] == PROTECTED_FILES,
        "stage_vm_protected_files",
    )
    separate = probe.exact(
        value["separateProbe"],
        {
            "passed",
            "isolationIndependentlyInspected",
            "migrationMapMatched",
            "exitStatus",
            "containerId",
        },
    )
    require(
        separate["passed"] is True
        and separate["isolationIndependentlyInspected"] is True
        and separate["migrationMapMatched"] is True
        and type(separate["exitStatus"]) is int
        and separate["exitStatus"] == 0,
        "stage_vm_separate_probe",
    )
    _ = probe.digest(separate["containerId"])
    require(separate["containerId"] != value["nativeActorId"], "stage_vm_distinct_probe")
    return value


def ssh(host: receive.Host, work: Path, port: int, arguments: Sequence[str]) -> list[str]:
    """Use the original SSH executable and generated exact enrolled guest only."""
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
                "/root/simplestchat-vm-fixture/security_vm_stage_guest.py",
                *arguments,
            ]
        ),
    ]


def run(  # noqa: C901, PLR0912, PLR0915 -- Ordered owners.
    options: receive.Options, control: fetch_vm.Control
) -> dict[str, object]:
    """Compose fixed existing owners, preserving every first failure and cleanup verdict."""
    report: dict[str, object] = {
        "schemaVersion": 1,
        "scope": "SignedMuslReleaseStageKvm",
        "passed": False,
        "cleanupPassed": False,
        "failure": "guard",
        "opsVerifierCalls": 0,
        "serverVerifierCalls": 0,
        "nativeValidatorIsolationIndependentlyInspected": False,
        "serverCurrentMetadataApiCalls": 0,
        "verifierCountsAreCompletedSuccessfulRolesOnly": True,
        "guestPrivateEvidenceRetained": False,
    }
    owners = fetch_vm.Owners()
    tools: fetch_vm.NativeTools | None = None
    ops_tree: receive.OpsTree | None = None
    server_tree: ServerTree | None = None
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
            "stage_vm_output",
        )
        require(os.environ.get("GH_TOKEN"), "stage_vm_token")
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
        require(isinstance(package, str), "stage_vm_source_package")
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
        report["serverVerifierCalls"] = 4
        selected_original = fetch_vm.Held.open(acquired / "selection.json", 16384, control)
        owners.inputs.append(selected_original)
        selected = fetch_vm.selection(selected_original.path, revision)
        require(selected["ciRunId"] == options.run_id, "stage_vm_same_run")
        server_tree = ServerTree(acquired / "artifact", control)
        pins: dict[str, fetch_vm.Held] = {}
        for name in sorted(SERVER_NAMES):
            original = fetch_vm.Held.open(
                server_tree.path / name,
                2 * 1024**3 if name in {"image.tar", "artifact.zip"} else 64 * 1024**2,
                control,
            )
            owners.inputs.append(original)
            pins[name] = original
        predicate = receive.private_record(pins["release-predicate.json"].path)
        receive.server_attempt(predicate, options.run_id, options.run_attempt)
        reverify_server(
            host, tools, binary, server_tree, pins, selected, options.run_attempt, credential
        )
        report["serverVerifierCalls"] = 8
        report["serverCurrentMetadataApiCalls"] = 2
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
        require(installed["passed"] is True, "stage_vm_ops_install")
        report["opsCases"], report["install"] = installed["cases"], installed["install"]
        inventory = receive.prepared_inventory(owners, work, port, run_id, revision, control)
        setup_started = time.monotonic()
        report["failure"] = "cold-docker-setup"
        check_server(server_tree, pins)
        source_vars = output / "stage-vars.json"
        write_private(
            source_vars,
            receive.encoded({"vm_stage_artifact_dir": str(server_tree.path)}),
            mode=0o600,
        )
        original_vars = fetch_vm.Held.open(source_vars, MAX_RECORD, control)
        owners.inputs.append(original_vars)
        _, data = host.run(
            "stage-docker-setup",
            [
                executable("ansible-playbook"),
                "-i",
                str(inventory),
                str(ROOT / "ops/ansible/security-vm-stage.yml"),
                "--extra-vars",
                "@" + str(source_vars),
            ],
            timeout=budget(control, 600),
        )
        _ = vm.recap(data, unchanged=False)
        report["coldDockerSetupSeconds"] = time.monotonic() - setup_started
        check_server(server_tree, pins)
        receive.check_ops(ops_tree, retained)
        tools.check(control)
        body = budget(control, 1640)
        require(body >= probe.MINIMUM_BODY, "stage_vm_two_trial_admission")
        expected = {
            "schemaVersion": 1,
            "runId": run_id,
            "revision": revision,
            "bodySeconds": body,
            "guestSha256": source["build/security_vm_stage_guest.py"].sha256,
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
            "manifest": {"sha256": pins["release.json"].sha256, "bytes": pins["release.json"].size},
            "archive": {"sha256": pins["image.tar"].sha256, "bytes": pins["image.tar"].size},
        }
        guest_request = output / "stage-guest-request.json"
        write_private(guest_request, receive.encoded(expected), mode=0o600)
        original_request = fetch_vm.Held.open(guest_request, MAX_RECORD, control)
        owners.inputs.append(original_request)
        report["failure"] = "native-stage-and-independent-observer"
        _, data = host.run(
            "stage-guest",
            ssh(host, work, port, ()),
            input_file=original_request.stream,
            timeout=body + 35,
        )
        original_request.check(control)
        report["stageProof"] = guest_projection(data, expected)
        check_server(server_tree, pins)
        receive.check_ops(ops_tree, retained)
        report["failure"] = None
    except BaseException as error:  # noqa: BLE001 -- Keep first; settle all.
        first = error
    finally:
        control.settling = True
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
                    "stage-private-evidence",
                    ssh(host, work, port, ("--private-evidence", run_id)),
                    timeout=min(20, control.end - time.monotonic()),
                )
                report["guestPrivateEvidenceRetained"] = True
            except BaseException as error:  # noqa: BLE001 -- Keep evidence failure; settle all.
                evidence_error = error
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
        if server_tree is not None:
            actions.append(server_tree.finish)
        cleanup = fetch_vm.settle(actions)
        owned_cleanup = owners.finish(control, report)
        cleanup = cleanup or owned_cleanup
        report["allOriginalInputStreamsClosed"] = all(
            original.stream.closed for original in owners.inputs
        )
        if not report["allOriginalInputStreamsClosed"]:
            cleanup = cleanup or ToolError("stage_vm_original_close")
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
        self.exit(2, "Stage VM artifact selectors are invalid.\n")


def main() -> int:
    """Return positive only after original VM, stdout, handler, umask and clock fences."""
    control = fetch_vm.Control()
    parser = Parser(allow_abbrev=False)
    for name in ("run-id", "run-attempt", "artifact-id"):
        _ = parser.add_argument("--" + name, required=True, type=int)
    _ = parser.add_argument("--output", required=True, type=Path)
    options = parser.parse_args(namespace=receive.Options())
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
        observed = run(options, control)
        data = receive.encoded(observed)
        require(len(data) <= MAX_RECORD, "stage_vm_summary_bound")
        write_private(options.output / "summary.json", data, mode=0o600)
        require(sys.stdout.buffer.write(data) == len(data), "stage_vm_stdout")
        _ = sys.stdout.buffer.flush()
        status = 0 if observed["passed"] is True else 1
    except (OSError, ValueError, ToolError, RuntimeError):
        _ = sys.stderr.write("Signed musl Stage VM proof is unproven.\n")
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
        if fetch_vm.settle(actions) is not None:
            status = 1
    try:
        control.check()
    except (OSError, ValueError, ToolError):
        status = 1
    return control.status or status


if __name__ == "__main__":
    raise SystemExit(main())
