"""Run the unchanged complete local release CLI with closed inert wire observations.

Actual metadata policy, stdlib HTTP framing, streaming files, ZIP extraction,
cached verification and late guards remain in the reference path. Only external
process/HTTP/clock/disk observations are adapted. Tokens and signed grants are
explicit inert fixture bytes and never enter observation JSON or command argv.
"""

from __future__ import annotations

import argparse
import hashlib
import http.client
import io
import json
import os
import runpy
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import zipfile
from contextlib import chdir, redirect_stderr, redirect_stdout
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, NamedTuple, cast, final, override
from unittest.mock import patch
from urllib.parse import urlsplit

import release_verify_oracle as cached
from test_release_fetch import envelope, fixture_files
from test_support import ROOT, obj

# isort: split
import bounded_process
import release_attestation as attest
import release_fetch_controller as fetch
import release_trust as trust
from release_json import JsonObject, JsonValue, decode_json

if TYPE_CHECKING:
    import socket
    from collections.abc import Mapping, Sequence
    from ssl import SSLContext


TODAY = "2050-01-01"
MAX_OUTPUT_BYTES = 131072
MAX_FIXTURE_BYTES = 2 * 1024**2
CHUNK_BYTES = 65536
LAST_VERIFICATION = 3
TOKEN_PROCESS_INDEX = 2
VERIFICATION_PROCESS_START = 3
INERT_TOKEN = b"INERT_GH_TOKEN_FOR_LOCAL_WIRE"
GRANT = "https://inert-release.blob.core.windows.net/path/artifact.zip?sig=INERT_SIGNED_GRANT"
SCENARIOS = (
    "fresh-success",
    "cached-success",
    "fresh-repeat",
    "cached-repeat",
    "run-attempt-large",
    "identifiers-max",
    "api-artifact-id",
    "api-artifact-expired",
    "api-artifact-expired-integer",
    "api-artifact-name",
    "api-size-zero",
    "api-size-boolean",
    "api-digest-missing",
    "api-digest-uppercase",
    "api-association-run",
    "api-association-revision",
    "api-association-fork",
    "api-association-boolean-id",
    "api-run-id",
    "api-run-branch",
    "api-run-attempt-zero",
    "api-run-attempt-boolean",
    "api-run-attempt-max-plus-one",
    "api-run-status",
    "api-run-conclusion",
    "api-run-event",
    "api-run-name",
    "api-run-path",
    "api-run-repository",
    "api-run-head-repository",
    "api-run-revision",
    "api-repository-case",
    "api-command-failure",
    "api-json-malformed",
    "token-command-failure",
    "token-empty",
    "token-spaced-positive",
    "token-control",
    "token-invalid-utf8",
    "token-too-large",
    "token-non-ascii",
    "redirect-status301",
    "redirect-status200",
    "redirect-location-missing",
    "redirect-location-duplicate",
    "redirect-location-leading-space",
    "redirect-location-trailing-space",
    "redirect-location-folded",
    "redirect-hostcase",
    "redirect-userinfo",
    "redirect-port",
    "redirect-fragment",
    "redirect-http",
    "redirect-query-missing",
    "redirect-path-percent",
    "redirect-empty-fragment",
    "redirect-multiple-fragment",
    "redirect-100",
    "redirect-103",
    "redirect-body-ignored",
    "blob-status302",
    "blob-idle-gap",
    "blob-deadline",
    "blob-content-encoding-gzip",
    "blob-content-encoding-case",
    "blob-content-encoding-duplicate",
    "blob-length-missing",
    "blob-length-wrong",
    "blob-length-duplicate",
    "blob-length-leading-space",
    "blob-length-trailing-space",
    "blob-length-folded",
    "blob-100",
    "blob-103",
    "blob-truncated",
    "blob-extra-with-length",
    "blob-extra-no-length",
    "blob-digest",
    "blob-read-fault",
    "blob-partial-truncated",
    "blob-partial-extra",
    "zip-crc",
    "zip-unsafe-name",
    "cached-member-changed",
    "cached-zip-changed",
    "gh-verifier-failure-first",
    "gh-verifier-failure-last",
    "late-source",
    "late-outcome",
    "late-manifest",
    "required",
    "optional-utc-error",
    "late-utc-error",
    "late-expiry",
    "disk-insufficient",
    "output-parent-relative",
    "output-parent-symlink",
    "cached-directory-symlink",
    "selector-artifact-plus",
    "selector-ci-underscores",
    "cached-output-parent-relative",
    "cached-directory-relative",
    "artifact-dir-empty",
)
PROFILE = {
    "blob-idle-gap": "blob-socket-idle-domain",
    "cached-directory-symlink": "canonical-absolute-nofollow-directory",
    "cached-directory-relative": "canonical-absolute-nofollow-directory",
    "selector-artifact-plus": "ascii-positive-decimal-selectors",
    "selector-ci-underscores": "ascii-positive-decimal-selectors",
}


def digest(data: bytes) -> str:
    """Pin original fixture bytes without exposing private payload text."""
    return hashlib.sha256(data).hexdigest()


def encoded(value: JsonValue) -> bytes:
    """Retain ordinary fixture JSON bytes."""
    return json.dumps(value, ensure_ascii=True, sort_keys=True).encode()


def write(path: Path, data: bytes, mode: int = 0o600) -> None:
    """Create a private original fixture without replacing any existing evidence."""
    with path.open("xb") as handle:
        _ = handle.write(data)
    path.chmod(mode)


@dataclass
class Wire:
    """One bounded raw HTTP response, including its physical body start."""

    data: bytes
    body_offset: int


def wire(status: int, headers: list[tuple[str, str]], body: bytes, interim: int = 0) -> Wire:
    """Preserve duplicate/folded/trailing header bytes for the actual stdlib parser."""
    prefix = b"" if not interim else f"HTTP/1.1 {interim} Interim\r\n\r\n".encode()
    head = (
        f"HTTP/1.1 {status} Fixture\r\n"
        + "".join(f"{name}: {value}\r\n" for name, value in headers)
        + "\r\n"
    )
    data = prefix + head.encode("ascii")
    return Wire(data + body, len(data))


@dataclass
class Fixture:
    """Closed source data and scripted external observations for the real CLI."""

    scenario: str
    branch: str
    files: dict[str, bytes]
    selection: JsonObject
    metadata: list[bytes]
    token: bytes
    api_wire: Wire
    blob_wire: Wire
    verifications: list[bytes]
    statuses: list[int]
    repeat: int
    clock: str
    profile: str | None


def api_records(selection: JsonObject) -> list[JsonObject]:
    """Build exact public API identities matching selected original artifact bytes."""
    repository, revision = selection["repository"], selection["revision"]
    run_id = selection["ciRunId"]
    return [
        {
            "id": selection["artifactId"],
            "expired": False,
            "name": f"simplestchat-production-{revision}",
            "size_in_bytes": selection["artifactZipBytes"],
            "digest": "sha256:" + str(selection["zipSha256"]),
            "workflow_run": {
                "id": run_id,
                "head_sha": revision,
                "repository_id": 42,
                "head_repository_id": 42,
            },
        },
        {
            "id": run_id,
            "head_sha": revision,
            "status": "completed",
            "conclusion": "success",
            "repository": {"full_name": repository},
            "head_repository": {"full_name": repository},
            "name": "CI",
            "path": ".github/workflows/ci.yml",
            "event": "push",
            "head_branch": "main",
            "run_attempt": selection["runAttempt"],
        },
    ]


def mutate_metadata(scenario: str, records: list[JsonObject]) -> None:
    """Apply one named API mutation without replacing actual relationship policy."""
    artifact, run = records
    artifact_fields: dict[str, tuple[str, JsonValue]] = {
        "api-artifact-id": ("id", 124),
        "api-artifact-expired": ("expired", True),
        "api-artifact-expired-integer": ("expired", 0),
        "api-artifact-name": ("name", "wrong"),
        "api-size-zero": ("size_in_bytes", 0),
        "api-size-boolean": ("size_in_bytes", True),
        "api-digest-missing": ("digest", None),
        "api-digest-uppercase": ("digest", "sha256:" + "A" * 64),
    }
    association_fields: dict[str, tuple[str, JsonValue]] = {
        "api-association-run": ("id", 790),
        "api-association-revision": ("head_sha", "b" * 40),
        "api-association-fork": ("head_repository_id", 43),
        "api-association-boolean-id": ("repository_id", True),
    }
    run_fields: dict[str, tuple[str, JsonValue]] = {
        "api-run-id": ("id", 790),
        "api-run-branch": ("head_branch", "feature"),
        "api-run-attempt-zero": ("run_attempt", 0),
        "api-run-attempt-boolean": ("run_attempt", True),
        "api-run-attempt-max-plus-one": ("run_attempt", 2**53),
        "api-run-status": ("status", "in_progress"),
        "api-run-conclusion": ("conclusion", "failure"),
        "api-run-event": ("event", "pull_request"),
        "api-run-name": ("name", "Other"),
        "api-run-path": ("path", ".github/workflows/other.yml"),
        "api-run-repository": ("repository", {"full_name": "other/repo"}),
        "api-run-head-repository": ("head_repository", {"full_name": "other/repo"}),
        "api-run-revision": ("head_sha", "b" * 40),
    }
    for target, changes in (
        (artifact, artifact_fields),
        (obj(artifact, "workflow_run"), association_fields),
        (run, run_fields),
    ):
        if scenario in changes:
            key, value = changes[scenario]
            target[key] = value
    if scenario == "api-repository-case":
        run["repository"] = run["head_repository"] = {"full_name": "OWNER/REPO"}


def redirect_wire(scenario: str) -> Wire:
    """Build raw API framing and URL cases while retaining an unread poison body."""
    locations = {
        "redirect-location-leading-space": "  " + GRANT,
        "redirect-location-trailing-space": GRANT + " ",
        "redirect-location-folded": GRANT + "\r\n folded",
        "redirect-hostcase": GRANT.replace("inert-release", "INERT-release"),
        "redirect-userinfo": GRANT.replace("https://", "https://user@"),
        "redirect-port": GRANT.replace(".net/", ".net:443/"),
        "redirect-fragment": GRANT + "#fragment",
        "redirect-empty-fragment": GRANT + "#",
        "redirect-multiple-fragment": GRANT + "##",
        "redirect-http": GRANT.replace("https://", "http://"),
        "redirect-query-missing": GRANT.split("?", maxsplit=1)[0],
        "redirect-path-percent": GRANT.replace("/path/", "/a%2Fb/%2e%2e/%25/"),
    }
    headers = [("Location", locations.get(scenario, GRANT)), ("Content-Length", "31")]
    if scenario == "redirect-location-missing":
        headers = headers[1:]
    if scenario == "redirect-location-duplicate":
        headers.append(("lOcAtIoN", GRANT))
    return wire(
        {"redirect-status301": 301, "redirect-status200": 200}.get(scenario, 302),
        headers,
        b"INERT_API_BODY_MUST_STAY_UNREAD",
        100 if scenario == "redirect-100" else 103 if scenario == "redirect-103" else 0,
    )


def blob_wire(scenario: str, archive: bytes) -> Wire:
    """Build actual raw framing; declared length intentionally governs extra-body cases."""
    size = str(len(archive))
    lengths = {
        "blob-length-wrong": "1",
        "blob-length-leading-space": "  " + size,
        "blob-length-trailing-space": size + " ",
        "blob-length-folded": size + "\r\n ",
    }
    headers = [("Content-Length", lengths.get(scenario, size)), ("Content-Encoding", "identity")]
    if scenario in {"blob-length-missing", "blob-extra-no-length", "blob-partial-extra"}:
        headers = headers[1:]
    if scenario == "blob-length-duplicate":
        headers.append(("content-length", size))
    encodings = {"blob-content-encoding-gzip": "gzip", "blob-content-encoding-case": "Identity"}
    if scenario in encodings:
        headers = [
            (key, encodings[scenario] if key == "Content-Encoding" else value)
            for key, value in headers
        ]
    if scenario == "blob-content-encoding-duplicate":
        headers.append(("content-encoding", "identity"))
    body = archive
    if scenario in {"blob-extra-with-length", "blob-extra-no-length", "blob-partial-extra"}:
        body += b"extra"
    if scenario == "blob-truncated":
        body = body[:-5]
    if scenario == "blob-partial-truncated":
        body = body[:65539]
    if scenario == "blob-digest":
        body = b"X" + body[1:]
    return wire(
        302 if scenario == "blob-status302" else 200,
        headers,
        body,
        100 if scenario == "blob-100" else 103 if scenario == "blob-103" else 0,
    )


def fixture(scenario: str) -> Fixture:
    """Prepare bounded source bytes before any unchanged CLI work runs."""
    if scenario not in SCENARIOS:
        message = "unknown closed fresh-release scenario"
        raise ValueError(message)
    files = fixture_files()
    files["sbom.spdx.json"] = b"opaque-inert-sbom\n" * 8192
    security = obj(decode_json(files["image-security.json"]))
    security["sbomSha256"] = digest(files["sbom.spdx.json"])
    files["image-security.json"] = encoded(security)
    selected = envelope()
    del selected["attestation"]
    base = {
        "run-attempt-large": "attempt-large",
        "zip-crc": "zip-selected-crc",
        "zip-unsafe-name": "zip-traversal",
        "late-outcome": "late-outcome-error",
        "late-manifest": "late-manifest-date",
    }.get(scenario, scenario)
    claim = cached.metadata_cases(base, files, selected)
    archive = cached.zip_bytes(
        files, "zip-store" if base not in {"zip-selected-crc", "zip-traversal"} else base
    )
    files["artifact.zip"] = archive
    selected["artifactZipBytes"], selected["zipSha256"] = len(archive), digest(archive)
    records = api_records(selected)
    mutate_metadata(scenario, records)
    token = {
        "token-empty": b"",
        "token-spaced-positive": b" \t" + INERT_TOKEN + b"\n",
        "token-control": b"INERT\nTOKEN",
        "token-invalid-utf8": b"\xff",
        "token-too-large": b"T" * 4097,
        "token-non-ascii": "inert-é".encode(),
    }.get(scenario, INERT_TOKEN)
    metadata = [encoded(value) for value in records]
    if scenario == "api-json-malformed":
        metadata[0] = b"not JSON"
    branch = "cached" if scenario.startswith("cached-") else "fresh"
    if scenario.startswith("selector-"):
        branch = "fresh"
    return Fixture(
        scenario,
        branch,
        files,
        selected,
        metadata,
        token,
        redirect_wire(scenario),
        blob_wire(scenario, archive),
        [cached.response_bytes(claim, base, i) for i in range(4)],
        [
            7
            if (scenario == "gh-verifier-failure-first" and i == 0)
            or (scenario == "gh-verifier-failure-last" and i == LAST_VERIFICATION)
            else 0
            for i in range(4)
        ],
        2 if scenario.endswith("repeat") else 1,
        {
            "optional-utc-error": "error",
            "late-utc-error": "late-error",
            "late-expiry": "late-expiry",
            "blob-deadline": "download-deadline",
        }.get(scenario, "fixed"),
        PROFILE.get(scenario),
    )


class RawPipe(io.BytesIO):
    """Memory-only original response bytes with observed physical body consumption."""

    def __init__(self, response: Wire, *, read_fault: bool = False, idle_gap: bool = False) -> None:
        """Retain the wire body boundary before the mature parser consumes any bytes."""
        super().__init__(response.data)
        self.body_offset: int = response.body_offset
        self.body_bytes: int = 0
        self.body_reads: int = 0
        self.position_at_close: int = 0
        self.read_fault: bool = read_fault
        self.idle_gap: bool = idle_gap
        self.close_attempts: int = 0

    @override
    def read(self, size: int | None = -1) -> bytes:
        """Only a named lower-I/O fault interrupts an actual reference download loop."""
        before = self.tell()
        if self.idle_gap and before >= self.body_offset + CHUNK_BYTES:
            message = "inert configured socket idle timeout after sixteen seconds"
            raise TimeoutError(message)
        if self.read_fault and before >= self.body_offset + CHUNK_BYTES:
            message = "inert response read fault"
            raise OSError(message)
        data = super().read(size)
        consumed = max(0, self.tell() - max(before, self.body_offset))
        self.body_bytes += consumed
        if before >= self.body_offset:
            self.body_reads += 1
        return data

    @override
    def close(self) -> None:
        """Retain observations before memory-only peer closure."""
        self.close_attempts += 1
        if not self.closed:
            self.position_at_close = self.tell()
        super().close()


@final
class RawSocket:
    """Supply only HTTPResponse's makefile interface, never an operating-system socket."""

    def __init__(self, stream: RawPipe) -> None:
        """Keep the original in-memory stream rather than reconstructing parsed headers."""
        self.stream = stream

    def makefile(self, _mode: str) -> RawPipe:
        """Return the original wire cursor to stdlib HTTPResponse."""
        return self.stream


@dataclass
class Connection:
    """Adapt a connection boundary while the original HTTPResponse parses its raw wire."""

    role: str
    source: Fixture
    row: JsonObject
    stream: RawPipe
    response: http.client.HTTPResponse | None = None
    closes: int = 0

    def request(self, method: str, target: str, *, headers: Mapping[str, str]) -> None:
        """Record safe request facts; credentials and signed query values remain in memory."""
        parsed = urlsplit(target)
        token = self.source.token.decode(errors="replace").strip()
        facts: JsonObject = {
            "method": method,
            "targetPath": parsed.path,
            "queryPresent": bool(parsed.query),
            "targetSha256": digest(target.encode()),
            "targetBytes": len(target.encode()),
            "headerNames": list[JsonValue](sorted(headers)),
            "authorizationPresent": "Authorization" in headers,
            "authorizationMatchesInert": headers.get("Authorization") == "Bearer " + token,
            "acceptMatches": headers.get("Accept")
            == (
                "application/vnd.github+json" if self.role == "api-redirect" else "application/zip"
            ),
            "explicitIdentityEncodingRequested": headers.get("Accept-Encoding") == "identity",
        }
        self.row.update(facts)

    def getresponse(self) -> http.client.HTTPResponse:
        """Use actual stdlib status/header parsing, including its 100-only skip loop."""
        # HTTPResponse's concrete socket annotation is narrower than its makefile-only use.
        memory_socket = cast("socket.socket", cast("object", RawSocket(self.stream)))
        self.response = http.client.HTTPResponse(memory_socket, method="GET")
        self.response.begin()
        self.row["responseStatus"] = self.response.status
        return self.response

    def close(self) -> None:
        """Attempt all original in-memory peers even after header/parser/read refusal."""
        self.closes += 1
        if self.response is not None:
            self.response.close()
        if not self.stream.closed:
            self.stream.close()

    def observation(self) -> JsonObject:
        """Finalize bounded facts without retaining raw headers, tokens or grant text."""
        self.row.update(
            {
                "connectionCloses": self.closes,
                "responseClosed": self.response is None or self.response.isclosed(),
                "pipeClosed": self.stream.closed,
                "pipeCloseAttempts": self.stream.close_attempts,
                "bodyBytesRead": self.stream.body_bytes,
                "bodyReads": self.stream.body_reads,
                "physicalBytesConsumed": self.stream.position_at_close,
            }
        )
        return self.row


@final
class Transport:
    """Own a closed API/blob sequence; a cached call cannot acquire either role."""

    def __init__(self, source: Fixture) -> None:
        """Retain only fixed fixture data and original connection observations."""
        self.source = source
        self.connections: list[Connection] = []

    def connect(self, host: str, *, timeout: int, context: SSLContext) -> Connection:
        """Construct a memory connection, with no real socket or TLS handshake."""
        del context
        if self.source.branch == "cached":
            message = "cached branch attempted HTTP"
            raise AssertionError(message)
        role = "api-redirect" if host == "api.github.com" else "blob"
        expected = "api.github.com" if role == "api-redirect" else urlsplit(GRANT).netloc
        if host != expected:
            message = "unexpected closed HTTP host"
            raise AssertionError(message)
        response = self.source.api_wire if role == "api-redirect" else self.source.blob_wire
        row: JsonObject = {
            "role": role,
            "timeoutSeconds": timeout,
            "rawResponseSha256": digest(response.data),
        }
        value = Connection(
            role,
            self.source,
            row,
            RawPipe(
                response,
                read_fault=role == "blob" and self.source.scenario == "blob-read-fault",
                idle_gap=role == "blob" and self.source.scenario == "blob-idle-gap",
            ),
        )
        self.connections.append(value)
        return value

    def monotonic(self) -> float:
        """Advance the original download clock only after its first accepted 64KiB prefix."""
        if self.source.scenario == "blob-deadline" and any(
            value.role == "blob" and value.stream.body_bytes > CHUNK_BYTES
            for value in self.connections
        ):
            return 241.0
        return 0.0


@final
class Processes:
    """Adapt external gh calls while retaining the complete CLI/policy orchestration."""

    def __init__(self, source: Fixture, root: Path) -> None:
        """Keep metadata and verifier invocation observations separate."""
        self.source = source
        self.root = root
        self.api_calls: list[JsonValue] = []
        self.verifier_calls: list[JsonValue] = []

    def metadata(
        self,
        arguments: Sequence[str],
        *,
        stdin: int,
        capture_output: bool,
        timeout: int,
        check: bool,
    ) -> subprocess.CompletedProcess[bytes]:
        """Return only closed gh API/token responses; every other program refuses."""
        args = list(arguments)
        if args[:2] == ["gh", "auth"]:
            if self.source.branch == "cached":
                message = "cached branch attempted token acquisition"
                raise AssertionError(message)
            role, data = "auth-token", self.source.token
        elif args[:2] == ["gh", "api"]:
            role = "api-artifact" if "/artifacts/" in args[-1] else "api-run"
            data = self.source.metadata[0 if role == "api-artifact" else 1]
        else:
            message = "unexpected external program in fresh-release oracle"
            raise AssertionError(message)
        status = (
            7
            if (
                (self.source.scenario == "api-command-failure" and role == "api-artifact")
                or (self.source.scenario == "token-command-failure" and role == "auth-token")
            )
            else 0
        )
        facts: JsonObject = {
            "role": role,
            "arguments": list[JsonValue](args),
            "timeoutSeconds": timeout,
            "stdinNull": stdin == subprocess.DEVNULL,
            "captureOutput": capture_output,
            "check": check,
            "status": status,
            "responseBytes": len(data),
            "responseSha256": digest(data),
        }
        self.api_calls.append(facts)
        if status and check:
            raise subprocess.CalledProcessError(status, args, output=data, stderr=b"")
        return subprocess.CompletedProcess(args, status, data, b"")

    def verify(
        self, arguments: Sequence[str], *, env: Mapping[str, str], limits: bounded_process.Limits
    ) -> tuple[int, bytes, bytes]:
        """Observe actual verifier arguments and return controlled complete raw output."""
        index = len(self.verifier_calls) % 4
        response, status = self.source.verifications[index], self.source.statuses[index]
        candidate = Path(arguments[3]).parent
        self.verifier_calls.append(
            {
                "subject": Path(arguments[3]).name,
                "arguments": [arg.replace(str(candidate), "$CANDIDATE") for arg in arguments],
                "environment": {
                    key: "$REDACTED"
                    if key in {"GH_TOKEN", "GITHUB_TOKEN"}
                    else value.replace(str(self.root), "$FIXTURE")
                    for key, value in sorted(env.items())
                },
                "timeoutSeconds": limits.timeout,
                "stdoutLimit": limits.stdout,
                "stderrLimit": limits.stderr,
                "status": status,
                "responseBytes": len(response),
                "responseSha256": digest(response),
            }
        )
        return status, response, b""


class Disk(NamedTuple):
    """Controlled local free-space observation; no filesystem work is replaced."""

    total: int
    used: int
    free: int


ADAPTER_SOURCE = r"""
import hashlib
import json
import os
import sys
import time
from pathlib import Path
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parent.parent
CONTROL = json.loads((ROOT / "adapter.json").read_text())


def event(role, status=0):
    # Fixed roles/status only; never retain config, credentials or signed URL text.
    row = json.dumps({"role": role, "status": status}, sort_keys=True).encode() + b"\n"
    fd = os.open(ROOT / "native-events.jsonl", os.O_WRONLY | os.O_APPEND)
    try:
        offset = 0
        while offset < len(row):
            offset += os.write(fd, row[offset:])
    finally:
        os.close(fd)


def refused():
    event("refused-adapter-contract", 99)
    return 99


def emit(data):
    offset = 0
    while offset < len(data):
        offset += os.write(1, data[offset:offset + 65536])


def gh(args):
    if args == ["--version"]:
        event("gh-version")
        emit(b"gh version 2.91.0 (inert fixture)\n")
        return 0
    index, role = None, None
    for at, expected in enumerate(CONTROL["metadataArguments"]):
        if args == expected:
            index, role = at, "api-artifact" if at == 0 else "api-run"
    if args == ["auth", "token", "--hostname", "github.com"]:
        if CONTROL["branch"] == "cached":
            return refused()
        index, role = 2, "auth-token"
    if len(args) >= 3 and args[:2] == ["attestation", "verify"]:
        subject = Path(args[2])
        if subject.name not in CONTROL["subjects"]:
            return refused()
        expected = ["attestation", "verify", args[2], "--bundle",
                    str(subject.parent / "release-attestation.jsonl")]
        expected += CONTROL["verificationTail"]
        if args != expected or "UNRELATED" in os.environ:
            return refused()
        index = 3 + CONTROL["subjects"].index(subject.name)
        role = "verify-" + subject.name
    if index is None:
        return refused()
    status = CONTROL["processStatuses"][index]
    event(role, status)
    emit((ROOT / "responses" / ("process-%02d.bin" % index)).read_bytes())
    return status


def curl(args):
    if args == ["--disable", "--version"]:
        event("curl-version")
        emit(b"curl 8.4.0 inert-fixture\nProtocols: https\nFeatures: SSL\n")
        return 0
    if args != CONTROL["curlArguments"]:
        return refused()
    data = bytearray()
    while len(data) <= 65536:
        chunk = os.read(0, 65537 - len(data))
        if not chunk:
            break
        data.extend(chunk)
    if len(data) > 65536 or "GH_TOKEN" in os.environ or "GITHUB_TOKEN" in os.environ:
        return refused()
    fields, headers = {}, []
    for line in data.decode("ascii").splitlines():
        key, separator, value = line.partition("=")
        if not separator:
            continue
        key = key.strip()
        if key in ("url", "header", "request-target", "user-agent", "connect-timeout"):
            parsed = json.loads(value.strip())
            if key == "header":
                headers.append(parsed)
            else:
                fields[key] = parsed
    target = urlsplit(fields.get("url", ""))
    request = fields.get("request-target", "")
    role = "api-redirect" if target.netloc == "api.github.com" else "blob"
    if str(fields.get("connect-timeout")) != ("20" if role == "api-redirect" else "15"):
        return refused()
    if role == "api-redirect":
        token = (ROOT / "responses/process-02.bin").read_bytes().decode().strip()
        if request != CONTROL["apiTarget"] or "Authorization: Bearer " + token not in headers:
            return refused()
        if "Accept: application/vnd.github+json" not in headers:
            return refused()
        if fields.get("user-agent") != "simplestchat-release-fetch":
            return refused()
        wire = (ROOT / "responses/api.http").read_bytes()
        status = 0
    else:
        if any(value.startswith("Authorization:") for value in headers):
            return refused()
        if fields.get("user-agent") != "":
            return refused()
        if target.netloc != CONTROL["blobHost"] or hashlib.sha256(request.encode()).hexdigest() \
                != CONTROL["blobTargetSha256"]:
            return refused()
        if "Accept: application/zip" not in headers or "Accept-Encoding: identity" not in headers:
            return refused()
        wire = (ROOT / "responses/blob.http").read_bytes()
        status = CONTROL["blobStatus"]
        wire = wire[:CONTROL["blobBodyOffset"] + CONTROL["blobBodyBytes"]]
        if CONTROL["scenario"] == "blob-read-fault":
            status = 7
            wire = wire[:CONTROL["blobBodyOffset"] + 65536]
    event(role, status)
    if role == "blob" and CONTROL["scenario"] == "blob-idle-gap":
        split = CONTROL["blobBodyOffset"] + 65536
        emit(wire[:split])
        time.sleep(16)
        emit(wire[split:])
    else:
        emit(wire)
    return status


def main():
    args = sys.argv[1:]
    if any("INERT_GH_TOKEN_FOR_LOCAL_WIRE" in value or "INERT_SIGNED_GRANT" in value
           for value in args):
        return refused()
    program = Path(sys.argv[0]).name
    if program == "gh":
        return gh(args)
    if program == "curl" and CONTROL["branch"] == "fresh":
        return curl(args)
    return refused()


if __name__ == "__main__":
    raise SystemExit(main())
"""


def adapter_control(source: Fixture) -> JsonObject:
    """Bind fixed inert executable roles to exported original response bytes."""
    repository = str(source.selection["repository"])
    endpoints = [
        f"repos/{repository}/actions/artifacts/{source.selection['artifactId']}",
        f"repos/{repository}/actions/runs/"
        + str(
            obj(decode_json(source.metadata[0]), "workflow_run")["id"]
            if source.scenario == "api-association-run"
            else source.selection["ciRunId"]
        ),
    ]
    metadata: list[JsonValue] = [
        [
            "api",
            "--hostname",
            "github.com",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "X-GitHub-Api-Version: 2022-11-28",
            endpoint,
        ]
        for endpoint in endpoints
    ]
    stream = RawPipe(source.api_wire)
    response = http.client.HTTPResponse(cast("socket.socket", cast("object", RawSocket(stream))))
    try:
        response.begin()
        location = response.getheader("Location", GRANT)
        parsed = urlsplit(location)
        target = parsed.path + ("?" + parsed.query if parsed.query else "")
    finally:
        response.close()
    # Real curl honors an exact Content-Length and reports truncated bodies as
    # exit18. Keep the original raw headers; project only these named fixture
    # body observations with the same mature parser used by the reference.
    body_bytes = len(source.blob_wire.data) - source.blob_wire.body_offset
    blob_status = 0
    if source.scenario in {"blob-extra-with-length", "blob-truncated", "blob-partial-truncated"}:
        stream = RawPipe(source.blob_wire)
        response = http.client.HTTPResponse(
            cast("socket.socket", cast("object", RawSocket(stream)))
        )
        try:
            response.begin()
            try:
                body = response.read()
            except http.client.IncompleteRead as incomplete:
                body, blob_status = incomplete.partial, 18
            body_bytes = len(body)
        finally:
            response.close()
    statuses: list[JsonValue] = [
        7 if source.scenario == "api-command-failure" else 0,
        0,
        7 if source.scenario == "token-command-failure" else 0,
        *source.statuses,
    ]
    return {
        "scenario": source.scenario,
        "branch": source.branch,
        "metadataArguments": metadata,
        "subjects": list[JsonValue](trust.SUBJECTS),
        "verificationTail": list[JsonValue](
            attest.verifier_arguments(
                Path("/fixture/image.tar"),
                Path("/fixture/release-attestation.jsonl"),
                source.selection,
            )[6:]
        ),
        "processStatuses": statuses,
        "apiTarget": "/" + endpoints[0] + "/zip",
        "blobHost": parsed.netloc,
        "blobTargetSha256": digest(target.encode()),
        "blobBodyOffset": source.blob_wire.body_offset,
        "blobBodyBytes": body_bytes,
        "blobStatus": blob_status,
        "curlArguments": [
            "--disable",
            "--silent",
            "--http1.1",
            "--include",
            "--path-as-is",
            "--globoff",
            "--noproxy",
            "*",
            "--proxy",
            "",
            "--proto",
            "=https",
            "--max-redirs",
            "0",
            "--retry",
            "0",
            "--config",
            "-",
        ],
    }


def populate(root: Path, source: Fixture) -> None:
    """Export pristine original inputs and raw response files before reference execution."""
    root.mkdir(mode=0o700)
    for name in ("candidate", "responses", "bin", "home", "config"):
        (root / name).mkdir(mode=0o700)
    if source.branch == "cached":
        for name, data in source.files.items():
            write(root / "candidate" / name, data)
    write(root / "download.zip", source.files["artifact.zip"])
    for index, data in enumerate([*source.metadata, source.token, *source.verifications]):
        write(root / "responses" / f"process-{index:02}.bin", data)
    write(root / "responses/api.http", source.api_wire.data)
    write(root / "responses/blob.http", source.blob_wire.data)
    write(root / "selection.json", encoded(source.selection))
    write(root / "environment.json", encoded(dict(cached.environment(root))))
    adapter = ("#!" + sys.executable + "\n" + ADAPTER_SOURCE).encode()
    write(root / "adapter.json", encoded(adapter_control(source)))
    write(root / "native-events.jsonl", b"")
    write(root / "bin/gh", adapter, 0o700)
    if source.branch == "fresh":
        write(root / "bin/curl", adapter, 0o700)
    if source.scenario in {"cached-member-changed", "cached-zip-changed"}:
        target = "source.json" if source.scenario == "cached-member-changed" else "artifact.zip"
        with (root / "candidate" / target).open("ab") as changed:
            _ = changed.write(b"changed")
    if source.scenario == "output-parent-symlink":
        (root / "real-output").mkdir(mode=0o700)
        (root / "output").symlink_to("real-output", target_is_directory=True)
    if source.scenario == "cached-directory-symlink":
        (root / "cached-link").symlink_to("candidate", target_is_directory=True)
    if sum(path.stat().st_size for path in root.rglob("*") if path.is_file()) > MAX_FIXTURE_BYTES:
        message = "test-only fresh fixture bound"
        raise ValueError(message)


def candidate_directory(root: Path, source: Fixture) -> Path | None:
    """Find only the fresh exact reference directory, without inventing a success path."""
    if source.branch == "cached":
        return root / "candidate"
    parent = root / "output"
    if not parent.is_dir() or parent.is_symlink():
        return None
    attempts = list(parent.iterdir())
    if len(attempts) != 1 or not attempts[0].is_dir():
        return None
    return attempts[0] / "artifact"


def observed_files(directory: Path | None) -> JsonObject:
    """Retain exact partial file prefixes after all reference calls return."""
    if directory is None or not directory.is_dir():
        return {}
    result: JsonObject = {}
    for path in sorted(directory.iterdir()):
        if path.is_file():
            data = path.read_bytes()
            result[path.name] = {
                "sha256": digest(data),
                "bytes": len(data),
                "mode": stat.S_IMODE(path.stat().st_mode),
            }
    return result


def selector_arguments(source: Fixture, root: Path) -> list[str]:
    """Use actual CLI options, preserving accepted reference-only lexical spellings."""
    artifact_id = str(source.selection["artifactId"])
    ci_run = str(source.selection["ciRunId"])
    if source.scenario == "selector-artifact-plus":
        artifact_id = "+" + artifact_id
    if source.scenario == "selector-ci-underscores":
        ci_run = "7_89"
    parent = (
        "relative-output"
        if source.scenario in {"output-parent-relative", "cached-output-parent-relative"}
        else str(root / "output")
    )
    args = [
        str(ROOT / "build/verify-release.py"),
        "--repository",
        "owner/repo",
        "--revision",
        str(source.selection["revision"]),
        "--artifact-id",
        artifact_id,
        "--ci-run",
        ci_run,
        "--output-parent",
        parent,
    ]
    if source.branch == "cached":
        directory = root / "candidate"
        if source.scenario == "cached-directory-symlink":
            directory = root / "cached-link"
        args.extend(
            [
                "--artifact-dir",
                "candidate" if source.scenario == "cached-directory-relative" else str(directory),
            ]
        )
    if source.scenario == "artifact-dir-empty":
        args.extend(["--artifact-dir", ""])
    return args


def run_main(root: Path, source: Fixture) -> tuple[int, JsonValue, str | None]:
    """Execute the original CLI main itself, retaining its real fixed/uncaught failure shape."""
    output, errors = io.StringIO(), io.StringIO()
    original_paths = sys.path[:]
    original_mask = os.umask(0o077)
    status, uncaught = 0, None
    try:
        with (
            patch.object(sys, "argv", selector_arguments(source, root)),
            chdir(root),
            redirect_stdout(output),
            redirect_stderr(errors),
        ):
            try:
                _ = runpy.run_path(str(ROOT / "build/verify-release.py"), run_name="__main__")
            except SystemExit as stopped:
                status = stopped.code if type(stopped.code) is int else 1
            except (
                ValueError,
                OSError,
                http.client.HTTPException,
                subprocess.SubprocessError,
                fetch.FetchError,
                AssertionError,
                zipfile.BadZipFile,
            ) as error:
                status, uncaught = 1, type(error).__name__
    finally:
        _ = os.umask(original_mask)
        sys.path[:] = original_paths
    data: JsonValue = None
    if output.getvalue():
        data = decode_json(output.getvalue())
        if isinstance(data, dict) and "directory" in data:
            data["directory"] = "$CANDIDATE"
    return status, data, uncaught


def output_tree(root: Path, source: Fixture) -> JsonObject:
    """Describe original creation ordering without exposing random directory suffixes."""
    parent = root / "output"
    directory = candidate_directory(root, source)
    return {
        "outputParentExists": parent.exists(),
        "outputParentSymlink": parent.is_symlink(),
        "transactionDirectories": len(list(parent.iterdir()))
        if parent.is_dir() and not parent.is_symlink()
        else 0,
        "candidateDirectoryExists": directory is not None and directory.is_dir(),
    }


def run(scenario: str, fixture_directory: Path | None = None) -> JsonObject:
    """Run both complete CLI branches with only closed external observations adapted."""
    source = fixture(scenario)
    if fixture_directory is not None:
        if (
            not fixture_directory.is_absolute()
            or fixture_directory.parent.resolve() != fixture_directory.parent
        ):
            message = "fixture export requires fresh canonical absolute path"
            raise ValueError(message)
        populate(fixture_directory, source)
    with tempfile.TemporaryDirectory(prefix="release-fresh-reference-") as temporary:
        root = Path(temporary).resolve() / "input"
        populate(root, source)
        before = observed_files(root / "candidate")
        processes, transport = Processes(source, root), Transport(source)
        real_disk = shutil.disk_usage

        def disk(path: str | Path) -> Disk:
            actual = real_disk(path)
            return Disk(
                actual.total, actual.used, 0 if scenario == "disk-insufficient" else actual.free
            )

        results: list[JsonValue] = []
        exit_codes: list[JsonValue] = []
        uncaught: list[JsonValue] = []
        file_snapshots: list[JsonValue] = []
        last_candidate: Path | None = root / "candidate" if source.branch == "cached" else None
        with (
            patch.object(subprocess, "run", processes.metadata),
            patch.object(bounded_process, "run", processes.verify),
            patch.object(http.client, "HTTPSConnection", transport.connect),
            patch.object(time, "monotonic", transport.monotonic),
            patch.object(shutil, "disk_usage", disk),
            patch.object(trust, "datetime", cached.FixedDateTime),
            patch.object(cached.FixedDateTime, "calls", 0),
            patch.object(cached.FixedDateTime, "clock", source.clock),
            patch.dict(
                os.environ,
                {**cached.environment(root), "UNRELATED": "must-not-inherit"},
                clear=True,
            ),
        ):
            for _ in range(source.repeat):
                parent = root / "output"
                previous: set[Path] = set(parent.iterdir()) if parent.is_dir() else set()
                status, result, failure = run_main(root, source)
                if source.branch == "fresh" and parent.is_dir() and not parent.is_symlink():
                    created = set(parent.iterdir()) - previous
                    if len(created) == 1:
                        last_candidate = created.pop() / "artifact"
                file_snapshots.append(observed_files(last_candidate))
                exit_codes.append(status)
                results.append(result)
                uncaught.append(failure)
                if status:
                    break
            utc_samples = cached.FixedDateTime.calls
        directory = last_candidate
        return {
            "schemaVersion": 1,
            "scenario": scenario,
            "input": {
                "branch": source.branch,
                "selectors": {
                    key: source.selection[key]
                    for key in ("repository", "revision", "artifactId", "ciRunId")
                },
                "repeat": source.repeat,
                "today": TODAY,
                "clock": source.clock,
                "rustProfile": source.profile,
                "nativeExpectedPassed": scenario == "blob-idle-gap" if source.profile else None,
                "socketIdleSeconds": 16 if scenario == "blob-idle-gap" else None,
                "selectorText": {
                    "artifactId": "+123"
                    if scenario == "selector-artifact-plus"
                    else str(source.selection["artifactId"]),
                    "ciRunId": "7_89"
                    if scenario == "selector-ci-underscores"
                    else str(source.selection["ciRunId"]),
                },
                "environment": {"path": "environment.json"},
                "diskFree": 0 if scenario == "disk-insufficient" else None,
                "processResponses": [
                    {
                        "path": f"responses/process-{i:02}.bin",
                        "sha256": digest(data),
                        "bytes": len(data),
                        "status": 7
                        if (i == 0 and scenario == "api-command-failure")
                        or (i == TOKEN_PROCESS_INDEX and scenario == "token-command-failure")
                        else source.statuses[i - 3]
                        if i >= VERIFICATION_PROCESS_START
                        else 0,
                    }
                    for i, data in enumerate(
                        [*source.metadata, source.token, *source.verifications]
                    )
                ],
                "httpResponses": [
                    {
                        "role": role,
                        "path": f"responses/{role}.http",
                        "sha256": digest(value.data),
                        "bytes": len(value.data),
                        "bodyOffset": value.body_offset,
                    }
                    for role, value in (("api", source.api_wire), ("blob", source.blob_wire))
                ],
            },
            "observation": {
                "passed": all(code == 0 for code in exit_codes),
                "exitCodes": exit_codes,
                "results": results,
                "uncaughtClasses": uncaught,
                "apiCalls": processes.api_calls,
                "verifierCalls": processes.verifier_calls,
                "httpCalls": [value.observation() for value in transport.connections],
                "files": observed_files(directory),
                "fileSnapshots": file_snapshots,
                "outputTree": {
                    **output_tree(root, source),
                    "candidateDirectoryExists": directory is not None and directory.is_dir(),
                },
                "cachedOriginalsUnchanged": before == observed_files(root / "candidate"),
                "utcSamples": utc_samples,
            },
        }


class Options(argparse.Namespace):
    """Only a closed scenario and optional fresh fixture export are accepted."""

    scenario: str = ""
    fixture_directory: Path | None = None


def main() -> int:
    """Expose only a finite closed fixture selector and bounded ordinary JSON."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("--scenario", required=True, choices=SCENARIOS)
    _ = parser.add_argument("--fixture-directory", type=Path)
    args = parser.parse_args(namespace=Options())
    result = run(args.scenario, args.fixture_directory)
    data = json.dumps(result, allow_nan=False)
    if len(data.encode()) > MAX_OUTPUT_BYTES:
        message = "fresh-release oracle output bound"
        raise ValueError(message)
    print(data)  # noqa: T201 -- One bounded public fixture observation, no raw credentials.
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
