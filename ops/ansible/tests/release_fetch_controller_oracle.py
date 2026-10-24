"""Run both unchanged fetch mains through a closed, owned SSH/HTTP fixture.

The bridge executes the original receiver in one real child. Metadata, crypto,
HTTP, root identity and named failure observations are inert; protocol reads,
local verification, ZIP extraction, flock, file writes and publication are real.
No network, real SSH, GitHub, Docker or service command is permitted here.
"""

from __future__ import annotations

import argparse
import errno
import fcntl
import hashlib
import http.client
import io
import json
import os
import shutil
import signal
import ssl
import stat
import struct
import subprocess
import sys
import tarfile
import tempfile
from contextlib import ExitStack, chdir, redirect_stderr, redirect_stdout
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, cast, final
from unittest.mock import patch
from urllib.parse import urlsplit

import release_fresh_verify_oracle as fresh
import release_verify_oracle as cached
from test_release_fetch import envelope, fixture_files
from test_support import ROOT, obj

# isort: split
import bounded_process
import release_fetch_controller as controller
import release_fetch_receiver as receiver
import release_trust as trust
from release_json import JsonObject, JsonValue, decode_json

if TYPE_CHECKING:
    import socket
    from collections.abc import Mapping, Sequence
    from ssl import SSLContext
    from typing import BinaryIO, TextIO
    from urllib.request import Request


TODAY = "2050-01-01"
SECOND_READ = 2
DOWNLOAD_TIMEOUT = 15
CHUNK = receiver.CHUNK
MAX_OUTPUT = 262144
MAX_FIXTURE = 4 * 1024**2
REVISION = "a" * 40
TOKEN = b"INERT_FETCH_CONTROLLER_TOKEN"
LOCAL_GRANT = "https://inert-local.blob.core.windows.net/a.zip?sig=INERT_LOCAL_GRANT"
REMOTE_GRANT = "https://inert-remote.blob.core.windows.net/a.zip?sig=INERT_REMOTE_GRANT"
SCENARIOS = (
    "success-fresh",
    "success-cached",
    "repeat-fresh",
    "repeat-cached",
    "existing-identical-pair",
    "existing-identical-manifest",
    "existing-identical-image",
    "metadata-refusal",
    "signature-first",
    "signature-fourth",
    "cached-member-changed",
    "root-required",
    "root-mode",
    "config-mode",
    "release-dir-mode",
    "results-mode",
    "work-mode",
    "lock-busy",
    "work-journal-unfinished",
    "release-journal-unfinished",
    "disk-insufficient",
    "grant-wire-invalid-host",
    "grant-wire-fragment",
    "grant-wire-empty-query",
    "grant-wire-oversize",
    "grant-api-failure",
    "url-wait-alarm",
    "http-status",
    "http-ce-gzip",
    "http-ce-duplicate-first-valid",
    "http-ce-duplicate-first-invalid",
    "http-cl-missing",
    "http-cl-duplicate-first-valid",
    "http-cl-duplicate-first-invalid",
    "http-truncated",
    "http-overflow",
    "http-checksum",
    "http-read-fault",
    "http-write-fault",
    "http-flush-fault",
    "http-fsync-fault",
    "zip-directory-cap",
    "zip-duplicate",
    "zip-unsafe",
    "zip-missing-selected",
    "zip-crc",
    "zip-truncated",
    "zip-method",
    "source-mutated",
    "outcome-failed",
    "manifest-revision",
    "bundle-mutated",
    "runtime-expired",
    "runtime-utc-error",
    "existing-manifest-diff",
    "existing-image-diff",
    "existing-symlink",
    "first-link-error",
    "second-link-error",
    "race-eexist-equal",
    "race-eexist-diff",
    "receipt-write-error",
    "receipt-fsync-error",
    "final-missing",
    "final-truncated",
    "final-nonzero",
    "final-extra-output",
    "final-ascii-whitespace",
    "final-non-ascii-whitespace",
    "final-identity-mismatch",
    "receiver-interrupt",
    "receiver-alarm",
    "cached-empty-directory",
)
POSITIVE = {
    "success-fresh",
    "success-cached",
    "repeat-fresh",
    "repeat-cached",
    "existing-identical-pair",
    "existing-identical-manifest",
    "existing-identical-image",
    "http-ce-duplicate-first-valid",
    "http-cl-missing",
    "http-cl-duplicate-first-valid",
    "race-eexist-equal",
    "final-ascii-whitespace",
    "cached-empty-directory",
}
REFERENCE_PATHS = (
    "build/release_fetch_controller.py",
    "build/fetch-release.py",
    "build/verify-release.py",
    "build/release_attestation.py",
    "build/security_vm.py",
    "build/security_vm_release.py",
    "ops/ansible/files/release_fetch_receiver.py",
    "ops/ansible/files/fetch-release.py",
    "ops/ansible/files/release_json.py",
    "ops/ansible/files/release_trust.py",
    "ops/ansible/release.yml",
    "ops/ansible/tests/test_release_fetch.py",
    "ops/ansible/tests/test_release_fetch_controller.py",
    "ops/src/owned_process.rs",
    "ops/src/owned_process_cleanup.rs",
    "ops/src/distribution_install.rs",
    "ops/src/release/trust.rs",
    "ops/src/release/trust_receipts.rs",
    ".github/workflows/infrastructure-security.yml",
)
FIXTURE_PATHS = (
    "ops/ansible/tests/release_fresh_verify_oracle.py",
    "ops/ansible/tests/release_verify_oracle.py",
    "ops/ansible/tests/test_support.py",
    "ops/ansible/files/bounded_process.py",
    "ops/ansible/files/release_artifact.py",
)
PYTHON_ROOTS = (
    "build",
    "ops/ansible/callback_plugins",
    "ops/ansible/files",
    "ops/ansible/filter_plugins",
    "ops/ansible/tests",
    "xtask/src/ci/release_http_policy",
)
PYTHON_VENDOR_PATHS = (
    "vendor/mediasoup-sys-0.19.0/subprojects/packagefiles/libuv/link_file_in_build_dir.py",
    "vendor/mediasoup-sys-0.19.0/tasks.py",
)
OWN_PYTHON_PATHS = {
    "ops/ansible/tests/release_fetch_controller_oracle.py",
    "ops/ansible/tests/test_release_fetch_controller_oracle.py",
}


def digest(data: bytes) -> str:
    """Pin exact bytes without exposing their content."""
    return hashlib.sha256(data).hexdigest()


def encoded(value: JsonValue) -> bytes:
    """Encode bounded ordinary fixture JSON with one final LF."""
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def pin(path: Path) -> JsonObject:
    """Read only an owned fixture file and retain exact bytes, mode and digest."""
    metadata = path.lstat()
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > MAX_FIXTURE:
        message = "unexpected fixture member"
        raise ValueError(message)
    data = path.read_bytes()
    return {"bytes": len(data), "sha256": digest(data), "mode": stat.S_IMODE(metadata.st_mode)}


def inventory(path: Path) -> JsonObject:
    """Measure regular owned members without following symlinks."""
    result: JsonObject = {}
    for item in sorted(path.rglob("*")):
        if item.is_symlink():
            result[str(item.relative_to(path))] = {"kind": "symlink"}
        elif item.is_file():
            result[str(item.relative_to(path))] = pin(item)
    return result


def write(path: Path, data: bytes, mode: int = 0o600) -> None:
    """Create one test-owned file and explicitly close its original handle."""
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, mode)
    with os.fdopen(descriptor, "wb") as output:
        _ = output.write(data)
        output.flush()


def event(root: Path, role: str, **facts: JsonValue) -> None:
    """Append finite safe facts; credentials and complete grants are never events."""
    with (root / "events.jsonl").open("ab") as output:
        _ = output.write(encoded({"role": role, **facts}))


def source_pins() -> JsonObject:
    """Bind every original reference and actual imported fixture dependency."""
    return {name: pin(ROOT / name) for name in (*REFERENCE_PATHS, *FIXTURE_PATHS)}


def python_pins() -> JsonObject:
    """Pin maintained Python roots and two vendor references, excluding this pair."""
    paths = {path for name in PYTHON_ROOTS for path in (ROOT / name).rglob("*.py")}
    paths.update(ROOT / name for name in PYTHON_VENDOR_PATHS)
    return {
        str(path.relative_to(ROOT)): pin(path)
        for path in sorted(paths)
        if path.relative_to(ROOT).as_posix() not in OWN_PYTHON_PATHS
    }


@dataclass
class Fixture:
    """Retain bounded exact source bytes and fixed selected identities."""

    scenario: str
    files: dict[str, bytes]
    selection: JsonObject
    metadata: list[bytes]
    responses: list[bytes]
    branch: str
    repeat: int


def fixture(scenario: str) -> Fixture:
    """Construct a complete valid archive before applying one named observation."""
    if scenario not in SCENARIOS:
        message = "unknown closed fetch scenario"
        raise ValueError(message)
    files = fixture_files()
    image = io.BytesIO()
    with (
        tarfile.open(fileobj=io.BytesIO(files["image.tar"]), mode="r:") as old,
        tarfile.open(fileobj=image, mode="w:") as archive,
    ):
        for member in old:
            source = old.extractfile(member)
            if source is None:
                message = "fixture tar member is not a file"
                raise ValueError(message)
            with source:
                data = source.read()
            if member.name == "layer.tar":
                data = b"inert-layer\n" + b"\0" * (receiver.CHUNK + 8192)
            member.size = len(data)
            archive.addfile(member, io.BytesIO(data))
    files["image.tar"] = image.getvalue()
    for name in ("release.json", "image-security.json", "runtime-proof.json"):
        value = obj(decode_json(files[name]))
        value["archiveSha256"] = digest(files["image.tar"])
        files[name] = encoded(value)
    selected = envelope()
    del selected["attestation"]
    claim = cached.metadata_cases(
        "required" if scenario.startswith("runtime-") else "success", files, selected
    )
    if scenario.startswith("runtime-"):
        proof = obj(decode_json(files["runtime-proof.json"]))
        obj(proof, "sourceReview")["expires"] = "2050-01-02"
        files["runtime-proof.json"] = encoded(proof)
        claim = cached.metadata_cases("success", files, selected)
    zip_case = {
        "zip-unsafe": "zip-traversal",
        "zip-crc": "zip-selected-crc",
        "zip-method": "zip-method",
        "zip-duplicate": "zip-duplicate",
        "zip-missing-selected": "zip-missing",
    }.get(scenario, "zip-store")
    raw = cached.zip_bytes(files, zip_case)
    if scenario == "zip-directory-cap":
        data = bytearray(raw)
        struct.pack_into("<L", data, data.rfind(b"PK\x05\x06") + 12, receiver.CHUNK + 1)
        raw = bytes(data)
    if scenario == "zip-truncated":
        raw = raw[:-10]
    files["artifact.zip"] = raw
    selected["artifactZipBytes"], selected["zipSha256"] = len(raw), digest(raw)
    records = fresh.api_records(selected)
    if scenario == "metadata-refusal":
        records[0]["expired"] = True
    return Fixture(
        scenario,
        files,
        selected,
        [encoded(value) for value in records],
        [cached.response_bytes(claim, "success", index) for index in range(4)],
        "cached"
        if scenario.endswith("cached")
        or scenario in {"cached-member-changed", "cached-empty-directory"}
        else "fresh",
        2 if scenario.startswith("repeat-") else 1,
    )


def remote_wire(source: Fixture) -> bytes:
    """Build a real HTTP message; HTTPResponse supplies first-duplicate semantics."""
    scenario, body = source.scenario, source.files["artifact.zip"]
    status = "503 Unavailable" if scenario == "http-status" else "200 OK"
    headers = [f"Content-Length: {len(body)}", "Content-Encoding: identity"]
    if scenario == "http-cl-missing":
        headers = ["Content-Encoding: identity"]
    if scenario == "http-ce-gzip":
        headers[1] = "Content-Encoding: gzip"
    if scenario.startswith("http-ce-duplicate"):
        first, second = (
            ("identity", "gzip")
            if scenario.endswith("valid") and not scenario.endswith("invalid")
            else ("gzip", "identity")
        )
        headers[1:] = ["Content-Encoding: " + first, "Content-Encoding: " + second]
    if scenario.startswith("http-cl-duplicate"):
        first, second = (str(len(body)), "1")
        if scenario.endswith("invalid"):
            first, second = second, first
        headers[:1] = ["Content-Length: " + first, "Content-Length: " + second]
    if scenario == "http-truncated":
        body = body[: receiver.CHUNK + 3]
    if scenario == "http-overflow":
        headers = ["Content-Encoding: identity"]
        body += b"EXTRA"
    if scenario == "http-checksum":
        body = bytes([body[0] ^ 1]) + body[1:]
    return ("HTTP/1.1 " + status + "\r\n" + "\r\n".join(headers) + "\r\n\r\n").encode() + body


SSH_SOURCE = """from pathlib import Path
import sys
root = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(root / 'oracle-source'))
from release_fetch_controller_oracle import worker
raise SystemExit(worker(root, sys.argv[1:]))
"""


def populate(root: Path, source: Fixture) -> JsonObject:
    """Export pristine inputs and a no-network bridge before reference execution."""
    for name in ("candidate", "responses", "bin", "remote", "home", "config"):
        (root / name).mkdir(mode=0o700)
    for name, data in source.files.items():
        write(root / "candidate" / name, data)
    for index, data in enumerate((*source.metadata, TOKEN, *source.responses)):
        write(root / "responses" / f"process-{index:02}.bin", data)
    write(root / "responses" / "remote.http", remote_wire(source))
    write(root / "identity", b"INERT_NON_KEY_FIXTURE\n")
    write(root / "events.jsonl", b"")
    # Import via source path directly: the bridge never resolves an ambient ssh executable.
    bridge = SSH_SOURCE.replace(
        "sys.path.insert(0, str(root / 'oracle-source'))",
        f"sys.path.insert(0, {str(Path(__file__).parent)!r})",
    )
    write(root / "bin" / "ssh", (f"#!{sys.executable}\n" + bridge).encode(), 0o700)
    options = controller.FetchOptions(
        "owner/repo",
        123,
        REVISION,
        789,
        "inert.example",
        "root",
        str(root / "identity"),
        str(root / "controller"),
    )
    control: JsonObject = {
        "scenario": source.scenario,
        "selection": source.selection,
        "branch": source.branch,
        "repeat": source.repeat,
        "sshArguments": list[JsonValue](controller.ssh_arguments(options)[1:]),
        "sourceRoot": str(ROOT),
        "python": sys.executable,
        "today": TODAY,
    }
    write(root / "bridge.json", encoded(control))
    native_adapters(root, source)
    return control


def native_adapters(root: Path, source: Fixture) -> None:
    """Export fixed GH/curl observations for the native whole-controller consumer."""
    data = fresh.Fixture(
        "fresh-success",
        source.branch,
        source.files,
        source.selection,
        source.metadata,
        TOKEN,
        fresh.wire(302, [("Location", LOCAL_GRANT)], b"ignored-api-body"),
        fresh.wire(
            200,
            [("Content-Length", str(len(source.files["artifact.zip"])))],
            source.files["artifact.zip"],
        ),
        source.responses,
        [
            7
            if (source.scenario, index) in {("signature-first", 0), ("signature-fourth", 3)}
            else 0
            for index in range(4)
        ],
        source.repeat,
        "fixed",
        None,
    )
    control = fresh.adapter_control(data)
    control["scenario"] = source.scenario
    control["fullController"] = True
    write(root / "adapter.json", encoded(control))
    write(root / "responses" / "api-local.http", data.api_wire.data)
    write(
        root / "responses" / "api-remote.http",
        fresh.wire(302, [("Location", REMOTE_GRANT)], b"ignored-api-body").data,
    )
    write(root / "responses" / "blob.http", data.blob_wire.data)
    write(root / "native-events.jsonl", b"")
    adapter = (
        fresh.ADAPTER_SOURCE.replace(
            '        if CONTROL["branch"] == "cached":\n            return refused()\n', ""
        )
        .replace(
            '        wire = (ROOT / "responses/api.http").read_bytes()',
            """        journal = (ROOT / "native-events.jsonl").read_text()
        prior = [json.loads(line) for line in journal.splitlines()]
        local = CONTROL["branch"] == "fresh" and sum(
            row["role"] == "api-redirect" for row in prior) % 2 == 0
        name = "api-local.http" if local else "api-remote.http"
        wire = (ROOT / "responses" / name).read_bytes()""",
        )
        .replace(
            '    status = CONTROL["processStatuses"][index]',
            """    status = CONTROL["processStatuses"][index]
    if role == "auth-token" and CONTROL["scenario"] == "grant-api-failure":
        prior = [json.loads(line) for line in (ROOT / "native-events.jsonl").read_text()
                 .splitlines()]
        count = sum(row["role"] == "auth-token" for row in prior)
        if count == (0 if CONTROL["branch"] == "cached" else 1):
            status = 7""",
        )
        .replace(
            '    if program == "curl" and CONTROL["branch"] == "fresh":',
            '    if program == "curl":',
        )
    )
    executable = ("#!" + sys.executable + "\n" + adapter).encode()
    for name in ("gh", "curl"):
        write(root / "bin" / name, executable, 0o700)
    environment = dict(cached.environment(root))
    environment["GH_TOKEN"] = TOKEN.decode()
    write(root / "environment.json", encoded(dict[str, JsonValue](environment)))


@final
class RemoteResponse:
    """Retain an actual HTTPResponse and adapt only a closed read failure boundary."""

    def __init__(self, root: Path, scenario: str) -> None:
        """Retain the original owned fixture state before any observation."""
        data = (root / "responses" / "remote.http").read_bytes()
        self.pipe = fresh.RawPipe(fresh.Wire(data, data.index(b"\r\n\r\n") + 4))
        memory = cast("socket.socket", cast("object", fresh.RawSocket(self.pipe)))
        self.response = http.client.HTTPResponse(memory, method="GET")
        self.response.begin()
        self.scenario, self.root, self.reads = scenario, root, 0
        self.status, self.headers = self.response.status, self.response.headers

    def read(self, size: int = -1, /) -> bytes:
        """Delegate real HTTP framing; fail or signal after one accepted logical chunk."""
        self.reads += 1
        if self.reads == SECOND_READ and self.scenario in {
            "http-read-fault",
            "receiver-interrupt",
            "receiver-alarm",
        }:
            if self.scenario != "http-read-fault":
                handler = signal.getsignal(
                    signal.SIGALRM if self.scenario == "receiver-alarm" else signal.SIGTERM
                )
                if callable(handler):
                    handler(
                        signal.SIGALRM if self.scenario == "receiver-alarm" else signal.SIGTERM,
                        None,
                    )
            raise OSError(errno.EIO, "inert response read fault")
        return self.response.read(size)

    def close(self) -> None:
        """Close both original in-memory peers before retaining safe completion facts."""
        self.response.close()
        self.pipe.close()
        event(
            self.root,
            "remote-http-closed",
            reads=self.reads,
            responseClosed=self.response.isclosed(),
            pipeClosed=self.pipe.closed,
        )


@final
class Opener:
    """Return one parsed closed response without making a connection."""

    def __init__(self, root: Path, scenario: str) -> None:
        """Retain the original owned fixture state before any observation."""
        self.root, self.scenario = root, scenario

    def open(self, request: Request, *, timeout: int) -> RemoteResponse:
        """Verify credentialless validated request facts before parsing raw fixture bytes."""
        headers = dict(request.header_items())
        if (
            request.full_url != REMOTE_GRANT
            or timeout != DOWNLOAD_TIMEOUT
            or "Authorization" in headers
        ):
            message = "unexpected receiver HTTP request"
            raise ValueError(message)
        event(
            self.root,
            "remote-http",
            timeout=timeout,
            authorization=False,
            accept=headers.get("Accept"),
            identity=headers.get("Accept-encoding"),
        )
        return RemoteResponse(self.root, self.scenario)


@final
class Input:
    """Read both actual pipe messages without waiting for input EOF."""

    def __init__(self, root: Path, scenario: str) -> None:
        """Retain the original owned fixture state before any observation."""
        self.root, self.scenario, self.lines = root, scenario, 0

    def readline(self, size: int = -1, /) -> bytes:
        """Apply only the named lower wire observation after the real original read."""
        data = sys.stdin.buffer.readline(size)
        self.lines += 1
        event(
            self.root, "receiver-input", line=self.lines, bytes=len(data), lf=data.endswith(b"\n")
        )
        if self.lines != SECOND_READ:
            return data
        if self.scenario == "url-wait-alarm":
            handler = signal.getsignal(signal.SIGALRM)
            if callable(handler):
                handler(signal.SIGALRM, None)
        wrong = {
            "grant-wire-invalid-host": "https://invalid.example/a?inert=1",
            "grant-wire-fragment": REMOTE_GRANT + "#bad",
            "grant-wire-empty-query": "https://inert-remote.blob.core.windows.net/a.zip",
            "grant-wire-oversize": REMOTE_GRANT + "x" * receiver.LINE_BYTES,
        }.get(self.scenario)
        return encoded({"url": wrong}) if wrong is not None else data


@final
class Output:
    """Project an owned test root into the original fixed wire namespace."""

    def __init__(self, root: Path, scenario: str) -> None:
        """Retain the original owned fixture state before any observation."""
        self.root, self.scenario = root, scenario

    def write(self, text: str) -> int:
        """Retain original receipt privately and apply closed final-wire faults."""
        value = obj(decode_json(text))
        if value.get("status") == "ready":
            event(self.root, "receiver-ready")
        else:
            write(
                self.root
                / f"remote-final-{len(list(self.root.glob('remote-final-*.json'))):03}.json",
                encoded(value),
            )
            event(
                self.root,
                "receiver-final",
                passed=value.get("passed"),
                phase=value.get("phase"),
                failureClass=value.get("failureClass"),
            )
            evidence = value.get("evidence")
            if isinstance(evidence, str):
                value["evidence"] = (
                    "/srv/simplestchat-public/releases/" + REVISION + "/" + (Path(evidence).name)
                )
            if self.scenario == "final-identity-mismatch":
                value["artifactId"] = 124
            text = json.dumps(value) + "\n"
            if self.scenario == "final-missing":
                text = ""
            elif self.scenario == "final-truncated":
                text = text[: len(text) // 2]
            elif self.scenario == "final-extra-output":
                text += "unexpected\n"
            elif self.scenario == "final-ascii-whitespace":
                text += " \t\r\n\v\f"
            elif self.scenario == "final-non-ascii-whitespace":
                text += "\u00a0"
        _ = sys.stdout.write(text)
        return len(text)

    def flush(self) -> None:
        """Flush the actual worker pipe without delaying or manufacturing EOF."""
        _ = sys.stdout.flush()


def prepare_host(root: Path, source: Fixture) -> tuple[Path, Path, Path]:
    """Prepare real protected directories and named prior publication/journal fixtures."""
    public, config, work = (root / "remote" / name for name in ("public", "config", "work"))
    for path in (public, config, work, public / "releases", public / "results"):
        path.mkdir(mode=0o700, exist_ok=True)
    scenario = source.scenario
    wrong_mode = {
        "root-mode": public,
        "config-mode": config,
        "release-dir-mode": public / "releases",
        "results-mode": public / "results",
        "work-mode": work,
    }.get(scenario)
    if wrong_mode is not None:
        wrong_mode.chmod(0o755)
    if scenario in {"work-journal-unfinished", "release-journal-unfinished"}:
        target = (
            work / "current.json"
            if scenario.startswith("work-")
            else (public / "release-state.json")
        )
        write(target, encoded({"schemaVersion": 1, "finalized": False}))
    destination = public / "releases" / REVISION
    destination.mkdir(mode=0o700, exist_ok=True)
    names = {
        "existing-identical-pair": ("release.json", "image.tar"),
        "existing-identical-manifest": ("release.json",),
        "existing-identical-image": ("image.tar",),
        "existing-manifest-diff": ("release.json",),
        "existing-image-diff": ("image.tar",),
    }.get(scenario, ())
    for name in names:
        write(destination / name, b"different" if scenario.endswith("diff") else source.files[name])
    if scenario == "existing-symlink":
        (destination / "release.json").symlink_to(root / "candidate" / "release.json")
    return public, config, work


def worker(root: Path, arguments: list[str]) -> int:  # noqa: C901, PLR0915 -- Owned fault scope.
    """Run the actual unchanged receiver as the one real inert SSH child."""
    control = obj(decode_json((root / "bridge.json").read_bytes()))
    if arguments == ["-V"]:
        _ = sys.stderr.write("OpenSSH_10.2p1 INERT\n")
        return 0
    if arguments != control["sshArguments"] or any(
        key in os.environ for key in ("GH_TOKEN", "GITHUB_TOKEN")
    ):
        return 99
    scenario = str(control["scenario"])
    public, config, work = (root / "remote" / name for name in ("public", "config", "work"))
    event(root, "ssh-start", argumentsMatch=True, credentialEnvironmentAbsent=True)
    real_lstat, real_open, real_link = os.lstat, Path.open, os.link
    real_fdopen, real_fsync = os.open, os.fsync
    receipt_fd: int | None = None
    lock_checks: list[JsonValue] = []
    altered: set[str] = set()
    held_lock: BinaryIO | None = None

    def lstat(
        path: str | bytes | os.PathLike[str] | os.PathLike[bytes], *, dir_fd: int | None = None
    ) -> os.stat_result:
        metadata = real_lstat(path, dir_fd=dir_fd)
        if os.fsdecode(path).startswith(str(root / "remote")):
            values = list(metadata)
            values[4] = 0
            return os.stat_result(values)
        return metadata

    def opened(  # noqa: PLR0913, PLR0917 -- Mirror the actual Path.open signature.
        path: Path,
        mode: str = "r",
        buffering: int = -1,
        encoding: str | None = None,
        errors: str | None = None,
        newline: str | None = None,
    ) -> BinaryIO | TextIO | FaultFile:
        if (
            mode == "rb"
            and path.name in {"source.json", "outcome.json", "release.json", trust.BUNDLE}
            and path.name not in altered
            and path.parent.name.startswith("download.")
        ):
            selected = {
                "source-mutated": "source.json",
                "outcome-failed": "outcome.json",
                "manifest-revision": "release.json",
                "bundle-mutated": trust.BUNDLE,
            }.get(scenario)
            if path.name == selected:
                altered.add(path.name)
                with real_open(path, "rb") as original_input:
                    data = original_input.read()
                value = obj(decode_json(data))
                if selected == "source.json":
                    value["revision"] = "b" * 40
                elif selected == "outcome.json":
                    value["passed"] = False
                elif selected == "release.json":
                    value["revision"] = "b" * 40
                else:
                    value["inertMutation"] = True
                with real_open(path, "wb") as output:
                    _ = output.write(encoded(value))
                event(root, "remote-file-mutated", member=path.name)
        output = real_open(path, mode, buffering, encoding, errors, newline)
        if (
            path.name == "artifact.zip"
            and mode == "xb"
            and scenario in {"http-write-fault", "http-flush-fault"}
        ):
            return FaultFile(cast("BinaryIO", output), scenario)
        return cast("BinaryIO | TextIO", output)

    def descriptor_open(
        path: object, flags: int, mode: int = 0o600, *, dir_fd: int | None = None
    ) -> int:
        nonlocal receipt_fd
        if Path(cast("str", path)).name == "fetch-outcome.json":
            check = real_open(work / "workload.lock", "rb")
            with check:
                try:
                    fcntl.flock(check, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    locked = False
                except BlockingIOError:
                    locked = True
                lock_checks.append(locked)
            if scenario == "receipt-write-error":
                raise OSError(errno.EIO, "inert receipt create failure")
            receipt_fd = real_fdopen(cast("str", path), flags, mode, dir_fd=dir_fd)
            return receipt_fd
        return real_fdopen(cast("str", path), flags, mode, dir_fd=dir_fd)

    def synced(descriptor: int) -> None:
        nonlocal receipt_fd
        is_receipt = descriptor == receipt_fd
        if is_receipt:
            receipt_fd = None
        if is_receipt and scenario == "receipt-fsync-error":
            raise OSError(errno.EIO, "inert receipt fsync failure")
        if scenario == "http-fsync-fault":
            metadata = os.fstat(descriptor)
            for attempt in public.glob("releases/*/download.*"):
                path = attempt / "artifact.zip"
                if path.exists() and (metadata.st_dev, metadata.st_ino) == (
                    path.stat().st_dev,
                    path.stat().st_ino,
                ):
                    raise OSError(errno.EIO, "inert archive fsync failure")
        real_fsync(descriptor)

    def linked(source: Path, target: Path, *, follow_symlinks: bool) -> None:
        if scenario == "first-link-error" or (
            scenario == "second-link-error" and target.name == "image.tar"
        ):
            raise OSError(errno.EIO, "inert link failure")
        if scenario.startswith("race-eexist-") and target.name == "release.json":
            data = source.read_bytes() if scenario.endswith("equal") else b"different-racing-owner"
            write(target, data)
            raise FileExistsError(errno.EEXIST, "inert competing publication")
        real_link(source, target, follow_symlinks=follow_symlinks)

    cached.FixedDateTime.calls = 0
    cached.FixedDateTime.clock = "error" if scenario == "runtime-utc-error" else "fixed"
    if scenario == "runtime-expired":
        cached.FixedDateTime.clock = "late-expiry"
        cached.FixedDateTime.calls = 1
    if scenario == "lock-busy":
        if not (work / "workload.lock").exists():
            write(work / "workload.lock", b"")
        held_lock = real_open(work / "workload.lock", "rb")
        fcntl.flock(held_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)

    def no_tls(**_kwargs: object) -> None:
        return None

    def memory_opener(*_args: object) -> Opener:
        return Opener(root, scenario)

    def free_space(_path: object) -> fresh.Disk:
        return fresh.Disk(2**50, 0, 0 if scenario == "disk-insufficient" else 2**50)

    replacements: list[tuple[object, str, object]] = [
        (receiver, "ROOT", public),
        (receiver, "CONFIG", config),
        (receiver, "WORK", work),
        (os, "geteuid", lambda: 1 if scenario == "root-required" else 0),
        (os, "lstat", lstat),
        (os, "open", descriptor_open),
        (os, "fsync", synced),
        (os, "link", linked),
        (Path, "open", opened),
        (ssl, "create_default_context", no_tls),
        (receiver, "build_opener", memory_opener),
        (shutil, "disk_usage", free_space),
        (trust, "datetime", cached.FixedDateTime),
    ]
    try:
        with ExitStack() as stack:
            for module, name, replacement in replacements:
                _ = stack.enter_context(patch.object(module, name, replacement))
            output = cast("TextIO", cast("object", Output(root, scenario)))
            code = receiver.main(Input(root, scenario), output)
    finally:
        if held_lock is not None:
            held_lock.close()
    available = True
    if (work / "workload.lock").exists():
        with real_open(work / "workload.lock", "rb") as final_lock:
            try:
                fcntl.flock(final_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                available = False
    event(
        root,
        "ssh-finished",
        receiverCode=code,
        lockHeldAtReceipt=lock_checks,
        lockAvailableAfter=available,
    )
    return 7 if scenario == "final-nonzero" else code


@final
class FaultFile:
    """Delegate the real output file, including context close, around one named IO fault."""

    def __init__(self, original: BinaryIO, scenario: str) -> None:
        """Retain the original owned fixture state before any observation."""
        self.original, self.scenario = original, scenario

    def __enter__(self) -> FaultFile:
        """Return the same admitted output wrapper."""
        return self

    def __exit__(self, *arguments: object) -> None:
        """Close the original output even when a named IO operation failed."""
        del arguments
        self.original.close()

    def write(self, data: bytes) -> int:
        """Delegate the actual write or retain its named three-byte failure prefix."""
        if self.scenario == "http-write-fault":
            _ = self.original.write(data[:3])
            self.original.flush()
            raise OSError(errno.EIO, "inert three-byte write failure")
        return self.original.write(data)

    def flush(self) -> None:
        """Flush the same original output unless the closed flush fault is selected."""
        if self.scenario == "http-flush-fault":
            raise OSError(errno.EIO, "inert flush failure")
        self.original.flush()

    def fileno(self) -> int:
        """Return the original descriptor for the actual fsync boundary."""
        return self.original.fileno()


def arguments(root: Path, source: Fixture) -> list[str]:
    """Build only the original controller's explicit fully spelled options."""
    values = {
        "repository": "owner/repo",
        "artifact-id": "123",
        "revision": REVISION,
        "ci-run": "789",
        "host": "inert.example",
        "user": "root",
        "identity": str(root / "identity"),
        "output-parent": str(root / "controller"),
    }
    if source.branch == "cached":
        values["verified-directory"] = (
            "" if source.scenario == "cached-empty-directory" else (str(root / "candidate"))
        )
    return [item for name, value in values.items() for item in ("--" + name, value)]


@final
class Processes:
    """Record only closed external GH observations; retain each actual SSH Popen separately."""

    def __init__(self, root: Path, source: Fixture) -> None:
        """Retain the original owned fixture state before any observation."""
        self.root, self.source, self.tokens, self.verifiers = root, source, 0, 0
        self.children: list[subprocess.Popen[bytes]] = []
        self.popen = subprocess.Popen

    def metadata(
        self,
        arguments: Sequence[str],
        *,
        stdin: int,
        capture_output: bool,
        timeout: int,
        check: bool,
    ) -> subprocess.CompletedProcess[bytes]:
        """Adapt only the fixed external GH metadata and token roles."""
        args = list(arguments)
        if args[:2] == ["gh", "api"]:
            index = 0 if "/artifacts/" in args[-1] else 1
            data, role = (
                self.source.metadata[index],
                "metadata-artifact" if index == 0 else ("metadata-run"),
            )
        elif args == ["gh", "auth", "token", "--hostname", "github.com"]:
            self.tokens += 1
            data, role = TOKEN, "token"
            if self.source.scenario == "grant-api-failure" and self.tokens == (
                1 if self.source.branch == "cached" else 2
            ):
                raise subprocess.CalledProcessError(7, args, output=b"", stderr=b"")
        else:
            message = "unexpected oracle process"
            raise ValueError(message)
        event(
            self.root,
            role,
            stdinNull=stdin == subprocess.DEVNULL,
            captureOutput=capture_output,
            timeout=timeout,
            check=check,
        )
        return subprocess.CompletedProcess(args, 0, data, b"")

    def verify(
        self, arguments: Sequence[str], *, env: Mapping[str, str], limits: bounded_process.Limits
    ) -> tuple[int, bytes, bytes]:
        """Return controlled crypto output after observing the actual verifier invocation."""
        index = self.verifiers % 4
        self.verifiers += 1
        status = (
            7
            if (self.source.scenario, index) in {("signature-first", 0), ("signature-fourth", 3)}
            else 0
        )
        event(
            self.root,
            "verify",
            subject=Path(arguments[3]).name,
            timeout=limits.timeout,
            stdoutLimit=limits.stdout,
            stderrLimit=limits.stderr,
            status=status,
            unrelatedAbsent="UNRELATED" not in env,
        )
        return status, self.source.responses[index], b""

    def spawn(  # noqa: PLR0913 -- Mirror the actual Popen ownership boundary.
        self,
        arguments: Sequence[str],
        *,
        env: Mapping[str, str],
        stdin: int,
        stdout: int,
        stderr: int,
        start_new_session: bool,
    ) -> subprocess.Popen[bytes]:
        """Retain the actual original SSH child before reference framing begins."""
        child = self.popen(
            arguments,
            env=dict(env),
            stdin=stdin,
            stdout=stdout,
            stderr=stderr,
            start_new_session=start_new_session,
        )
        self.children.append(child)
        return child

    def finish(self) -> list[JsonValue]:
        """Settle every saved actual child and stream, independently of reference cleanup."""
        facts: list[JsonValue] = []
        for child in self.children:
            if child.poll() is None:
                child.terminate()
                try:
                    _ = child.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    child.kill()
                    _ = child.wait(timeout=2)
            for pipe in (child.stdin, child.stdout, child.stderr):
                if pipe is not None and not pipe.closed:
                    pipe.close()
            facts.append(
                {
                    "status": child.returncode,
                    "reaped": child.poll() is not None,
                    "pipesClosed": all(
                        value is None or value.closed
                        for value in (child.stdin, child.stdout, child.stderr)
                    ),
                }
            )
        return facts


@final
class LocalTransport:
    """Execute real HTTPResponse parsing for both local download roles."""

    def __init__(self, root: Path, source: Fixture) -> None:
        """Retain the original owned fixture state before any observation."""
        self.root, self.source, self.redirects = root, source, 0
        self.connections: list[fresh.Connection] = []

    def connect(self, host: str, *, timeout: int, context: SSLContext) -> fresh.Connection:
        """Construct a closed raw response while preserving mature HTTP parsing."""
        del context
        role = "api-redirect" if host == "api.github.com" else "blob"
        if role == "api-redirect":
            self.redirects += 1
            local = self.source.branch == "fresh" and self.redirects % 2 == 1
            grant = LOCAL_GRANT if local else REMOTE_GRANT
            data = fresh.wire(302, [("Location", grant)], b"ignored-api-body")
            event(self.root, "grant-local" if local else "grant-remote", timeout=timeout)
        else:
            if host != urlsplit(LOCAL_GRANT).netloc:
                message = "unexpected local download host"
                raise ValueError(message)
            data = fresh.wire(
                200,
                [("Content-Length", str(len(self.source.files["artifact.zip"])))],
                self.source.files["artifact.zip"],
            )
            event(self.root, "local-http", timeout=timeout)
        source = fresh.fixture("fresh-success")
        source.token = TOKEN
        connection = fresh.Connection(role, source, {"role": role}, fresh.RawPipe(data))
        self.connections.append(connection)
        return connection


def normalize(value: JsonValue, root: Path) -> JsonValue:
    """Preserve semantic journal facts while removing variable paths and clock samples."""
    if isinstance(value, dict):
        return {
            key: normalize(item, root)
            for key, item in value.items()
            if key not in {"elapsedSeconds", "startedAt"}
        }
    if isinstance(value, list):
        return [normalize(item, root) for item in value]
    if isinstance(value, str):
        result = value.replace(str(root), "$FIXTURE")
        for attempt in (root / "remote" / "public" / "releases" / REVISION).glob("download.*"):
            result = result.replace(attempt.name, "$ATTEMPT")
        for attempt in (root / "controller").glob("release-fetch.*"):
            result = result.replace(attempt.name, "$TRANSACTION")
        return result
    return value


def controller_journals(attempt: Path, files: JsonObject, root: Path) -> JsonObject:
    """Keep local JSON values and modes; retain exact pins when parsing fails."""
    journals: JsonObject = {}
    for name in ("artifact.json", "outcome.json"):
        path = attempt / name
        if name not in files:
            continue
        pinned = obj(files.pop(name))
        if path.is_symlink():
            journals[name] = pinned
            continue
        try:
            value = normalize(decode_json(path.read_bytes()), root)
        except ValueError:
            journals[name] = {"partial": pinned}
        else:
            journals[name] = {"mode": pinned["mode"], "value": value}
    return journals


def controller_evidence(root: Path) -> JsonObject:
    """Observe every local attempt, preserving journal semantics and complete file effects."""
    parent = root / "controller"
    if parent.is_symlink():
        message = "unexpected controller evidence parent"
        raise ValueError(message)
    if not parent.exists():
        return {"exists": False, "attempts": []}
    if not parent.is_dir():
        message = "unexpected controller evidence parent"
        raise ValueError(message)
    attempts: list[JsonValue] = []
    for attempt in sorted(parent.iterdir()):
        if (
            attempt.is_symlink()
            or not attempt.is_dir()
            or not attempt.name.startswith("release-fetch.")
        ):
            message = "unexpected controller attempt"
            raise ValueError(message)
        files = inventory(attempt)
        directories: JsonObject = {"": stat.S_IMODE(attempt.stat().st_mode)}
        for path in sorted(attempt.rglob("*")):
            if not path.is_symlink() and path.is_dir():
                directories[str(path.relative_to(attempt))] = stat.S_IMODE(path.stat().st_mode)
        journals = controller_journals(attempt, files, root)
        attempts.append({"files": files, "directories": directories, "journals": journals})
    attempts.sort(key=lambda value: json.dumps(value, sort_keys=True))
    return {"exists": True, "mode": stat.S_IMODE(parent.stat().st_mode), "attempts": attempts}


def plentiful_disk(_path: object) -> fresh.Disk:
    """Supply one inert capacity observation without replacing any filesystem work."""
    return fresh.Disk(2**50, 0, 2**50)


def run(scenario: str, root: Path, prepared: Fixture | None = None) -> JsonObject:  # noqa: C901, PLR0915 -- Ordered whole operation.
    """Run complete controller mains and close every saved process before observing verdicts."""
    source = fixture(scenario) if prepared is None else prepared
    before_sources = source_pins()
    before_python = python_pins()
    control = populate(root, source)
    _ = prepare_host(root, source)
    original = inventory(root / "candidate")
    if scenario == "cached-member-changed":
        _ = (root / "candidate" / "source.json").write_bytes(b"changed source")
    processes, transport = Processes(root, source), LocalTransport(root, source)
    stdout, stderr = io.StringIO(), io.StringIO()
    reports: list[JsonValue] = []
    previous_mask = os.umask(0o077)
    try:
        with ExitStack() as stack:
            stack.enter_context(
                patch.dict(
                    os.environ,
                    {
                        "PATH": str(root / "bin") + ":/usr/bin:/bin",
                        "HOME": str(root / "home"),
                        "USER": "inert",
                        "LOGNAME": "inert",
                        "GH_TOKEN": TOKEN.decode(),
                    },
                    clear=True,
                )
            )
            replacements: list[tuple[object, str, object]] = [
                (subprocess, "run", processes.metadata),
                (subprocess, "Popen", processes.spawn),
                (bounded_process, "run", processes.verify),
                (http.client, "HTTPSConnection", transport.connect),
                (shutil, "disk_usage", plentiful_disk),
                (trust, "datetime", cached.FixedDateTime),
                (controller, "datetime", cached.FixedDateTime),
            ]
            for module, name, replacement in replacements:
                _ = stack.enter_context(patch.object(module, name, replacement))
            cached.FixedDateTime.calls, cached.FixedDateTime.clock = 0, "fixed"
            with chdir(root / "candidate"), redirect_stdout(stdout), redirect_stderr(stderr):
                for _index in range(source.repeat):
                    code = controller.main(arguments(root, source))
                    reports.append({"code": code})
    finally:
        child_facts = processes.finish()
        _ = os.umask(previous_mask)
    output = stdout.getvalue().encode()
    if len(output) > MAX_OUTPUT or len(stderr.getvalue().encode()) > MAX_OUTPUT:
        message = "oracle diagnostic output exceeded closed bound"
        raise ValueError(message)
    parsed = [obj(decode_json(line)) for line in output.splitlines() if line]
    for result, report in zip(reports, parsed, strict=True):
        obj(result)["report"] = normalize(report, root)
    remote = root / "remote" / "public" / "releases" / REVISION
    attempts: list[JsonValue] = []
    for attempt in sorted(remote.glob("download.*")):
        files = inventory(attempt)
        journals: JsonObject = {}
        for name in ("envelope.json", "fetch-outcome.json"):
            path = attempt / name
            if path.exists():
                try:
                    journals[name] = normalize(decode_json(path.read_bytes()), root)
                except ValueError:
                    journals[name] = {"partial": True}
        linked: JsonObject = {}
        for name in ("release.json", "image.tar"):
            target, retained = remote / name, attempt / name
            linked[name] = bool(
                target.exists()
                and not target.is_symlink()
                and retained.exists()
                and (target.stat().st_dev, target.stat().st_ino)
                == (retained.stat().st_dev, retained.stat().st_ino)
            )
        attempts.append({"files": files, "journals": journals, "publicationLinks": linked})
    events = [decode_json(line) for line in (root / "events.jsonl").read_bytes().splitlines()]
    finals = sorted(root.glob("remote-final-*.json"))
    final_path = finals[-1] if finals else root / "missing-final.json"
    public_bytes = encoded({"reports": reports, "events": events})
    clean = all(
        value not in public_bytes for value in (TOKEN, b"INERT_LOCAL_GRANT", b"INERT_REMOTE_GRANT")
    )
    after_sources = source_pins()
    return {
        "schemaVersion": 1,
        "scenario": scenario,
        "input": {
            "selection": source.selection,
            "branch": source.branch,
            "repeat": source.repeat,
            "today": TODAY,
            "fault": scenario,
            "candidate": original,
            "nativeProfile": "canonical-cached-directory"
            if scenario == "cached-empty-directory"
            else None,
            "bridge": control,
        },
        "observation": {
            "passed": bool(parsed and all(value.get("passed") is True for value in parsed)),
            "runs": reports,
            "controllerEvidence": controller_evidence(root),
            "attempts": attempts,
            "publication": inventory(remote),
            "receiverFinal": normalize(decode_json(final_path.read_bytes()), root)
            if final_path.exists()
            else None,
            "events": events,
            "children": child_facts,
            "secretsAbsent": clean,
            "originalUnchanged": inventory(root / "candidate") == original,
            "sourceUnchanged": before_sources == after_sources,
            "originalPythonUnchanged": before_python == python_pins(),
            "originalPythonCount": len(before_python),
            "originalPythonClosureSha256": digest(encoded(before_python)),
            "sourcePins": after_sources,
            "stderrBytes": len(stderr.getvalue().encode()),
            "localConnections": [value.observation() for value in transport.connections],
        },
    }


def export_and_run(scenario: str, root: Path) -> JsonObject:
    """Keep pristine native inputs separate from the real reference's retained effects."""
    source = fixture(scenario)
    native = root / "native"
    native.mkdir(mode=0o700)
    _ = populate(native, source)
    _ = prepare_host(native, source)
    if scenario == "cached-member-changed":
        _ = (native / "candidate" / "source.json").write_bytes(b"changed source")
    reference = root / "reference"
    reference.mkdir(mode=0o700)
    value = run(scenario, reference, source)
    inputs = obj(value, "input")
    inputs["fixtureDirectory"] = "native"
    inputs["environment"] = "native/environment.json"
    inputs["selection"] = source.selection
    write(root / "source-python.json", encoded(python_pins()))
    return value


class Arguments(argparse.Namespace):
    """Keep the two closed CLI destinations typed."""

    scenario: str = ""
    fixture_directory: Path | None = None


def main(argv: list[str] | None = None) -> int:
    """Export pristine input before running one fixed bounded whole-operation scenario."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("--scenario", choices=SCENARIOS, required=True)
    _ = parser.add_argument("--fixture-directory", type=Path)
    args = parser.parse_args(argv, namespace=Arguments())
    directory = args.fixture_directory
    if directory is None:
        with tempfile.TemporaryDirectory(prefix="release-fetch-oracle.") as name:
            value = export_and_run(args.scenario, Path(name).resolve())
    else:
        if not directory.is_absolute() or directory.resolve() != directory or directory.exists():
            parser.error("fixture directory must be fresh canonical absolute")
        directory.mkdir(mode=0o700)
        value = export_and_run(args.scenario, directory)
    output = encoded(value)
    if len(output) > MAX_OUTPUT:
        message = "oracle observation exceeded closed bound"
        raise ValueError(message)
    _ = sys.stdout.buffer.write(output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
