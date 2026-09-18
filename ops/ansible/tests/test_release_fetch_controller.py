"""Offline controller identity, credential isolation and bounded protocol checks."""

from __future__ import annotations

import errno
import http.client
import io
import json
import os
import selectors
import signal
import stat
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from copy import deepcopy
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, override
from unittest.mock import AsyncMock, MagicMock, Mock, patch

import test_support

# Bootstrap flat checkout imports before loading helpers.
# isort: split
import release_fetch_controller as FETCH  # noqa: N812 -- Keep established helper aliases.
from release_json import JsonObject, JsonValue, decode_json, object_value, string_value

if TYPE_CHECKING:
    from collections.abc import Callable
    from typing import IO

REVISION = "a" * 40
TOKEN = "PRIVATE_GITHUB_TOKEN_SENTINEL"  # noqa: S105 -- Non-secret sentinel verifies credential redaction.
URL = (
    "https://productionresultssa6.blob.core.windows.net/path/artifact.zip?sig=PRIVATE_URL_SENTINEL"
)


def envelope() -> JsonObject:
    """Return a valid artifact envelope with fixed, non-secret identities."""
    return {
        "schemaVersion": 1,
        "repository": "owner/repo",
        "artifactId": 123,
        "buildRunId": 456,
        "ciRunId": 789,
        "revision": REVISION,
        "artifactZipBytes": 12345,
        "zipSha256": "b" * 64,
    }


def receipt(**changes: JsonValue) -> JsonObject:
    """Return a valid receiver receipt with explicit per-test mutations."""
    return dict(
        {
            "schemaVersion": 1,
            "status": "complete",
            "passed": True,
            "settled": True,
            "finalized": True,
            "phase": "complete",
            "failureClass": None,
            "evidence": f"/srv/simplestchat-public/releases/{REVISION}/download.fixture",
            "revision": REVISION,
            "artifactId": 123,
            "zipSha256": "b" * 64,
            "artifactZipBytes": 12345,
            "downloadedBytes": 12345,
            "archiveSha256": "c" * 64,
            "manifestSha256": "d" * 64,
            "elapsedSeconds": 1.25,
        },
        **changes,
    )


def api_records() -> list[JsonObject]:
    """Return the ordered GitHub API responses for a successful fixture selection."""
    artifact: JsonObject = {
        "id": 123,
        "expired": False,
        "name": f"simplestchat-production-{REVISION}",
        "size_in_bytes": 12345,
        "digest": "sha256:" + "b" * 64,
        "workflow_run": {
            "id": 456,
            "head_sha": REVISION,
            "repository_id": 42,
            "head_repository_id": 42,
        },
    }
    common: JsonObject = {
        "head_sha": REVISION,
        "status": "completed",
        "conclusion": "success",
        "repository": {"full_name": "owner/repo"},
        "head_repository": {"full_name": "owner/repo"},
    }
    return [
        artifact,
        dict(
            common,
            id=456,
            name="Build production release artifact",
            path=".github/workflows/release-artifact.yml",
            event="workflow_dispatch",
        ),
        dict(common, id=789, name="CI", path=".github/workflows/ci.yml", event="push"),
    ]


@dataclass
class FakeReceiver:
    """A scripted transport with typed responses and observable lifecycle events."""

    messages: list[JsonValue | Exception] = field(default_factory=list)
    sent: list[JsonObject] = field(default_factory=list)
    deadlines: list[float] = field(default_factory=list)
    finish_status: int = 0
    finish_error: Exception | None = None
    closes: int = 0
    event: Callable[[str, JsonValue], None] | None = None

    def send(self, value: JsonObject) -> None:
        """Write one validated request to the receiver protocol."""
        self.sent.append(value)
        if self.event:
            self.event("send", value)

    def receive(self, seconds: float) -> JsonValue:
        """Read one bounded protocol response before its deadline."""
        self.deadlines.append(seconds)
        if self.event:
            self.event("receive", None)
        value = self.messages.pop(0)
        if isinstance(value, Exception):
            raise value
        return value

    def finish(self) -> int:
        """Confirm receiver exit and reject unexpected trailing protocol output."""
        if self.finish_error:
            raise self.finish_error
        return self.finish_status

    def close(self) -> None:
        """Close the owned transport without leaving its child process running."""
        self.closes += 1


@dataclass
class HttpResponse:
    """A controlled API response used to test redirect validation."""

    status: int = 302
    headers: list[tuple[str, str]] = field(default_factory=lambda: [("Location", URL)])

    def getheaders(self) -> list[tuple[str, str]]:
        """Return the fixture response headers without making a network request."""
        return self.headers


@dataclass
class HttpConnection:
    """A recording HTTP boundary that cannot perform network requests."""

    response: HttpResponse
    requests: list[tuple[str, str, dict[str, str]]] = field(default_factory=list)
    closes: int = 0

    def request(self, method: str, url: str, *, headers: dict[str, str]) -> None:
        """Record the HTTP request for credential and endpoint assertions."""
        self.requests.append((method, url, headers))

    def getresponse(self) -> HttpResponse:
        """Return the controlled response without following any redirect."""
        return self.response

    def close(self) -> None:
        """Close the owned transport without leaving its child process running."""
        self.closes += 1


@dataclass
class CleanupProcess:
    """Model owned-child exit races without launching or signalling any process."""

    polls: list[int | None]
    exits: list[int | subprocess.TimeoutExpired]
    pid: int = 424242
    stdin: io.BytesIO = field(default_factory=io.BytesIO)
    stdout: io.BytesIO = field(default_factory=io.BytesIO)
    waits: list[float] = field(default_factory=list)

    def poll(self) -> int | None:
        """Return the next explicitly scripted owned-child observation."""
        return self.polls.pop(0)

    def wait(self, *, timeout: float) -> int:
        """Record bounded exit confirmation or the scripted escalation timeout."""
        self.waits.append(timeout)
        outcome = self.exits.pop(0)
        if isinstance(outcome, subprocess.TimeoutExpired):
            raise outcome
        return outcome


@dataclass
class CleanupSelector:
    """Accept only the in-memory receiver stream and record cleanup ownership."""

    registered: list[tuple[IO[bytes], int]] = field(default_factory=list)
    closed: bool = False

    def register(self, stream: IO[bytes], events: int) -> None:
        """Retain the receiver stream without requiring an operating-system descriptor."""
        self.registered.append((stream, events))

    def close(self) -> None:
        """Mark the selector closed even when later process cleanup fails."""
        self.closed = True


class ControllerTests(unittest.TestCase):
    """Verify artifact identity, credential isolation and bounded receiver behavior."""

    def __init__(self, method_name: str = "runTest") -> None:
        """Initialize explicit fixture or transport state before use."""
        super().__init__(method_name)
        self.root: Path = test_support.ROOT
        self.identity: Path = Path()
        self.argv: list[str] = []
        self.args: FETCH.FetchOptions = FETCH.FetchOptions(
            "owner/repo", 123, REVISION, 789, "host", "root", "/key", "/output"
        )

    @override
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="simplestchat-fetch-controller.")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.identity = self.root / "private key"
        _ = self.identity.write_text("disposable nonfunctional key fixture\n")
        self.identity.chmod(0o600)
        self.argv = [
            "--repository",
            "owner/repo",
            "--artifact-id",
            "123",
            "--revision",
            REVISION,
            "--ci-run",
            "789",
            "--host",
            "chat.example.test",
            "--user",
            "root",
            "--identity",
            str(self.identity),
            "--output-parent",
            str(self.root),
        ]
        self.args = FETCH.options(self.argv)

    def run_main(
        self, receiver: FakeReceiver, *, url: str = URL
    ) -> tuple[int, JsonObject, MagicMock | AsyncMock]:
        """Run the controller against a fake receiver and capture its private evidence."""
        output = io.StringIO()
        with (
            patch.object(FETCH, "verified_envelope", return_value=envelope()),
            patch.object(FETCH, "Receiver", return_value=receiver) as create,
            patch.object(FETCH, "download_url", return_value=url) as lookup,
            redirect_stdout(output),
        ):
            code = FETCH.main(self.argv)
        report = object_value(decode_json(output.getvalue()))
        create.assert_called_once()
        self.assertEqual(receiver.closes, 1)
        return code, report, lookup

    def test_options_reject_unsafe_destinations_and_require_a_private_owned_regular_key(
        self,
    ) -> None:
        """Verify options reject unsafe destinations and require a private owned regular key."""
        for option, value in (
            ("--repository", "../repo"),
            ("--repository", "owner/.repo"),
            ("--revision", "main"),
            ("--artifact-id", "0"),
            ("--ci-run", "01"),
            ("--port", "65536"),
            ("--port", "1.5"),
            ("--host", "2001:db8::1"),
            ("--host", "-oProxyCommand=command"),
            ("--user", "root;command"),
            ("--identity", "~/.ssh/key"),
            ("--output-parent", str(self.root / ".." / "results")),
        ):
            arguments = list(self.argv)
            if option in arguments:
                arguments[arguments.index(option) + 1] = value
            else:
                arguments.extend((option, value))
            with (
                self.subTest(option=option, value=value),
                redirect_stderr(io.StringIO()),
                self.assertRaises((FETCH.FetchError, SystemExit)),
            ):
                _ = FETCH.options(arguments)
        for mode in (0o644, 0o666, 0o700):
            self.identity.chmod(mode)
            with self.subTest(mode=mode), self.assertRaises(FETCH.FetchError):
                _ = FETCH.options(self.argv)
        self.identity.chmod(0o400)
        self.assertEqual(FETCH.options(self.argv).identity, str(self.identity))
        link = self.root / "key-link"
        link.symlink_to(self.identity)
        arguments = list(self.argv)
        arguments[arguments.index("--identity") + 1] = str(link)
        with self.assertRaises(FETCH.FetchError):
            _ = FETCH.options(arguments)

    def test_help_is_successful_without_network_or_evidence(self) -> None:
        """Verify help is successful without network or evidence."""
        output = io.StringIO()
        with (
            patch.object(FETCH, "gh") as github,
            patch.object(FETCH, "Receiver") as remote,
            redirect_stdout(output),
        ):
            self.assertEqual(FETCH.main(["--help"]), 0)
        github.assert_not_called()
        remote.assert_not_called()
        self.assertIn("usage:", output.getvalue())
        self.assertEqual(list(self.root.iterdir()), [self.identity])

    def test_exact_artifact_build_and_ci_run_are_required(self) -> None:
        """Verify exact artifact build and ci run are required."""
        with patch.object(FETCH, "api", side_effect=api_records()) as api:
            self.assertEqual(FETCH.verified_envelope(self.args), envelope())
        self.assertEqual(
            [call.args[0] for call in api.call_args_list],
            [
                "repos/owner/repo/actions/artifacts/123",
                "repos/owner/repo/actions/runs/456",
                "repos/owner/repo/actions/runs/789",
            ],
        )
        cases: list[tuple[int, tuple[str, ...], JsonValue]] = [
            (0, ("id",), True),
            (0, ("expired",), True),
            (0, ("name",), "other"),
            (0, ("size_in_bytes",), True),
            (0, ("size_in_bytes",), FETCH.MAX_ZIP + 1),
            (0, ("digest",), None),
            (0, ("workflow_run", "head_sha"), "e" * 40),
            (0, ("workflow_run", "id"), True),
            (0, ("workflow_run", "head_repository_id"), True),
        ]
        for index in (1, 2):
            cases.extend(
                (index, (key,), value)
                for key, value in (
                    ("id", 1),
                    ("name", "other"),
                    ("path", ".github/workflows/other.yml"),
                    ("event", "pull_request"),
                    ("head_sha", "e" * 40),
                    ("status", "in_progress"),
                    ("conclusion", "failure"),
                )
            )
            cases.extend(
                (index, (key, "full_name"), "fork/repo")
                for key in ("repository", "head_repository")
            )
        for index, path, value in cases:
            records = deepcopy(api_records())
            target = records[index]
            for key in path[:-1]:
                target = object_value(target[key])
            target[path[-1]] = value
            with (
                self.subTest(index=index, path=path, value=value),
                patch.object(FETCH, "api", side_effect=records),
                self.assertRaises(FETCH.FetchError),
            ):
                _ = FETCH.verified_envelope(self.args)

    def test_api_is_github_com_only_and_rejects_ambiguous_json(self) -> None:
        """Verify api is github com only and rejects ambiguous json."""
        with patch.object(FETCH, "gh", return_value=b'{"id":123}') as github:
            self.assertEqual(FETCH.api("repos/owner/repo/actions/artifacts/123"), {"id": 123})
        github.assert_called_once_with(
            [
                "api",
                "--hostname",
                "github.com",
                "-H",
                "Accept: application/vnd.github+json",
                "-H",
                "X-GitHub-Api-Version: 2022-11-28",
                "repos/owner/repo/actions/artifacts/123",
            ]
        )
        for data in (b'{"id":1,"id":2}', b'{"value":NaN}', b'{"value":Infinity}'):
            with self.subTest(data=data), self.assertRaises(FETCH.FetchError):
                _ = FETCH.decoded(data)
        with (
            patch.object(subprocess, "run", return_value=Mock(stdout=b"x" * (FETCH.MAX_LINE + 1))),
            self.assertRaisesRegex(FETCH.FetchError, "github_response_too_large"),
        ):
            _ = FETCH.gh(["api", "fixture"])

    def test_successful_main_push_ci_can_supply_its_own_exact_artifact(self) -> None:
        """Verify successful main push ci can supply its own exact artifact."""
        artifact, _, run = api_records()
        object_value(artifact["workflow_run"])["id"] = self.args.ci_run
        run["head_branch"] = "main"
        with patch.object(FETCH, "api", side_effect=[artifact, run]) as api:
            self.assertEqual(
                FETCH.verified_envelope(self.args), dict(envelope(), buildRunId=self.args.ci_run)
            )
        self.assertEqual(
            [call.args[0] for call in api.call_args_list],
            ["repos/owner/repo/actions/artifacts/123", "repos/owner/repo/actions/runs/789"],
        )

    def test_json_failures_preserve_redacted_error_classification(self) -> None:
        """Preserve duplicate, non-finite and malformed JSON failure categories."""
        with self.assertRaisesRegex(FETCH.FetchError, "duplicate_json_key"):
            _ = FETCH.decoded(b'{"id":1,"id":2}')
        with self.assertRaisesRegex(FETCH.FetchError, "invalid_json"):
            _ = FETCH.decoded(b'{"value":NaN}')
        with self.assertRaises(json.JSONDecodeError):
            _ = FETCH.decoded(b"not-json")
        with self.assertRaises(UnicodeDecodeError):
            _ = FETCH.decoded(b"\xff")

    def test_ci_artifacts_require_the_same_successful_trusted_push_run(self) -> None:
        """Verify ci artifacts require the same successful trusted push run."""
        artifact, _, run = api_records()
        object_value(artifact["workflow_run"])["id"] = self.args.ci_run
        run["head_branch"] = "main"
        changes: list[tuple[int, tuple[str, ...], JsonValue]] = [
            (0, ("workflow_run", "id"), 456),
            (0, ("workflow_run", "head_sha"), "e" * 40),
            (0, ("workflow_run", "head_repository_id"), 99),
            (0, ("expired",), True),
            (0, ("name",), "unrelated-artifact"),
            (1, ("id",), 456),
            (1, ("head_branch",), "feature"),
            (1, ("name",), "Untrusted workflow"),
            (1, ("path",), ".github/workflows/unrelated.yml"),
            (1, ("event",), "pull_request"),
            (1, ("event",), "workflow_dispatch"),
            (1, ("head_sha",), "e" * 40),
            (1, ("status",), "in_progress"),
            (1, ("conclusion",), "failure"),
            (1, ("repository", "full_name"), "fork/repo"),
            (1, ("head_repository", "full_name"), "fork/repo"),
        ]
        for index, path, value in changes:
            records = deepcopy([artifact, run])
            target = records[index]
            for key in path[:-1]:
                target = object_value(target[key])
            target[path[-1]] = value
            with (
                self.subTest(index=index, path=path, value=value),
                patch.object(FETCH, "api", side_effect=[*records, run]),
                self.assertRaises(FETCH.FetchError),
            ):
                _ = FETCH.verified_envelope(self.args)

    def test_storage_urls_require_one_trusted_https_endpoint_without_credentials_or_fragments(
        self,
    ) -> None:
        """Verify this storage urls require one trusted https endpoint without case."""
        self.assertEqual(FETCH.storage_url(URL), URL)
        for value in (
            URL.replace("https:", "http:"),
            URL.replace(".net/", ".net.evil.test/"),
            URL.replace("https://", "https://user:password@"),
            URL.replace(".net/", ".net:443/"),
            URL + "#fragment",
            URL.replace("?sig=PRIVATE_URL_SENTINEL", ""),
            URL + "\n",
            URL.replace("productionresultssa6.blob.core.windows.net", "127.0.0.1"),
            "file:///tmp/archive.zip",
            "x" * 12289,
        ):
            with self.subTest(value=value), self.assertRaises(FETCH.FetchError):
                _ = FETCH.storage_url(value)

    def test_token_stays_in_api_memory_and_authenticated_redirect_is_not_followed(self) -> None:
        """Verify token stays in api memory and authenticated redirect is not followed."""
        response = HttpResponse()
        connection = HttpConnection(response)
        with (
            patch.object(FETCH, "gh", return_value=(TOKEN + "\n").encode()) as github,
            patch.object(http.client, "HTTPSConnection", return_value=connection) as connect,
        ):
            self.assertEqual(FETCH.download_url(self.args), URL)
        github.assert_called_once_with(["auth", "token", "--hostname", "github.com"])
        self.assertEqual(connect.call_args.args, ("api.github.com",))
        self.assertEqual(len(connection.requests), 1)
        method, endpoint, headers = connection.requests[0]
        self.assertEqual((method, endpoint), ("GET", "/repos/owner/repo/actions/artifacts/123/zip"))
        self.assertEqual(headers["Authorization"], "Bearer " + TOKEN)
        self.assertEqual(connection.closes, 1)
        for status, headers in (
            (200, [("Location", URL)]),
            (307, [("Location", URL)]),
            (302, []),
            (302, [("Location", URL), ("location", URL)]),
        ):
            response.status, response.headers = status, headers
            with (
                self.subTest(status=status, headers=headers),
                patch.object(FETCH, "gh", return_value=TOKEN.encode()),
                patch.object(http.client, "HTTPSConnection", return_value=connection),
                self.assertRaises(FETCH.FetchError),
            ):
                _ = FETCH.download_url(self.args)

    def test_ssh_argv_and_environment_exclude_credentials_and_disable_unsupported_options(
        self,
    ) -> None:
        """Verify ssh argv and environment exclude credentials and disable unsupported options."""
        with patch.dict(os.environ, {"GH_TOKEN": TOKEN, "GITHUB_TOKEN": TOKEN, "PRIVATE_URL": URL}):
            environment = FETCH.ssh_environment()
            arguments = FETCH.ssh_arguments(self.args)
        combined = json.dumps([arguments, environment])
        self.assertNotIn(TOKEN, combined)
        self.assertNotIn(URL, combined)
        self.assertNotIn("GH_TOKEN", environment)
        self.assertEqual(
            set(environment) - {"PATH", "HOME", "USER", "LOGNAME", "SSH_AUTH_SOCK", "LC_ALL"}, set()
        )
        for setting in (
            "StrictHostKeyChecking=yes",
            "ForwardAgent=no",
            "SendEnv=-*",
            "ProxyCommand=none",
            "ProxyJump=none",
            "PermitLocalCommand=no",
            "ClearAllForwardings=yes",
            "ControlMaster=no",
            "ControlPath=none",
            "PasswordAuthentication=no",
        ):
            self.assertIn(setting, arguments)
        self.assertEqual(arguments[-1], "/usr/bin/python3 -B " + FETCH.RECEIVER)
        self.args.user = "deploy"
        self.assertEqual(
            FETCH.ssh_arguments(self.args)[-1], "sudo -n -- /usr/bin/python3 -B " + FETCH.RECEIVER
        )

    def test_receipt_projects_only_valid_identity_bound_fields(self) -> None:
        """Verify receipt projects only valid identity bound fields."""
        self.assertEqual(
            FETCH.verified_receipt(receipt(secret=TOKEN, url=URL), envelope()), receipt()
        )
        for key, value in (
            ("schemaVersion", True),
            ("artifactId", True),
            ("revision", "e" * 40),
            ("zipSha256", "e" * 64),
            ("artifactZipBytes", 1),
            ("passed", 1),
            ("evidence", "/tmp/download.fixture"),  # noqa: S108 -- Rejected path fixture; no file is created.
            ("evidence", None),
            ("elapsedSeconds", float("nan")),
            ("elapsedSeconds", 331),
            ("downloadedBytes", True),
            ("downloadedBytes", 12344),
            ("archiveSha256", None),
            ("manifestSha256", URL),
            ("phase", "download"),
            ("settled", False),
            ("finalized", False),
            ("failureClass", "error"),
        ):
            with self.subTest(key=key, value=value), self.assertRaises(FETCH.FetchError):
                _ = FETCH.verified_receipt(receipt(**{key: value}), envelope())
        early = receipt(
            status="failed",
            passed=False,
            settled=False,
            finalized=False,
            phase="preflight",
            failureClass="unfinished_operation",
            evidence=None,
            downloadedBytes=0,
            archiveSha256=None,
            manifestSha256=None,
        )
        self.assertEqual(FETCH.verified_receipt(early, envelope()), early)

    def test_ready_precedes_url_and_success_evidence_never_contains_secrets(self) -> None:
        """Verify ready precedes url and success evidence never contains secrets."""
        events: list[tuple[str, JsonValue]] = []

        def event(name: str, value: JsonValue) -> None:
            events.append((name, value))

        remote = FakeReceiver(
            messages=[{"schemaVersion": 1, "status": "ready"}, receipt(secret=TOKEN, url=URL)],
            event=event,
        )

        def download_url(_args: FETCH.ReleaseSelection) -> str:
            events.append(("url", None))
            return URL

        with (
            patch.object(FETCH, "verified_envelope", return_value=envelope()),
            patch.object(FETCH, "Receiver", return_value=remote),
            patch.object(FETCH, "download_url", side_effect=download_url),
            redirect_stdout(io.StringIO()) as output,
        ):
            self.assertEqual(FETCH.main(self.argv), 0)
        self.assertEqual(
            [event[0] for event in events], ["send", "receive", "url", "send", "receive"]
        )
        self.assertEqual(events[0][1], envelope())
        self.assertEqual(events[3][1], {"url": URL})
        report = object_value(decode_json(output.getvalue()))
        evidence = Path(string_value(report["evidence"]))
        self.assertEqual(evidence.parent, self.root)
        self.assertEqual(stat.S_IMODE(evidence.stat().st_mode), 0o700)
        for path in evidence.iterdir():
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
            self.assertNotIn(TOKEN, path.read_text())
            self.assertNotIn(URL, path.read_text())
        self.assertNotIn(TOKEN, output.getvalue())
        self.assertNotIn(URL, output.getvalue())
        self.assertEqual(decode_json((evidence / "artifact.json").read_text()), envelope())
        self.assertEqual(decode_json((evidence / "outcome.json").read_text()), report)
        self.assertTrue(report["passed"] and report["remoteSettled"])
        self.assertEqual(remote.deadlines, [45, 315])

    def test_failed_preflight_never_requests_url_and_is_not_retried(self) -> None:
        """Verify failed preflight never requests url and is not retried."""
        remote = FakeReceiver()
        remote.messages = [
            receipt(
                status="failed",
                passed=False,
                settled=False,
                finalized=False,
                phase="preflight",
                failureClass="unfinished_operation",
                evidence=None,
                downloadedBytes=0,
                archiveSha256=None,
                manifestSha256=None,
            )
        ]
        remote.finish_status = 1
        code, report, lookup = self.run_main(remote)
        self.assertEqual(code, 1)
        self.assertFalse(report["passed"] or report["remoteSettled"])
        self.assertEqual(object_value(report["remote"])["failureClass"], "unfinished_operation")
        lookup.assert_not_called()
        self.assertEqual(remote.sent, [envelope()])

    def test_failed_github_gate_never_starts_ssh_or_requests_a_url(self) -> None:
        """Verify failed github gate never starts ssh or requests a url."""
        with (
            patch.object(FETCH, "verified_envelope", side_effect=OSError(TOKEN + URL)),
            patch.object(FETCH, "Receiver") as remote,
            patch.object(FETCH, "download_url") as lookup,
            redirect_stdout(io.StringIO()) as output,
        ):
            self.assertEqual(FETCH.main(self.argv), 1)
        remote.assert_not_called()
        lookup.assert_not_called()
        report = object_value(decode_json(output.getvalue()))
        self.assertFalse(report["passed"] or report["remoteSettled"])
        self.assertNotIn(TOKEN, output.getvalue())
        self.assertNotIn(URL, output.getvalue())
        self.assertEqual(
            {path.name for path in Path(string_value(report["evidence"])).iterdir()},
            {"outcome.json"},
        )

    def test_nonzero_exit_cannot_turn_a_passed_looking_receipt_into_success(self) -> None:
        """Verify nonzero exit cannot turn a passed looking receipt into success."""
        remote = FakeReceiver()
        remote.messages = [{"schemaVersion": 1, "status": "ready"}, receipt()]
        remote.finish_status = 42
        code, report, lookup = self.run_main(remote)
        self.assertEqual(code, 1)
        self.assertFalse(report["passed"])
        self.assertEqual(report["failureClass"], "remote_fetch_failed")
        lookup.assert_called_once()

    def test_missing_receipt_or_unconfirmed_exit_never_claims_remote_settlement(self) -> None:
        """Verify missing receipt or unconfirmed exit never claims remote settlement."""
        for failure_at in ("receive", "finish"):
            remote = FakeReceiver()
            remote.messages = [{"schemaVersion": 1, "status": "ready"}, receipt()]
            if failure_at == "receive":
                remote.messages = [{"schemaVersion": 1, "status": "ready"}, OSError(TOKEN + URL)]
            else:
                remote.finish_error = subprocess.TimeoutExpired(
                    "ssh", 15, stderr=(TOKEN + URL).encode()
                )
            with self.subTest(failure_at=failure_at):
                code, report, lookup = self.run_main(remote)
                self.assertEqual(code, 1)
                self.assertFalse(report["passed"] or report["remoteSettled"])
                self.assertNotIn(TOKEN, json.dumps(report))
                self.assertNotIn(URL, json.dumps(report))
                lookup.assert_called_once()

    def test_real_local_subprocess_exercises_line_protocol_exit_and_extra_output(self) -> None:
        """Verify real local subprocess exercises line protocol exit and extra output."""
        # This replaces SSH with an owned Python child: no network or daemon.
        source = """import json, sys
json.loads(sys.stdin.readline())
print(json.dumps({"schemaVersion": 1, "status": "ready"}), flush=True)
assert set(json.loads(sys.stdin.readline())) == {"url"}
print(sys.argv[1], flush=True)
if sys.argv[2] == "extra": print("unexpected extra output", flush=True)
sys.exit(42 if sys.argv[2] == "failed" else 0)
"""
        for mode in ("complete", "failed", "extra"):
            with (
                self.subTest(mode=mode),
                patch.object(
                    FETCH,
                    "ssh_arguments",
                    return_value=[
                        sys.executable,
                        "-u",
                        "-c",
                        source,
                        json.dumps(receipt()),
                        mode,
                    ],
                ),
            ):
                remote = FETCH.Receiver(self.args)
                try:
                    remote.send(envelope())
                    self.assertEqual(remote.receive(3), {"schemaVersion": 1, "status": "ready"})
                    remote.send({"url": URL})
                    self.assertEqual(remote.receive(3), receipt())
                    if mode == "extra":
                        with self.assertRaisesRegex(FETCH.FetchError, "receiver_extra_output"):
                            _ = remote.finish()
                    else:
                        self.assertEqual(remote.finish(), 42 if mode == "failed" else 0)
                finally:
                    remote.close()

    def test_signal_permission_denial_requires_confirmed_owned_child_exit(self) -> None:
        """Ignore TERM/KILL permission races only after the exact owned child has exited."""
        for denied_signal in (signal.SIGTERM, signal.SIGKILL):
            for exited in (False, True):
                process = CleanupProcess(polls=[None], exits=[])
                expected_signals = [signal.SIGTERM]
                expected_waits: list[float] = []
                if denied_signal == signal.SIGKILL:
                    process.exits.append(subprocess.TimeoutExpired("fixture", 5))
                    expected_signals.append(signal.SIGKILL)
                    expected_waits.append(5)
                if exited:
                    process.exits.extend((0, 0))
                    expected_waits.extend((FETCH.EXIT_RACE_SECONDS, 5))
                else:
                    process.exits.append(
                        subprocess.TimeoutExpired("fixture", FETCH.EXIT_RACE_SECONDS)
                    )
                    expected_waits.append(FETCH.EXIT_RACE_SECONDS)
                selector = CleanupSelector()
                signals: list[signal.Signals] = []

                def send_group(
                    identifier: int,
                    signum: signal.Signals,
                    *,
                    owned: CleanupProcess = process,
                    observed: list[signal.Signals] = signals,
                    denied: signal.Signals = denied_signal,
                ) -> None:
                    self.assertEqual(identifier, owned.pid)
                    observed.append(signum)
                    if signum == denied:
                        raise PermissionError(errno.EPERM, "fixture signal permission")

                with (
                    self.subTest(denied_signal=denied_signal, exited=exited),
                    patch.object(subprocess, "Popen", return_value=process),
                    patch.object(selectors, "DefaultSelector", return_value=selector),
                    patch.object(os, "killpg", side_effect=send_group),
                ):
                    remote = FETCH.Receiver(self.args)
                    if exited:
                        remote.close()
                    else:
                        with self.assertRaises(PermissionError) as failure:
                            remote.close()
                        self.assertEqual(failure.exception.errno, errno.EPERM)
                    self.assertEqual(signals, expected_signals)
                    self.assertEqual(process.waits, expected_waits)
                    self.assertEqual(process.polls, [])
                    self.assertEqual(process.exits, [])
                    self.assertTrue(selector.closed)
                    self.assertTrue(process.stdin.closed)
                    self.assertTrue(process.stdout.closed)

    def test_real_local_subprocess_eof_and_truncated_receipts_fail_closed(self) -> None:
        """Verify real local subprocess eof and truncated receipts fail closed."""
        for message in ("", '{"schemaVersion":1', "not-json\n"):
            source = (
                "import sys; sys.stdin.readline(); "
                + "sys.stdout.write(sys.argv[1]); sys.stdout.flush()"
            )
            with (
                self.subTest(message=message),
                patch.object(
                    FETCH,
                    "ssh_arguments",
                    return_value=[
                        sys.executable,
                        "-u",
                        "-c",
                        source,
                        message,
                    ],
                ),
            ):
                remote = FETCH.Receiver(self.args)
                try:
                    self.assertEqual(os.getpgid(remote.process.pid), remote.process.pid)
                    remote.send(envelope())
                    with self.assertRaises((FETCH.FetchError, json.JSONDecodeError)):
                        _ = remote.receive(3)
                finally:
                    remote.close()
                self.assertIsNotNone(remote.process.poll())


if __name__ == "__main__":
    _ = unittest.main()
