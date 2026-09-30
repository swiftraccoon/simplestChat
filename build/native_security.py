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
import re
import shutil
import signal
import stat
import sys
import tempfile
import time
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "ops/ansible/files"))

# isort: split
import bounded_process  # noqa: E402 -- Maintained flat helper used by installed operations tools too.
from release_json import (  # noqa: E402
    JsonObject,
    array_value,
    decode_json,
    integer_value,
    object_value,
    string_value,
)

if TYPE_CHECKING:
    from collections.abc import Sequence

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


def sandbox_args(image: str, run_id: str, digest: str) -> list[str]:
    """No host paths, published ports, network, privileges or unbounded storage."""
    require(re.fullmatch(r"sha256:[0-9a-f]{64}", image), "immutable_image_required")
    require(re.fullmatch(r"[0-9a-f]{32}", run_id), "run_identity")
    return [
        "create",
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
        "--cpus=2",
        "--memory=6g",
        "--memory-swap=6g",
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


def run_native(options: Options, engine: Sequence[str], output: Path) -> dict[str, object]:
    """Run one mode and require complete worker results plus verified resource removal."""
    digest = inputs_digest(ROOT)
    info = checked_image(engine, options.image, digest)
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
                    *args,
                    "--mode",
                    options.mode,
                ]
            )
            .decode("ascii")
            .strip()
        )
        require(re.fullmatch(r"[0-9a-f]{64}", identifier), "container_identity")
        status = capture(
            [*engine, "start", "--attach", identifier], output, "native", BUILD_SECONDS + 900
        )
        require(status == 0, f"native_failed_exit_{status}")
        lines = read_regular(output / "native.stdout.log", MAX_LOG).decode("utf-8").splitlines()
        results = [line.removeprefix(EVENT) for line in lines if line.startswith(EVENT)]
        require(len(results) == 1, "missing_worker_result")
        result = object_value(decode_json(results[0]))
        expected = (
            [case.identifier for case in corpus()] if options.mode == "replay" else [options.mode]
        )
        require(
            result.get("status") == "passed"
            and result.get("mode") == options.mode
            and result.get("completed") == expected,
            "incomplete_worker_result",
        )
    finally:
        cleanup(engine, run_id, options.image)
    return {
        "schemaVersion": 1,
        "status": "passed",
        "mode": options.mode,
        "runId": run_id,
        "imageId": options.image,
        "inputsSha256": digest,
        "architecture": info.get("Architecture"),
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
            "ASAN_OPTIONS": "halt_on_error=1:detect_leaks=1:symbolize=1:"
            + "detect_stack_use_after_return=1",
            "UBSAN_OPTIONS": "halt_on_error=1:print_stacktrace=1",
        }
    )
    if family is not None:
        require(family in FAMILIES, "corpus_family")
        env["MS_FUZZ_" + family.upper()] = "1"
    return env


def worker_command(argv: Sequence[str], *, timeout: int, family: str | None = None) -> None:
    """Emit bounded diagnostics and fail on any sanitizer or child-process failure."""
    status, _, _ = bounded_process.run(
        argv,
        cwd=Path("/work/worker"),
        env=worker_environment(family),
        output=sys.stdout.buffer,
        error=sys.stderr.buffer,
        limits=bounded_process.Limits(timeout=timeout, stdout=MAX_LOG // 2, stderr=MAX_LOG // 2),
    )
    require(status == 0, "worker_command_failed")


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


def worker(options: Options) -> dict[str, object]:
    """Execute only inside the offline native sandbox; no remote target input exists."""
    require(sys.platform == "linux" and Path("/opt/worker").is_dir(), "worker_sandbox_required")
    started = time.monotonic()
    cases = corpus()
    _ = shutil.copytree("/opt/worker", "/work/worker")
    data_directory = Path("/work/worker/test/data")
    data_directory.mkdir()
    for case in cases:
        if case.identifier in UNIT_FIXTURES:
            # The unchanged upstream readBinaryFile helper intentionally omits
            # the file's final byte. Supply its terminator separately, keeping
            # the reviewed packet bytes identical to finite parser replay.
            new_file(data_directory / UNIT_FIXTURES[case.identifier], case.data + b"\n")
    task = {
        "asan": "test-asan-address",
        "ubsan": "test-asan-undefined",
        "replay": "fuzzer",
    }[options.mode]
    worker_command(
        ["/usr/bin/python3", "-m", "invoke", "--search-root", "/work/worker", task],
        timeout=BUILD_SECONDS,
    )
    completed: list[str] = []
    if options.mode in {"asan", "ubsan"}:
        completed.append(options.mode)
    else:
        directory = Path("/work/corpus")
        directory.mkdir()
        for case in cases:
            family_directory = directory / case.family
            family_directory.mkdir(exist_ok=True)
            new_file(family_directory / case.identifier, case.data)
        for case in cases:
            worker_command(
                replay_args(directory / case.family / case.identifier),
                timeout=10,
                family=case.family,
            )
            completed.append(case.identifier)
    return {
        "status": "passed",
        "mode": options.mode,
        "completed": completed,
        "elapsedSeconds": round(time.monotonic() - started, 3),
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


def options_from(argv: Sequence[str] | None = None) -> Options:
    """Accept only preparation, sanitizers and fixed reviewed-file replay."""
    parser = argparse.ArgumentParser(description=__doc__)
    subs = parser.add_subparsers(dest="operation", required=True)
    _ = subs.add_parser("verify")
    for operation in ("prepare", "run", "_worker"):
        child = subs.add_parser(operation, argument_default=argparse.SUPPRESS)
        if operation != "_worker":
            _ = child.add_argument("--engine", choices=("docker", "podman"), default="docker")
            _ = child.add_argument("--output", type=Path, required=True)
        if operation != "prepare":
            _ = child.add_argument("--mode", choices=MODES, required=True)
        if operation == "run":
            _ = child.add_argument("--image", required=True)
    return parser.parse_args(argv, namespace=Options())


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
        if options.operation == "_worker":
            print(EVENT + json.dumps(worker(options), sort_keys=True))  # noqa: T201 -- Closed worker receipt.
            return 0
        output = fresh_output(output)
        created = True
        engine = engine_prefix(options.engine)
        result = (
            prepare(engine, output)
            if options.operation == "prepare"
            else run_native(options, engine, output)
        )
        write_json(output / "report.json", result)
        print(json.dumps(result, sort_keys=True))  # noqa: T201 -- Sanitized identity/result evidence.
    except (OSError, ValueError, KeyError, SecurityError, bounded_process.ProcessError) as error:
        code = (
            str(error)
            if isinstance(error, (SecurityError, bounded_process.ProcessError))
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
