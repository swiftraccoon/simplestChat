"""Fetch a reviewed GitHub release artifact directly onto a prepared public VPS.

GitHub credentials remain on this controller. The installed receiver requests a
short-lived URL over SSH stdin only after its locked preflight is ready. This
command neither stages an image nor deploys/restarts services. Failure evidence
is private and retained; an uncertain remote outcome requires inspection, not an
automatic retry. Python 3.12+, authenticated gh, and OpenSSH are required.
"""

# Failure codes are intentionally fixed literals at validation boundaries.
# ruff: noqa: EM101

import argparse
import http.client
import json
import os
import re
import selectors
import signal
import ssl
import stat
import subprocess
import tempfile
import time
from dataclasses import dataclass
from datetime import UTC, datetime
from http import HTTPStatus
from pathlib import Path
from types import FrameType
from typing import IO, TypeGuard
from urllib.parse import urlsplit

from release_json import (
    DuplicateJsonError,
    JsonObject,
    JsonValue,
    decode_json,
    integer_value,
    string_value,
)

MAX_ZIP = 2 * 1024**3
MAX_LINE = 65536
MAX_PORT = 65535
MAX_URL_BYTES = 12288
FIRST_PRINTABLE = 32
DELETE_CHARACTER = 127
MAX_TOKEN_BYTES = 4096
MAX_REQUEST_BYTES = 16384
MAX_RECEIPT_SECONDS = 330
EXIT_RACE_SECONDS = 0.25
RECEIVER = "/usr/local/libexec/simplestchat-public/fetch-release.py"


class FetchError(Exception):
    """Only fixed, non-secret failure codes may cross the CLI boundary."""


def require(condition: object, code: str) -> None:
    """Reject an unmet invariant with its stable, non-sensitive failure code."""
    if not condition:
        raise FetchError(code)


def decoded(data: str | bytes) -> JsonValue:
    """Decode bounded JSON while rejecting duplicate keys and non-finite values."""
    try:
        return decode_json(data)
    except DuplicateJsonError as error:
        raise FetchError("duplicate_json_key") from error
    except ValueError as error:
        if isinstance(error, (json.JSONDecodeError, UnicodeDecodeError)):
            raise
        raise FetchError("invalid_json") from error


def positive(value: object) -> TypeGuard[int]:
    """Accept positive integer identities but never boolean values."""
    return type(value) is int and 0 < value <= 2**53 - 1


@dataclass
class ReleaseSelection:
    """Exact non-secret GitHub identities selected by the controller."""

    repository: str
    artifact_id: int
    revision: str
    ci_run: int


@dataclass
class FetchOptions(ReleaseSelection):
    """Validated fetch and direct SSH destinations."""

    host: str
    user: str
    identity: str
    output_parent: str
    port: int = 22


@dataclass
class RawOptions(argparse.Namespace):
    """Typed argparse string destinations before numeric and path validation."""

    repository: str = ""
    artifact_id: str = ""
    revision: str = ""
    ci_run: str = ""
    host: str = ""
    user: str = ""
    identity: str = ""
    output_parent: str = ""
    port: str = "22"


def options(argv: list[str] | None = None) -> FetchOptions:
    """Parse and validate explicit command-line selectors before side effects."""
    parser = argparse.ArgumentParser(description=__doc__)
    for name in (
        "repository",
        "artifact-id",
        "revision",
        "ci-run",
        "host",
        "user",
        "identity",
        "output-parent",
    ):
        _ = parser.add_argument("--" + name, required=True)
    _ = parser.add_argument("--port", default="22")
    raw = parser.parse_args(argv, namespace=RawOptions())

    def numeric(value: str) -> int:
        require(re.fullmatch(r"[1-9][0-9]{0,15}", value), "invalid_numeric_option")
        number = int(value)
        require(positive(number), "invalid_numeric_option")
        return number

    args = FetchOptions(
        raw.repository,
        numeric(raw.artifact_id),
        raw.revision,
        numeric(raw.ci_run),
        raw.host,
        raw.user,
        raw.identity,
        raw.output_parent,
        numeric(raw.port),
    )
    require(
        re.fullmatch(
            r"[A-Za-z0-9][A-Za-z0-9-]{0,38}/[A-Za-z0-9][A-Za-z0-9_.-]{0,99}", args.repository
        ),
        "invalid_repository",
    )
    require(re.fullmatch(r"[a-f0-9]{40}", args.revision), "invalid_revision")
    require(args.port <= MAX_PORT, "invalid_port")
    require(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,252}", args.host), "invalid_host")
    require(re.fullmatch(r"[a-z_][a-z0-9_-]{0,31}", args.user), "invalid_user")
    identity = Path(args.identity)
    require(identity.is_absolute() and not identity.is_symlink(), "invalid_identity")
    metadata = identity.stat()
    require(
        stat.S_ISREG(metadata.st_mode)
        and metadata.st_uid == os.getuid()
        and stat.S_IMODE(metadata.st_mode) in (0o400, 0o600),
        "invalid_identity",
    )
    parent = Path(args.output_parent)
    require(
        parent.is_absolute()
        and parent.resolve() == parent
        and (parent.is_dir() if parent.exists() else parent.parent.is_dir()),
        "invalid_output_parent",
    )
    return args


def gh(arguments: list[str]) -> bytes:
    """Run the authenticated GitHub CLI with bounded captured output."""
    result = subprocess.run(  # noqa: S603 -- Execute explicit argv without a shell at the process boundary.
        ["gh", *arguments],  # noqa: S607 -- Use the controller's authenticated GitHub CLI on PATH.
        stdin=subprocess.DEVNULL,
        capture_output=True,
        timeout=20,
        check=True,
    )
    require(len(result.stdout) <= MAX_LINE, "github_response_too_large")
    return result.stdout


def api(endpoint: str) -> JsonValue:
    """Read GitHub.com API JSON using the pinned protocol headers."""
    return decoded(
        gh(
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
        )
    )


def record(value: JsonValue, code: str) -> JsonObject:
    """Require a JSON object while retaining the caller's fixed failure code."""
    if not isinstance(value, dict):
        raise FetchError(code)
    return value


def repository_name(run: JsonObject, key: str) -> str:
    """Read a workflow repository identity, treating missing fields as mismatches."""
    value = run.get(key)
    if not isinstance(value, dict):
        return ""
    name = value.get("full_name")
    return name.lower() if isinstance(name, str) else ""


def verified_envelope(args: ReleaseSelection) -> JsonObject:
    """Require the exact reviewed commit, successful workflows, and API digest."""
    artifact = record(
        api(f"repos/{args.repository}/actions/artifacts/{args.artifact_id}"),
        "artifact_identity_mismatch",
    )
    require(
        artifact.get("id") == args.artifact_id
        and type(artifact.get("id")) is int
        and artifact.get("expired") is False
        and artifact.get("name") == f"simplestchat-production-{args.revision}",
        "artifact_identity_mismatch",
    )
    size = artifact.get("size_in_bytes")
    digest = artifact.get("digest")
    require(
        positive(size)
        and size <= MAX_ZIP
        and isinstance(digest, str)
        and re.fullmatch(r"sha256:[a-f0-9]{64}", digest),
        "artifact_digest_or_size_missing",
    )
    association = record(artifact.get("workflow_run"), "artifact_run_mismatch")
    require(
        positive(association.get("id"))
        and association.get("head_sha") == args.revision
        and positive(association.get("repository_id"))
        and positive(association.get("head_repository_id"))
        and association.get("repository_id") == association.get("head_repository_id"),
        "artifact_run_mismatch",
    )
    build_run = record(
        api(f"repos/{args.repository}/actions/runs/{association['id']}"),
        "workflow_identity_or_success_mismatch",
    )
    from_ci = build_run.get("path") == ".github/workflows/ci.yml"
    if from_ci:
        # A normal CI artifact is acceptable only from the exact successful
        # trusted push run supplied as the CI gate, never a PR or a second run.
        require(
            association["id"] == args.ci_run and build_run.get("head_branch") == "main",
            "artifact_ci_run_mismatch",
        )
        expected_build = ("CI", ".github/workflows/ci.yml", "push")
        runs = [(association["id"], build_run, *expected_build)]
    else:
        expected_build = (
            "Build production release artifact",
            ".github/workflows/release-artifact.yml",
            "workflow_dispatch",
        )
        ci_run = record(
            api(f"repos/{args.repository}/actions/runs/{args.ci_run}"),
            "workflow_identity_or_success_mismatch",
        )
        runs = [
            (association["id"], build_run, *expected_build),
            (args.ci_run, ci_run, "CI", ".github/workflows/ci.yml", "push"),
        ]
    for run_id, run, name, path, event in runs:
        require(
            type(run.get("id")) is int
            and run["id"] == run_id
            and run.get("name") == name
            and run.get("path") == path
            and run.get("event") == event
            and run.get("head_sha") == args.revision
            and run.get("status") == "completed"
            and run.get("conclusion") == "success"
            and repository_name(run, "repository") == args.repository.lower()
            and repository_name(run, "head_repository") == args.repository.lower(),
            "workflow_identity_or_success_mismatch",
        )
    return {
        "schemaVersion": 1,
        "repository": args.repository,
        "artifactId": args.artifact_id,
        "buildRunId": association["id"],
        "ciRunId": args.ci_run,
        "revision": args.revision,
        "artifactZipBytes": size,
        "zipSha256": string_value(digest).removeprefix("sha256:"),
    }


def storage_url(value: JsonValue) -> str:
    """Require a signed HTTPS download URL on an approved storage endpoint."""
    require(
        isinstance(value, str)
        and 0 < len(value) <= MAX_URL_BYTES
        and all(FIRST_PRINTABLE < ord(char) < DELETE_CHARACTER for char in value),
        "download_url_rejected",
    )
    value = string_value(value)
    parsed = urlsplit(value)
    require(
        parsed.scheme == "https"
        and re.fullmatch(r"[a-z0-9][a-z0-9-]{0,62}\.blob\.core\.windows\.net", parsed.netloc)
        and parsed.path.startswith("/")
        and parsed.query
        and not parsed.fragment,
        "download_url_rejected",
    )
    return value


def download_url(args: ReleaseSelection) -> str:
    """Never follow an authenticated redirect or place credentials in argv."""
    token = gh(["auth", "token", "--hostname", "github.com"]).decode().strip()
    require(
        0 < len(token) <= MAX_TOKEN_BYTES
        and all(FIRST_PRINTABLE < ord(char) < DELETE_CHARACTER for char in token),
        "github_authentication_failed",
    )
    connection = http.client.HTTPSConnection(
        "api.github.com", timeout=20, context=ssl.create_default_context()
    )
    try:
        connection.request(
            "GET",
            f"/repos/{args.repository}/actions/artifacts/{args.artifact_id}/zip",
            headers={
                "Authorization": "Bearer " + token,
                "Accept": "application/vnd.github+json",
                "X-GitHub-Api-Version": "2022-11-28",
                "User-Agent": "simplestchat-release-fetch",
            },
        )
        response = connection.getresponse()
        locations = [value for name, value in response.getheaders() if name.lower() == "location"]
        require(
            response.status == HTTPStatus.FOUND and len(locations) == 1,
            "github_download_redirect_missing",
        )
        return storage_url(locations[0])
    finally:
        token = None
        connection.close()


def ssh_environment() -> dict[str, str]:
    """Expose only the controller environment required for isolated SSH."""
    # No GitHub/API tokens enter SSH, including through configured SendEnv rules.
    environment = {
        key: os.environ[key]
        for key in ("PATH", "HOME", "USER", "LOGNAME", "SSH_AUTH_SOCK")
        if key in os.environ
    }
    return dict(environment, LC_ALL="C")


def ssh_arguments(args: FetchOptions) -> list[str]:
    """Construct pinned SSH arguments with forwarding and proxy execution disabled."""
    command = ["/usr/bin/python3", "-B", RECEIVER]
    if args.user != "root":
        command = ["sudo", "-n", "--", *command]
    settings = (
        "BatchMode=yes",
        "StrictHostKeyChecking=yes",
        "ConnectTimeout=15",
        "ControlMaster=no",
        "ControlPath=none",
        "ForwardAgent=no",
        "ForwardX11=no",
        "ClearAllForwardings=yes",
        "PermitLocalCommand=no",
        "ProxyCommand=none",
        "ProxyJump=none",
        "IdentitiesOnly=yes",
        "PasswordAuthentication=no",
        "KbdInteractiveAuthentication=no",
        "SendEnv=-*",
        "ServerAliveInterval=10",
        "ServerAliveCountMax=3",
    )
    return [
        "ssh",
        *[item for value in settings for item in ("-o", value)],
        "-i",
        args.identity,
        "-p",
        str(args.port),
        "-l",
        args.user,
        args.host,
        " ".join(command),
    ]


class Receiver:
    """Bound the two-message SSH protocol; never print raw remote output."""

    def __init__(self, args: FetchOptions) -> None:
        """Initialize explicit fixture or transport state before use."""
        self.process: subprocess.Popen[bytes] = subprocess.Popen(  # noqa: S603 -- Execute explicit argv without a shell at the process boundary.
            ssh_arguments(args),
            env=ssh_environment(),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
        self.selector: selectors.BaseSelector = selectors.DefaultSelector()
        if self.process.stdout is None or self.process.stdin is None:
            raise FetchError("receiver_pipes_missing")
        self.stdout: IO[bytes] = self.process.stdout
        self.stdin: IO[bytes] = self.process.stdin
        _ = self.selector.register(self.stdout, selectors.EVENT_READ)
        self.buffer: bytes = b""
        self.bytes: int = 0

    def send(self, value: JsonObject) -> None:
        """Write one validated request to the receiver protocol."""
        data = (json.dumps(value, separators=(",", ":")) + "\n").encode()
        require(len(data) <= MAX_REQUEST_BYTES, "request_too_large")
        _ = self.stdin.write(data)
        self.stdin.flush()

    def receive(self, seconds: float) -> JsonValue:
        """Read one bounded protocol response before its deadline."""
        deadline = time.monotonic() + seconds
        while b"\n" not in self.buffer:
            remaining = deadline - time.monotonic()
            require(remaining > 0 and self.selector.select(remaining), "receiver_response_timeout")
            chunk = os.read(self.stdout.fileno(), 4096)
            require(chunk, "receiver_closed_without_receipt")
            self.bytes += len(chunk)
            require(self.bytes <= MAX_LINE, "receiver_response_too_large")
            self.buffer += chunk
        line, self.buffer = self.buffer.split(b"\n", 1)
        return decoded(line)

    def finish(self) -> int:
        """Confirm receiver exit and reject unexpected trailing protocol output."""
        self.stdin.close()
        code = self.process.wait(timeout=15)
        # Wait for EOF as well as exit: a child must not retain the output pipe.
        deadline = time.monotonic() + 5
        while True:
            remaining = deadline - time.monotonic()
            require(remaining > 0 and self.selector.select(remaining), "receiver_output_not_closed")
            chunk = os.read(self.stdout.fileno(), 4096)
            if not chunk:
                break
            self.buffer += chunk
            require(len(self.buffer) <= MAX_LINE, "receiver_response_too_large")
        require(not self.buffer.strip(), "receiver_extra_output")
        return code

    def close(self) -> None:
        """Close the owned transport without leaving its child process running."""
        self.selector.close()
        try:
            if self.process.poll() is None:
                self._signal_group(signal.SIGTERM)
                try:
                    _ = self.process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    self._signal_group(signal.SIGKILL)
                    _ = self.process.wait(timeout=5)
        finally:
            try:
                self.stdin.close()
            except BrokenPipeError:
                pass
            finally:
                self.stdout.close()

    def _signal_group(self, signum: signal.Signals) -> None:
        """Signal only the owned group and tolerate permission denial only after exit."""
        try:
            os.killpg(self.process.pid, signum)
        except ProcessLookupError:
            pass
        except PermissionError as error:
            # The child can exit between poll() and killpg(); macOS may report
            # EPERM before waitpid can observe its exit. Only a bounded wait on
            # this exact Popen may confirm the race; a live child still fails.
            try:
                _ = self.process.wait(timeout=EXIT_RACE_SECONDS)
            except subprocess.TimeoutExpired:
                raise error from None


def verified_receipt(value: JsonValue, envelope: JsonObject) -> JsonObject:
    """Project only validated outcome fields bound to the requested artifact."""
    value = record(value, "receiver_receipt_invalid")
    require(
        type(value.get("schemaVersion")) is int
        and value["schemaVersion"] == 1
        and value.get("status") in ("complete", "failed")
        and all(type(value.get(key)) is bool for key in ("passed", "settled", "finalized")),
        "receiver_receipt_invalid",
    )
    for key in ("revision", "artifactId", "zipSha256", "artifactZipBytes"):
        require(
            value.get(key) == envelope[key] and type(value.get(key)) is type(envelope[key]),
            "receiver_identity_mismatch",
        )
    evidence = value.get("evidence")
    if evidence is None:
        require(
            not value["passed"]
            and not value["settled"]
            and not value["finalized"]
            and value.get("phase") == "preflight"
            and value.get("downloadedBytes") == 0
            and value.get("archiveSha256") is None
            and value.get("manifestSha256") is None,
            "receiver_evidence_invalid",
        )
    else:
        require(
            isinstance(evidence, str)
            and re.fullmatch(
                rf"/srv/simplestchat-public/releases/{envelope['revision']}/download\.[A-Za-z0-9_-]+",
                evidence,
            ),
            "receiver_evidence_invalid",
        )
    elapsed = value.get("elapsedSeconds")
    require(
        isinstance(elapsed, (int, float))
        and not isinstance(elapsed, bool)
        and 0 <= elapsed <= MAX_RECEIPT_SECONDS,
        "receiver_timing_invalid",
    )
    downloaded = value.get("downloadedBytes")
    require(
        type(downloaded) is int and 0 <= downloaded <= integer_value(envelope["artifactZipBytes"]),
        "receiver_byte_count_invalid",
    )
    for key in ("archiveSha256", "manifestSha256"):
        digest = value.get(key)
        require(
            digest is None or (isinstance(digest, str) and re.fullmatch(r"[a-f0-9]{64}", digest)),
            "receiver_digest_invalid",
        )
    phase = value.get("phase")
    require(
        isinstance(phase, str) and re.fullmatch(r"[a-z_]{1,40}", phase), "receiver_phase_invalid"
    )
    failure = value.get("failureClass")
    require(
        failure is None or (isinstance(failure, str) and re.fullmatch(r"[a-z_]{1,80}", failure)),
        "receiver_failure_invalid",
    )
    if value["passed"]:
        require(
            value["status"] == "complete"
            and value["settled"]
            and value["finalized"]
            and failure is None
            and value["phase"] == "complete"
            and value["downloadedBytes"] == envelope["artifactZipBytes"]
            and value["archiveSha256"] is not None
            and value["manifestSha256"] is not None,
            "receiver_success_inconsistent",
        )
    else:
        require(
            value["status"] == "failed" and failure is not None, "receiver_failure_inconsistent"
        )
    return {
        key: value[key]
        for key in (
            "schemaVersion",
            "status",
            "passed",
            "settled",
            "phase",
            "failureClass",
            "evidence",
            "revision",
            "artifactId",
            "zipSha256",
            "artifactZipBytes",
            "downloadedBytes",
            "archiveSha256",
            "manifestSha256",
            "elapsedSeconds",
            "finalized",
        )
    }


def save(path: Path, value: JsonObject) -> None:
    """Create private JSON evidence without replacing an earlier record."""
    with path.open("x", encoding="utf-8") as output:
        json.dump(value, output, indent=2)
        _ = output.write("\n")
        output.flush()
        os.fsync(output.fileno())


def main(argv: list[str] | None = None) -> int:  # noqa: C901, PLR0912, PLR0915 -- Keep ordered transaction checks together.
    """Run the CLI transaction and return a redacted success or failure status."""
    try:
        args = options(argv)
    except SystemExit as error:
        return error.code if isinstance(error.code, int) else 1
    except (FetchError, OSError, ValueError):
        print(json.dumps({"passed": False, "phase": "options", "failureClass": "invalid_options"}))  # noqa: T201 -- Intentional CLI status output.
        return 1
    began = time.monotonic()
    directory = None
    receiver = None
    report: JsonObject = {
        "schemaVersion": 1,
        "operation": "fetch_release_controller",
        "passed": False,
        "phase": "options",
        "remoteSettled": False,
        "remote": None,
        "failureClass": None,
        "startedAt": datetime.now(UTC).isoformat(),
    }

    def interrupted(signum: int, _frame: FrameType | None) -> None:
        raise FetchError("deadline_exceeded" if signum == signal.SIGALRM else "interrupted")

    previous = {
        signum: signal.signal(signum, interrupted)
        for signum in (signal.SIGALRM, signal.SIGINT, signal.SIGTERM)
    }
    _ = signal.alarm(420)
    previous_umask = os.umask(0o077)
    try:
        Path(args.output_parent).mkdir(mode=0o700, exist_ok=True)
        directory = Path(tempfile.mkdtemp(prefix="release-fetch.", dir=args.output_parent))
        report["evidence"] = str(directory)
        report["phase"] = "github_metadata"
        envelope = verified_envelope(args)
        save(directory / "artifact.json", envelope)
        report["phase"] = "receiver_preflight"
        receiver = Receiver(args)
        receiver.send(envelope)
        first = receiver.receive(45)
        if (
            isinstance(first, dict)
            and first == {"schemaVersion": 1, "status": "ready"}
            and type(first["schemaVersion"]) is int
        ):
            report["phase"] = "download_link"
            receiver.send({"url": download_url(args)})
            report["phase"] = "remote_fetch"
            final = receiver.receive(315)
        else:
            final = first
        receipt = verified_receipt(final, envelope)
        report["remote"] = receipt
        code = receiver.finish()
        report["remoteSettled"] = receipt["settled"]
        require(code == 0 and receipt["passed"] and report["remoteSettled"], "remote_fetch_failed")
        report["phase"] = "complete"
        report["passed"] = True
    except BaseException as error:  # noqa: BLE001 -- Redact all failures while finalizing owned resources.
        report["passed"] = False
        report["failureClass"] = (
            str(error) if isinstance(error, FetchError) else "controller_operation_failed"
        )
    finally:
        if receiver is not None:
            try:
                receiver.close()
            except BaseException:  # noqa: BLE001 -- Redact all failures while finalizing owned resources.
                report.update(passed=False, failureClass="controller_cleanup_uncertain")
        report["elapsedSeconds"] = round(time.monotonic() - began, 3)
        try:
            if directory is not None:
                save(directory / "outcome.json", report)
                for path in (directory, directory.parent):
                    descriptor = os.open(path, os.O_RDONLY)
                    try:
                        os.fsync(descriptor)
                    finally:
                        os.close(descriptor)
        except BaseException:  # noqa: BLE001 -- Redact all failures while finalizing owned resources.
            report.update(passed=False, failureClass="controller_receipt_write_failed")
        _ = signal.alarm(0)
        for signum, handler in previous.items():
            _ = signal.signal(signum, handler)
        _ = os.umask(previous_umask)
        print(json.dumps(report), flush=True)  # noqa: T201 -- Intentional CLI status output.
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
