#!/usr/bin/env python3
"""Build and exercise the native worker with finite, isolated local workloads.

Only preparation downloads authenticated tool/dependency archives. Every actual
compile/test run has no network, host mounts or container-engine socket. A run
owns one exactly labelled container and retains bounded logs plus an identity
receipt; a timeout, cleanup error or incomplete result is never a passing check.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import signal
import stat
import sys
import tempfile
import time
import uuid
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "ops/ansible/files"))

# isort: split
import bounded_process  # noqa: E402 -- Maintained flat helper used by installed operations tools too.
import security_codeql_resources as resources  # noqa: E402
from release_json import (  # noqa: E402
    JsonObject,
    array_value,
    decode_json,
    integer_value,
    object_value,
    string_value,
)
from security_tools import ToolError  # noqa: E402

if TYPE_CHECKING:
    from collections.abc import Generator, Sequence

FAMILIES = ("stun", "dtls", "sctp", "rtp", "rtcp", "codecs", "utils")
UNIT_FIXTURES = {
    "rtp-unit-packet1": "packet1.raw",
    "rtp-unit-packet2": "packet2.raw",
    "rtp-unit-packet3": "packet3.raw",
}
MODES = ("asan", "ubsan", "replay")
WORKER = "vendor/mediasoup-sys-0.17.0"
SOURCE_PATHS = (
    "deps/libwebrtc",
    "fbs",
    "fuzzer/include",
    "fuzzer/src",
    "mocks/include",
    "mocks/src",
    "include",
    "scripts",
    "src",
    "subprojects/packagefiles",
    "test/include",
    "test/src",
    "build.rs",
    "Cargo.toml",
    "meson.build",
    "meson_options.txt",
    "python-invoke-requirements.txt",
    "python-tools-requirements.txt",
    "tasks.py",
)
EXCLUDED = {"include/FBS", "scripts/node_modules"}
MIB = 1024**2
MAX_INPUT = 65536
MAX_CASES = 64
MAX_PROVENANCE = 1024
MAX_SOURCE_FILES = 20000
BUILD_SECONDS = 1200
PREPARE_SECONDS = 2400
MAX_LOG = 16 * MIB
EVENT = "NATIVE_SECURITY_RESULT "
BUILD_EVENT = "NATIVE_BUILD_RESULT "
MAX_BINARY = 512 * MIB
ELF_HEADER_BYTES = 64
TARGETS = {
    "asan": "mediasoup-worker-test-asan-address",
    "ubsan": "mediasoup-worker-test-asan-undefined",
    "replay": "mediasoup-worker-fuzzer",
}
INPUT_LABEL = "org.simplestchat.native-security.inputs"
RUN_LABEL = "org.simplestchat.native-security.run"


class SecurityError(RuntimeError):
    """A fixed failure code safe to retain without command output or secrets."""


def require(condition: object, code: str) -> None:
    """Fail closed with a stable diagnostic."""
    if not condition:
        raise SecurityError(code)


def read_regular(path: Path, limit: int) -> bytes:
    """Read a bounded regular file without following a final symlink."""
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        metadata = os.fstat(descriptor)
        require(stat.S_ISREG(metadata.st_mode), "input_not_regular")
        require(0 <= metadata.st_size <= limit, "input_size_exceeded")
        with os.fdopen(descriptor, "rb", closefd=False) as source:
            data = source.read(limit + 1)
        require(len(data) == metadata.st_size, "input_changed")
        return data
    finally:
        os.close(descriptor)


def new_file(path: Path, data: bytes) -> None:
    """Never overwrite an earlier run or follow an output symlink."""
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, "wb") as stream:
        _ = stream.write(data)


def write_json(path: Path, data: object) -> None:
    """Write a fresh deterministic evidence object."""
    new_file(path, (json.dumps(data, sort_keys=True, indent=2) + "\n").encode())


@dataclass(frozen=True, kw_only=True)
class Case:
    """One reviewed decoded corpus input and its fixed target family."""

    identifier: str
    family: str
    relative: str
    data: bytes
    digest: str


def corpus(root: Path = ROOT) -> tuple[Case, ...]:
    """Require complete family coverage, exact bytes and no unlisted corpus files."""
    directory = root / "security/native"
    manifest = object_value(decode_json(read_regular(directory / "corpus.json", MIB)))
    require(set(manifest) == {"schemaVersion", "description", "cases"}, "corpus_schema")
    require(integer_value(manifest["schemaVersion"]) == 1, "corpus_version")
    require(bool(string_value(manifest["description"])), "corpus_description")
    cases: list[Case] = []
    identifiers: set[str] = set()
    paths: set[str] = set()
    counts: dict[str, int] = dict.fromkeys(FAMILIES, 0)
    for value in array_value(manifest["cases"]):
        item = object_value(value)
        require(
            set(item) == {"id", "family", "path", "sha256", "bytes", "provenance"},
            "corpus_case_schema",
        )
        identifier, family = string_value(item["id"]), string_value(item["family"])
        relative, digest = string_value(item["path"]), string_value(item["sha256"])
        require(family in FAMILIES, "corpus_family")
        require(re.fullmatch(r"[a-z][a-z0-9-]{0,79}", identifier), "corpus_case_id")
        require(identifier not in identifiers and relative not in paths, "corpus_duplicate")
        require(
            re.fullmatch(r"corpus/" + family + r"/[a-z][a-z0-9-]{0,79}\.hex", relative),
            "corpus_path",
        )
        require(not (directory / "corpus" / family).is_symlink(), "corpus_symlink")
        encoded = read_regular(directory / relative, MAX_INPUT * 3)
        require(re.fullmatch(rb"(?:[0-9a-f]{2}[ \n]*)+", encoded), "corpus_encoding")
        decoded = bytes.fromhex(encoded.decode("ascii"))
        require(1 <= len(decoded) <= MAX_INPUT, "corpus_input_size")
        require(integer_value(item["bytes"]) == len(decoded), "corpus_size_mismatch")
        require(hashlib.sha256(decoded).hexdigest() == digest, "corpus_digest_mismatch")
        require(1 <= len(string_value(item["provenance"])) <= MAX_PROVENANCE, "corpus_provenance")
        counts[family] += 1
        require(counts[family] <= MAX_CASES, "corpus_case_count")
        identifiers.add(identifier)
        paths.add(relative)
        cases.append(
            Case(
                identifier=identifier, family=family, relative=relative, data=decoded, digest=digest
            )
        )
    require(all(counts.values()), "corpus_missing_family")
    require(identifiers >= UNIT_FIXTURES.keys(), "missing_native_unit_fixture")
    actual = {
        str(path.relative_to(directory))
        for path in (directory / "corpus").rglob("*")
        if not path.is_dir() or path.is_symlink()
    }
    require(paths == actual, "corpus_unlisted_file")
    return tuple(cases)


def source_files(root: Path) -> list[Path]:
    """Mirror the Rust build's maintained source allowlist, excluding generated trees."""
    worker = root / WORKER
    selected: list[Path] = []
    for relative in SOURCE_PATHS:
        path = worker / relative
        require(path.exists() and not path.is_symlink(), "missing_native_source")
        candidates = path.rglob("*") if path.is_dir() else (path,)
        for candidate in candidates:
            local = candidate.relative_to(worker)
            if any(
                str(local) == excluded or str(local).startswith(excluded + "/")
                for excluded in EXCLUDED
            ):
                continue
            require(not candidate.is_symlink(), "native_source_symlink")
            if candidate.is_dir():
                continue
            require(candidate.is_file(), "native_source_not_regular")
            selected.append(candidate)
    selected.extend(worker.glob("subprojects/*.wrap"))
    selected.extend(
        root / relative
        for relative in (
            "build/native-security.Dockerfile",
            "build/native_security.py",
            "build/security_codeql_resources.py",
            "build/security_tools.py",
            "build/install-openssl.sh",
            "build/pip-constraints.txt",
            "ops/ansible/files/bounded_process.py",
            "ops/ansible/files/release_json.py",
            "security/native/toolchain.json",
            "security/native/corpus.json",
        )
    )
    selected.extend(root / "security/native" / case.relative for case in corpus(root))
    require(1 <= len(selected) <= MAX_SOURCE_FILES, "native_source_count")
    return sorted(selected)


def inputs_digest(root: Path, destination: Path | None = None) -> str:
    """Bind every builder input; optionally copy only these bytes into a fresh context."""
    digest = hashlib.sha256()
    for path in source_files(root):
        relative = path.relative_to(root)
        data = read_regular(path, 32 * MIB)
        digest.update(str(relative).encode() + b"\0" + hashlib.sha256(data).digest())
        if destination is not None:
            target = destination / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            new_file(target, data)
            if path.stat().st_mode & 0o111:
                target.chmod(0o700)
    return digest.hexdigest()


def command(argv: Sequence[str], *, timeout: int = 30) -> bytes:
    """Run a bounded local command without shell interpolation or hidden failure."""
    status, output, _ = bounded_process.run(argv, limits=bounded_process.Limits(timeout=timeout))
    require(status == 0, "command_failed")
    return output


def engine_prefix(name: str) -> list[str]:
    """Select a local engine explicitly; reject ambient remote Docker contexts."""
    executable = shutil.which(name)
    require(executable is not None, "engine_missing")
    if executable is None:
        raise SecurityError("engine_missing")  # noqa: EM101 -- Fixed diagnostic.
    if name == "docker":
        sockets = (Path("/var/run/docker.sock"), Path.home() / ".docker/run/docker.sock")
        socket = next((path for path in sockets if path.exists()), None)
        require(socket is not None, "local_docker_socket_missing")
        return [executable, "--host", "unix://" + str(socket)]
    require(name == "podman", "unsupported_engine")
    if sys.platform != "darwin":
        return [executable, "--remote=false"]
    connections = array_value(
        decode_json(command([executable, "system", "connection", "list", "--format", "json"]))
    )
    matches = [
        object_value(row)
        for row in connections
        if object_value(row).get("Name") == "podman-machine-default"
    ]
    require(len(matches) == 1, "local_podman_machine_missing")
    uri = urlsplit(string_value(matches[0]["URI"]))
    require(
        uri.scheme == "ssh" and uri.hostname in {"127.0.0.1", "localhost", "::1"},
        "podman_machine_not_local",
    )
    return [executable, "--connection", "podman-machine-default"]


def native_architecture() -> str:
    """Use the runner's real ISA; sanitizer emulation cannot provide a native result."""
    architectures = {"x86_64": "amd64", "amd64": "amd64", "aarch64": "arm64", "arm64": "arm64"}
    machine = platform.machine()
    require(machine in architectures, "native_host_architecture_unsupported")
    return architectures[machine]


def sandbox_args(image: str, run_id: str, digest: str) -> list[str]:
    """No host paths, published ports, network, privileges or unbounded storage."""
    require(re.fullmatch(r"sha256:[0-9a-f]{64}", image), "immutable_image_required")
    require(re.fullmatch(r"[0-9a-f]{32}", run_id), "run_identity")
    budget = resources.detect()
    # Two GiB per compiler plus one GiB for Meson, the generator and runtime.
    memory = budget.workers * 2048 + 1024
    require(budget.query_ram_mib >= budget.workers * 2048, "native_insufficient_memory")
    return [
        "create",
        "--platform=linux/" + native_architecture(),
        "--rm",
        "--name",
        "simplestchat-native-security-" + run_id,
        "--label",
        RUN_LABEL + "=" + run_id,
        "--label",
        INPUT_LABEL + "=" + digest,
        "--network=none",
        "--log-driver=none",
        "--read-only",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges:true",
        "--user=65532:65532",
        f"--cpus={budget.workers}",
        f"--memory={memory}m",
        f"--memory-swap={memory}m",
        f"--env=MEDIASOUP_BUILD_JOBS={budget.workers}",
        "--pids-limit=256",
        "--ulimit=core=0:0",
        # Meson executes compiler sanity checks and the built tests here.
        # Docker tmpfs defaults to noexec unless execution is explicit.
        "--tmpfs=/work:rw,exec,nosuid,nodev,size=5g,mode=1777",
        "--tmpfs=/tmp:rw,noexec,nosuid,nodev,size=256m,mode=1777",
        image,
    ]


def checked_image(engine: Sequence[str], image: str, digest: str) -> JsonObject:
    """Require the exact local image and this source/tool/corpus snapshot."""
    require(re.fullmatch(r"sha256:[0-9a-f]{64}", image), "immutable_image_required")
    info = object_value(
        decode_json(command([*engine, "image", "inspect", "--format", "{{json .}}", image]))
    )
    require(canonical_image_id(string_value(info["Id"])) == image, "image_identity_mismatch")
    labels = object_value(object_value(info["Config"])["Labels"])
    require(labels.get(INPUT_LABEL) == digest, "image_inputs_mismatch")
    require(info.get("Os") == "linux", "native_linux_required")
    require(info.get("Architecture") == native_architecture(), "native_image_architecture_mismatch")
    return info


def canonical_image_id(value: str) -> str:
    """Normalize Docker/Podman's full content identifiers at the engine boundary."""
    digest = value.removeprefix("sha256:")
    require(re.fullmatch(r"[0-9a-f]{64}", digest), "invalid_engine_image_id")
    return "sha256:" + digest


def cleanup(engine: Sequence[str], run_id: str, image: str) -> None:
    """Stop/remove only exact full IDs whose fresh labels and image still match."""
    output = command(
        [
            *engine,
            "ps",
            "--all",
            "--no-trunc",
            "--filter",
            "label=" + RUN_LABEL + "=" + run_id,
            "--format",
            "{{.ID}}",
        ]
    )
    identifiers = output.decode("ascii").splitlines()
    require(len(identifiers) <= 1, "unexpected_owned_containers")
    for identifier in identifiers:
        require(re.fullmatch(r"[0-9a-f]{64}", identifier), "container_identity")
        info = object_value(
            decode_json(command([*engine, "inspect", "--format", "{{json .}}", identifier]))
        )
        labels = object_value(object_value(info["Config"])["Labels"])
        require(
            info.get("Id") == identifier
            and canonical_image_id(string_value(info["Image"])) == image
            and labels.get(RUN_LABEL) == run_id,
            "container_ownership_changed",
        )
        _ = command([*engine, "rm", "--force", identifier], timeout=30)


def capture(argv: Sequence[str], output: Path, phase: str, timeout: int) -> int:
    """Retain bounded separate streams before returning the real child status."""
    with (
        (output / (phase + ".stdout.log")).open("xb") as stdout,
        (output / (phase + ".stderr.log")).open("xb") as stderr,
    ):
        status, _, _ = bounded_process.run(
            argv,
            output=stdout,
            error=stderr,
            limits=bounded_process.Limits(timeout=timeout, stdout=MAX_LOG, stderr=MAX_LOG),
        )
    return status


def prepare(engine: Sequence[str], output: Path) -> dict[str, object]:
    """Build a reusable local tool image from only the authenticated source snapshot."""
    with tempfile.TemporaryDirectory(prefix="native-security-context-") as temporary:
        context = Path(temporary)
        digest = inputs_digest(ROOT, context)
        iid = output / "image.id"
        status = capture(
            [
                *engine,
                "build",
                "--platform=linux/" + native_architecture(),
                "--file",
                str(context / "build/native-security.Dockerfile"),
                "--label",
                INPUT_LABEL + "=" + digest,
                "--iidfile",
                str(iid),
                str(context),
            ],
            output,
            "prepare",
            PREPARE_SECONDS,
        )
        require(status == 0, "builder_failed")
        image = canonical_image_id(read_regular(iid, 80).decode("ascii").strip())
        info = checked_image(engine, image, digest)
    return {
        "schemaVersion": 1,
        "status": "passed",
        "imageId": image,
        "inputsSha256": digest,
        "architecture": info.get("Architecture"),
    }


@contextmanager
def owned_sandbox(
    options: Options, engine: Sequence[str], output: Path, digest: str
) -> Generator[tuple[str, str]]:
    """Keep only owned tmpfs alive between bounded compilation, copy and execution."""
    run_id = uuid.uuid4().hex
    write_json(
        output / "ownership.json",
        {"schemaVersion": 1, "runId": run_id, "imageId": options.image, "inputsSha256": digest},
    )
    try:
        args = sandbox_args(options.image, run_id, digest)
        identifier = (
            command(
                [
                    *engine,
                    *args[:-1],
                    "--entrypoint=/usr/bin/python3",
                    args[-1],
                    "/opt/check/build/native_security.py",
                    "_hold",
                ]
            )
            .decode("ascii")
            .strip()
        )
        require(re.fullmatch(r"[0-9a-f]{64}", identifier), "container_identity")
        _ = command([*engine, "start", identifier])
        yield identifier, run_id
    finally:
        cleanup(engine, run_id, options.image)


def worker_result(output: Path, phase: str, event: str) -> JsonObject:
    """Require exactly one bounded worker receipt from the selected execution."""
    lines = read_regular(output / (phase + ".stdout.log"), MAX_LOG).decode("utf-8").splitlines()
    results = [line.removeprefix(event) for line in lines if line.startswith(event)]
    require(len(results) == 1, "missing_worker_result")
    return object_value(decode_json(results[0]))


def binary_identity(path: Path) -> tuple[int, str]:
    """Stream only a bounded ordinary native ELF executable, rejecting changed bytes."""
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as source:
        metadata = os.fstat(source.fileno())
        require(stat.S_ISREG(metadata.st_mode), "compiled_binary_not_regular")
        require(ELF_HEADER_BYTES <= metadata.st_size <= MAX_BINARY, "compiled_binary_size")
        header = source.read(20)
        machine = {"amd64": 62, "arm64": 183}[native_architecture()]
        require(
            header[:6] == b"\x7fELF\x02\x01" and int.from_bytes(header[18:20], "little") == machine,
            "compiled_binary_architecture",
        )
        _ = source.seek(0)
        digest = hashlib.file_digest(source, "sha256").hexdigest()
        after = os.fstat(source.fileno())
        require(
            (after.st_size, after.st_mtime_ns) == (metadata.st_size, metadata.st_mtime_ns),
            "compiled_binary_changed",
        )
    return metadata.st_size, digest


def compiled_receipt(directory: Path, options: Options, digest: str) -> JsonObject:
    """Validate exact compiled input independently from successful test evidence."""
    require(directory.is_absolute() and directory.resolve() == directory, "compiled_path")
    require(
        {path.name for path in directory.iterdir()} == {"binary", "receipt.json"}, "compiled_files"
    )
    receipt = object_value(decode_json(read_regular(directory / "receipt.json", MIB)))
    require(
        set(receipt)
        == {
            "schemaVersion",
            "status",
            "mode",
            "imageId",
            "inputsSha256",
            "architecture",
            "binaryBytes",
            "binarySha256",
            "worker",
        }
        and receipt.get("schemaVersion") == 1
        and receipt.get("status") == "built"
        and receipt.get("mode") == options.mode
        and receipt.get("imageId") == options.image
        and receipt.get("inputsSha256") == digest
        and receipt.get("architecture") == native_architecture(),
        "compiled_receipt_mismatch",
    )
    size, checksum = binary_identity(directory / "binary")
    require(
        size == receipt["binaryBytes"] and checksum == receipt["binarySha256"],
        "compiled_binary_mismatch",
    )
    return receipt


def export_binary(engine: Sequence[str], identifier: str, mode: str, destination: Path) -> None:
    """Read one fixed tmpfs executable through bounded exec, without an engine mount copy."""
    with destination.open("xb") as output:
        status, _, _ = bounded_process.run(
            [*engine, "exec", identifier, "/usr/bin/cat", "/work/build/" + TARGETS[mode]],
            output=output,
            limits=bounded_process.Limits(timeout=120, stdout=MAX_BINARY, stderr=MIB),
        )
    require(status == 0, "compiled_export_failed")


def send_binary(engine: Sequence[str], identifier: str, path: Path, checksum: str) -> None:
    """Send at most one bounded binary to an unprivileged fixed-path receiver."""
    data = read_regular(path, MAX_BINARY)
    require(hashlib.sha256(data).hexdigest() == checksum, "compiled_transfer_changed")
    status, _, _ = bounded_process.run(
        [
            *engine,
            "exec",
            "--interactive",
            identifier,
            "/usr/bin/python3",
            "/opt/check/build/native_security.py",
            "_receive",
            "--compiled-sha256",
            checksum,
            "--compiled-bytes",
            str(len(data)),
        ],
        input_data=data,
        limits=bounded_process.Limits(timeout=120, stdout=MIB, stderr=MIB),
    )
    require(status == 0, "compiled_import_failed")


def receive_binary(options: Options) -> None:
    """Stream authenticated executable bytes only into this sandbox's private tmpfs."""
    require(sys.platform == "linux" and Path("/opt/worker").is_dir(), "worker_sandbox_required")
    require(ELF_HEADER_BYTES <= options.compiled_bytes <= MAX_BINARY, "compiled_binary_size")
    require(re.fullmatch(r"[0-9a-f]{64}", options.compiled_sha256), "compiled_binary_digest")
    path = Path("/work/native-program")
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o700)
    digest = hashlib.sha256()
    size = 0
    with os.fdopen(descriptor, "wb") as output:
        while chunk := sys.stdin.buffer.read(MIB):
            size += len(chunk)
            require(size <= options.compiled_bytes, "compiled_import_size")
            _ = output.write(chunk)
            digest.update(chunk)
    require(
        size == options.compiled_bytes and digest.hexdigest() == options.compiled_sha256,
        "compiled_import_mismatch",
    )


def compile_native(options: Options, engine: Sequence[str], output: Path) -> dict[str, object]:
    """Atomically publish one verified binary after complete build and exact cleanup."""
    directory = options.compiled_directory
    require(directory is not None, "compiled_directory_required")
    if directory is None:
        raise SecurityError("compiled_directory_required")  # noqa: EM101
    require(directory.is_absolute() and directory.resolve() == directory, "compiled_path")
    require(not directory.exists(), "compiled_already_exists")
    digest = inputs_digest(ROOT)
    _ = checked_image(engine, options.image, digest)
    with tempfile.TemporaryDirectory(prefix="native-compiled-", dir=directory.parent) as temporary:
        stage = Path(temporary) / "artifact"
        stage.mkdir(mode=0o700)
        with owned_sandbox(options, engine, output, digest) as (identifier, _):
            status = capture(
                [
                    *engine,
                    "exec",
                    identifier,
                    "/usr/bin/python3",
                    "/opt/check/build/native_security.py",
                    "_compile",
                    "--mode",
                    options.mode,
                ],
                output,
                "compile",
                BUILD_SECONDS,
            )
            require(status == 0, f"native_build_failed_exit_{status}")
            result = worker_result(output, "compile", BUILD_EVENT)
            require(
                result.get("status") == "built" and result.get("mode") == options.mode,
                "incomplete_build_result",
            )
            export_binary(engine, identifier, options.mode, stage / "binary")
            (stage / "binary").chmod(0o755)
            size, checksum = binary_identity(stage / "binary")
            require(
                size == result.get("binaryBytes") and checksum == result.get("binarySha256"),
                "compiled_copy_mismatch",
            )
        receipt: dict[str, object] = {
            "schemaVersion": 1,
            "status": "built",
            "mode": options.mode,
            "imageId": options.image,
            "inputsSha256": digest,
            "architecture": native_architecture(),
            "binaryBytes": size,
            "binarySha256": checksum,
            "worker": result,
        }
        write_json(stage / "receipt.json", receipt)
        _ = stage.rename(directory)
    return {**receipt, "cleanup": "verified"}


def run_native(options: Options, engine: Sequence[str], output: Path) -> dict[str, object]:
    """Execute the complete current suite, even when its exact compiled binary is reused."""
    digest = inputs_digest(ROOT)
    info = checked_image(engine, options.image, digest)
    arguments: list[str] = []
    directory = options.compiled_directory
    if directory is not None:
        receipt = compiled_receipt(directory, options, digest)
        arguments = ["--compiled-sha256", string_value(receipt["binarySha256"])]
    with owned_sandbox(options, engine, output, digest) as (identifier, run_id):
        if directory is not None:
            send_binary(engine, identifier, directory / "binary", arguments[1])
        status = capture(
            [
                *engine,
                "exec",
                identifier,
                "/usr/bin/python3",
                "/opt/check/build/native_security.py",
                "_worker",
                "--mode",
                options.mode,
                *arguments,
            ],
            output,
            "native",
            BUILD_SECONDS + 900,
        )
        require(status == 0, f"native_failed_exit_{status}")
        result = worker_result(output, "native", EVENT)
        expected = (
            [case.identifier for case in corpus()] if options.mode == "replay" else [options.mode]
        )
        require(
            result.get("status") == "passed"
            and result.get("mode") == options.mode
            and result.get("completed") == expected,
            "incomplete_worker_result",
        )
    return {
        "schemaVersion": 1,
        "status": "passed",
        "mode": options.mode,
        "runId": run_id,
        "imageId": options.image,
        "inputsSha256": digest,
        "architecture": info.get("Architecture"),
        "compiledReused": directory is not None,
        "worker": result,
        "cleanup": "verified",
    }


def worker_environment(family: str | None = None) -> dict[str, str]:
    """Allow only the prepared build environment and exactly one selected parser family."""
    names = (
        "PATH",
        "OPENSSL_DIR",
        "OPENSSL_STATIC",
        "PKG_CONFIG_PATH",
        "CC",
        "CXX",
        "PYTHON",
        "PYTHONNOUSERSITE",
        "PYTHONDONTWRITEBYTECODE",
        "PYTHONPATH",
        "PIP_CONFIG_FILE",
        "PIP_INDEX_URL",
        "PIP_CONSTRAINT",
        "MEDIASOUP_OUT_DIR",
        "MEDIASOUP_INSTALL_DIR",
        "BUILD_DIR",
        "MEDIASOUP_BUILD_JOBS",
        "ASAN_SYMBOLIZER_PATH",
    )
    env = {name: os.environ[name] for name in names if name in os.environ}
    env.update(
        {
            "MESON_ARGS": "--wrap-mode=nodownload",
            "NINJA": "/opt/native-tools/pip_meson_ninja/bin/ninja",
            "ASAN_OPTIONS": "halt_on_error=1:print_stacktrace=1:detect_leaks=1:symbolize=1:"
            + "detect_stack_use_after_return=1:strict_init_order=1:"
            + "check_initialization_order=1:detect_container_overflow=1",
            "UBSAN_OPTIONS": "halt_on_error=1:print_stacktrace=1",
        }
    )
    if family is not None:
        require(family in FAMILIES, "corpus_family")
        env["MS_FUZZ_" + family.upper()] = "1"
    return env


def worker_command(argv: Sequence[str], *, timeout: int, family: str | None = None) -> float:
    """Emit bounded diagnostics and fail on any sanitizer or child-process failure."""
    started = time.monotonic()
    status, _, _ = bounded_process.run(
        argv,
        cwd=Path("/work/worker"),
        env=worker_environment(family),
        output=sys.stdout.buffer,
        error=sys.stderr.buffer,
        limits=bounded_process.Limits(timeout=timeout, stdout=MAX_LOG // 2, stderr=MAX_LOG // 2),
    )
    require(status == 0, "worker_command_failed")
    return round(time.monotonic() - started, 3)


def replay_args(path: Path) -> list[str]:
    """Run one reviewed file once, with no mutation directory or generated corpus."""
    return [
        "/work/build/mediasoup-worker-fuzzer",
        "-timeout=5",
        "-rss_limit_mb=2048",
        "-max_len=65536",
        "-seed=1",
        "-error_exitcode=77",
        "-timeout_exitcode=78",
        str(path),
    ]


def worker_sources() -> tuple[Case, ...]:
    """Restore exact source and all reviewed test fixtures inside the sandbox."""
    require(sys.platform == "linux" and Path("/opt/worker").is_dir(), "worker_sandbox_required")
    cases = corpus()
    _ = shutil.copytree("/opt/worker", "/work/worker")
    data_directory = Path("/work/worker/test/data")
    data_directory.mkdir()
    for case in cases:
        if case.identifier in UNIT_FIXTURES:
            # Upstream readBinaryFile omits its final byte; preserve every reviewed byte.
            new_file(data_directory / UNIT_FIXTURES[case.identifier], case.data + b"\n")
    return cases


def compile_commands(mode: str, workers: int) -> dict[str, list[str]]:
    """Use the unchanged upstream sanitizer targets with bounded generator jobs too."""
    require(mode in MODES and 1 <= workers <= resources.MAX_WORKERS, "native_compile_options")
    meson = "/opt/native-tools/pip_meson_ninja/bin/meson"
    target = TARGETS[mode]
    sanitizer = "undefined" if mode == "ubsan" else "address"
    return {
        "configure": [
            meson,
            "setup",
            "--prefix",
            "/work/install",
            "--bindir",
            "",
            "--libdir",
            "",
            "--buildtype",
            "release",
            "-Db_ndebug=true",
            "--wrap-mode=nodownload",
            "-Dms_build_fuzzer=true" if mode == "replay" else "-Dms_build_tests=true",
            "-Db_sanitize=" + sanitizer,
            "-Db_lundef=false",
            "/work/build",
        ],
        "generator": [
            meson,
            "compile",
            "-C",
            "/work/build",
            "-j",
            str(workers),
            "flatbuffers-generator",
        ],
        "compile": [meson, "compile", "-C", "/work/build", "-j", str(workers), target],
        "install": [meson, "install", "-C", "/work/build", "--no-rebuild", "--tags", target],
    }


def worker_compile(mode: str) -> dict[str, object]:
    """Measure each complete build phase; never issue a successful test receipt here."""
    budget = resources.detect(os.environ.get("MEDIASOUP_BUILD_JOBS"))
    timings = {
        phase: worker_command(argv, timeout=BUILD_SECONDS)
        for phase, argv in compile_commands(mode, budget.workers).items()
    }
    size, digest = binary_identity(Path("/work/build") / TARGETS[mode])
    return {
        "status": "built",
        "mode": mode,
        "workers": budget.workers,
        "phaseSeconds": timings,
        "binaryBytes": size,
        "binarySha256": digest,
    }


def worker(options: Options) -> dict[str, object]:
    """Fresh complete sanitizer execution or every finite reviewed replay input."""
    started = time.monotonic()
    cases = worker_sources()
    build: dict[str, object] = {}
    binary = Path("/work/build") / TARGETS[options.mode]
    if options.compiled_sha256:
        binary = Path("/work/native-program")
        _, digest = binary_identity(binary)
        require(digest == options.compiled_sha256, "compiled_runtime_mismatch")
    else:
        build = worker_compile(options.mode)
    completed: list[str] = []
    test_started = time.monotonic()
    if options.mode in {"asan", "ubsan"}:
        _ = worker_command([str(binary), "--invisibles"], timeout=BUILD_SECONDS)
        completed.append(options.mode)
    else:
        directory = Path("/work/corpus")
        directory.mkdir()
        for case in cases:
            family_directory = directory / case.family
            family_directory.mkdir(exist_ok=True)
            new_file(family_directory / case.identifier, case.data)
        for case in cases:
            args = replay_args(directory / case.family / case.identifier)
            args[0] = str(binary)
            _ = worker_command(args, timeout=10, family=case.family)
            completed.append(case.identifier)
    return {
        "status": "passed",
        "mode": options.mode,
        "completed": completed,
        "elapsedSeconds": round(time.monotonic() - started, 3),
        "testSeconds": round(time.monotonic() - test_started, 3),
        "build": build,
        "llvmVersion": "23.1.2",
        "opensslInstrumented": False,
        "builderRpmSha256": hashlib.sha256(
            read_regular(ROOT / "builder-rpms.txt", MIB)
        ).hexdigest(),
    }


@dataclass
class Options(argparse.Namespace):
    """A closed local operation and finite native workload."""

    operation: str = "verify"
    engine: str = "docker"
    output: Path | None = None
    image: str = ""
    mode: str = "replay"
    compiled_directory: Path | None = None
    compiled_sha256: str = ""
    compiled_bytes: int = 0


def options_from(argv: Sequence[str] | None = None) -> Options:
    """Accept only preparation, sanitizers and fixed reviewed-file replay."""
    parser = argparse.ArgumentParser(description=__doc__)
    subs = parser.add_subparsers(dest="operation", required=True)
    _ = subs.add_parser("verify")
    _ = subs.add_parser("_hold")
    receiver = subs.add_parser("_receive")
    _ = receiver.add_argument("--compiled-sha256", required=True)
    _ = receiver.add_argument("--compiled-bytes", type=int, required=True)
    for operation in ("prepare", "run", "compile", "_worker", "_compile"):
        child = subs.add_parser(operation, argument_default=argparse.SUPPRESS)
        if operation not in {"_worker", "_compile"}:
            _ = child.add_argument("--engine", choices=("docker", "podman"), default="docker")
            _ = child.add_argument("--output", type=Path, required=True)
        if operation != "prepare":
            _ = child.add_argument("--mode", choices=MODES, required=True)
        if operation in {"run", "compile"}:
            _ = child.add_argument("--image", required=True)
            _ = child.add_argument(
                "--compiled-directory", type=Path, required=operation == "compile"
            )
        if operation == "_worker":
            _ = child.add_argument("--compiled-sha256", default="")
    return parser.parse_args(argv, namespace=Options())


def worker_operation(options: Options) -> None:
    """Dispatch only the closed internal sandbox operations accepted by the CLI."""
    if options.operation == "_hold":
        time.sleep(BUILD_SECONDS + 900)
    elif options.operation == "_receive":
        receive_binary(options)
    elif options.operation == "_compile":
        _ = worker_sources()
        print(BUILD_EVENT + json.dumps(worker_compile(options.mode), sort_keys=True))  # noqa: T201
    else:
        require(options.operation == "_worker", "worker_operation")
        print(EVENT + json.dumps(worker(options), sort_keys=True))  # noqa: T201


def main(argv: Sequence[str] | None = None) -> int:
    """Produce one bounded result and preserve failure evidence without retrying it."""
    options = options_from(argv)
    output = options.output
    created = False
    try:
        if options.operation == "verify":
            print(  # noqa: T201 -- CLI result.
                json.dumps(
                    {
                        "status": "passed",
                        "corpusCases": len(corpus()),
                        "families": list(FAMILIES),
                    }
                )
            )
            return 0
        if options.operation.startswith("_"):
            worker_operation(options)
            return 0
        output = fresh_output(output)
        created = True
        engine = engine_prefix(options.engine)
        if options.operation == "prepare":
            result = prepare(engine, output)
        elif options.operation == "compile":
            result = compile_native(options, engine, output)
        else:
            result = run_native(options, engine, output)
        write_json(output / "report.json", result)
        print(json.dumps(result, sort_keys=True))  # noqa: T201 -- Sanitized identity/result evidence.
    except (
        OSError,
        ValueError,
        KeyError,
        SecurityError,
        ToolError,
        bounded_process.ProcessError,
    ) as error:
        code = (
            str(error)
            if isinstance(error, (SecurityError, ToolError, bounded_process.ProcessError))
            else "invalid_native_security_input"
        )
        if created and output is not None and not (output / "report.json").exists():
            write_json(
                output / "report.json", {"schemaVersion": 1, "status": "failed", "error": code}
            )
        print("Native security check failed: " + code, file=sys.stderr)  # noqa: T201 -- Stable failure code.
        return 1
    else:
        return 0


def fresh_output(output: Path | None) -> Path:
    """Reserve a new private destination before producing any evidence."""
    if output is None or not output.is_absolute():
        raise SecurityError("absolute_output_required")  # noqa: EM101 -- Fixed diagnostic.
    output.mkdir(mode=0o700)
    return output


if __name__ == "__main__":

    def terminate(_signal: int, _frame: object) -> None:
        """Unwind exact-container cleanup when the owning workflow is cancelled."""
        raise KeyboardInterrupt

    _ = signal.signal(signal.SIGTERM, terminate)
    raise SystemExit(main())
