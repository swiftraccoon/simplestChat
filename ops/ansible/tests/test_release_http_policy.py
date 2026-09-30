"""Offline passive response-policy tests; never contact Docker or a network endpoint."""

from __future__ import annotations

import http.client
import io
import json
import os
import signal
import socket
import ssl
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from typing import Unpack
from unittest.mock import patch

from test_support import ROOT

# isort: split
import release_container_harness as harness
import release_http_policy as policy
from release_container_harness import CommandOptions
from release_json import JsonObject, object_value
from test_release_container_harness import FakeCommands, FakeRun, TextResult

TOKEN = "c" * 32
CADDY = "a" * 64
APP = "b" * 64
CAPABILITIES: JsonObject = {
    "version": 1,
    "accounts": True,
    "passwordLogin": True,
    "passkeyLogin": False,
    "passwordRegistration": "invite",
    "passkeyRegistration": "disabled",
    "roomDirectory": True,
    "roomCreation": True,
    "adHocRooms": False,
}


def headers(endpoint: policy.Endpoint) -> dict[str, str]:
    """Provide one complete proxy response policy without credential values."""
    return {
        **policy.FIXED_HEADERS,
        "strict-transport-security": "max-age=31536000",
        "content-security-policy": "; ".join(
            key + " " + value for key, value in policy.CSP.items()
        ).strip(),
        "content-type": endpoint.content_type + "; charset=utf-8",
        "cache-control": "no-store",
    }


def body(endpoint: policy.Endpoint) -> bytes:
    """Use the five real endpoint shapes with entirely synthetic public content."""
    if endpoint.path == "/":
        return b"<!DOCTYPE html><html>Fixture</html>"
    if endpoint.path == "/api/capabilities":
        return json.dumps(CAPABILITIES).encode()
    if endpoint.path == "/api/auth/profile":
        return b'{"error":"Missing authorization"}'
    return b"Not Found"


def fixture(ca: Path | None = None) -> policy.Fixture:
    """Describe exact fixture identities; tests always replace external inspection."""
    return policy.Fixture(TOKEN, CADDY, APP, ca or Path("/unused-fixture-ca"), "d" * 64)


def inspection(identity: str) -> JsonObject:
    """Represent only the reviewed local Docker inspection fields."""
    return {
        "id": identity,
        "running": True,
        "labels": {
            policy.LABEL: TOKEN,
            "com.docker.compose.project": "simplestchat-public",
            "com.docker.compose.service": "caddy" if identity == CADDY else "simplestchat",
        },
        "ports": {"443/tcp": [{"HostIp": "127.0.0.1", "HostPort": "443"}], "80/tcp": None},
    }


class ResponsePolicyTests(unittest.TestCase):
    """Headers, bodies and authentication omission are checked without sending requests."""

    def test_current_response_contracts_pass(self) -> None:
        """All fixed responses satisfy the same validator used by the real local client."""
        for endpoint in policy.ENDPOINTS:
            with self.subTest(path=endpoint.path):
                policy.validate_response(
                    endpoint, endpoint.status, headers(endpoint), body(endpoint)
                )

    def test_security_headers_csp_overrides_and_duplicate_directives_fail(self) -> None:
        """Additional permissive CSP directives cannot override an otherwise safe fallback."""
        endpoint = policy.ENDPOINTS[0]
        for key in policy.FIXED_HEADERS:
            changed = headers(endpoint)
            del changed[key]
            with (
                self.subTest(key=key),
                self.assertRaisesRegex(policy.PolicyError, "security_headers"),
            ):
                policy.security_headers(changed)
        for appended in ("; script-src *", "; script-src-elem *", "; default-src 'self'"):
            changed = headers(endpoint)
            changed["content-security-policy"] += appended
            with self.subTest(appended=appended), self.assertRaises(policy.PolicyError):
                policy.security_headers(changed)
        changed = headers(endpoint)
        changed["content-security-policy"] = changed["content-security-policy"].replace(
            "script-src 'self'", "script-src 'self' 'unsafe-inline'"
        )
        with self.assertRaisesRegex(policy.PolicyError, "http_csp"):
            policy.security_headers(changed)

    def test_anonymous_cookie_transport_and_server_leaks_fail(self) -> None:
        """Even a securely flagged cookie is unexpected on these credential-free requests."""
        for name, value in (
            ("set-cookie", "fixture=value; Secure; HttpOnly; SameSite=Strict"),
            ("server", "fixture"),
            ("strict-transport-security", "max-age=0"),
        ):
            changed = headers(policy.ENDPOINTS[0])
            changed[name] = value
            with self.subTest(name=name), self.assertRaises(policy.PolicyError):
                policy.security_headers(changed)

    def test_duplicate_control_and_excessive_headers_fail(self) -> None:
        """Ambiguous or large wire metadata is rejected before reading a response body."""
        for values in (
            [("Content-Type", "text/html"), ("content-type", "application/json")],
            [("X-Test", "value\r\ninjected: value")],
            [("X-Test", "x" * policy.MAX_HEADERS)],
            [(f"X-{number}", "x") for number in range(policy.MAX_HEADER_COUNT + 1)],
        ):
            with self.subTest(count=len(values)), self.assertRaises(policy.PolicyError):
                _ = policy.header_map(values)

    def test_json_cache_and_public_payload_do_not_accept_secrets_or_unknown_fields(self) -> None:
        """Public capabilities are a fixed feature contract, not a route to internal data."""
        endpoint = policy.ENDPOINTS[1]
        changed = headers(endpoint)
        changed["cache-control"] = "public, max-age=600"
        with self.assertRaisesRegex(policy.PolicyError, "cache_policy"):
            policy.validate_response(endpoint, 200, changed, body(endpoint))
        for value in (
            {**CAPABILITIES, "sessionToken": "PRIVATE-FIXTURE"},
            {**CAPABILITIES, "accounts": "yes"},
            {**CAPABILITIES, "version": True},
        ):
            with self.assertRaisesRegex(policy.PolicyError, "public_capabilities"):
                policy.validate_response(
                    endpoint, 200, headers(endpoint), json.dumps(value).encode()
                )

    def test_auth_omission_and_private_denial_are_exact_and_bounded(self) -> None:
        """An authenticated profile or detailed operational response cannot pass as a denial."""
        endpoint = policy.ENDPOINTS[2]
        with self.assertRaisesRegex(policy.PolicyError, "http_status"):
            policy.validate_response(
                endpoint, 200, headers(endpoint), b'{"user":"PRIVATE-FIXTURE"}'
            )
        with self.assertRaisesRegex(policy.PolicyError, "auth_omission"):
            policy.validate_response(
                endpoint,
                401,
                headers(endpoint),
                b'{"error":"Missing authorization","token":"PRIVATE-FIXTURE"}',
            )
        for endpoint in policy.ENDPOINTS[3:]:
            with self.assertRaisesRegex(policy.PolicyError, "private_endpoint"):
                policy.validate_response(
                    endpoint, 404, headers(endpoint), b"detailed internal state"
                )
        for endpoint in policy.ENDPOINTS:
            with self.assertRaisesRegex(policy.PolicyError, "body_size"):
                policy.validate_response(
                    endpoint, endpoint.status, headers(endpoint), b"x" * (endpoint.limit + 1)
                )

    def test_redirect_compression_type_and_unlisted_endpoint_are_rejected(self) -> None:
        """Fixed requests do not follow redirects or decode unbounded compressed bodies."""
        endpoint = policy.ENDPOINTS[0]
        for status, updates, code in (
            (302, {}, "http_status"),
            (200, {"content-encoding": "gzip"}, "content_encoding"),
            (200, {"content-type": "application/json"}, "content_type"),
        ):
            with self.subTest(code=code), self.assertRaisesRegex(policy.PolicyError, code):
                policy.validate_response(
                    endpoint, status, {**headers(endpoint), **updates}, body(endpoint)
                )
        with self.assertRaisesRegex(policy.PolicyError, "unlisted_endpoint"):
            policy.validate_response(
                policy.Endpoint("/unlisted", 200, "text/html"),
                200,
                headers(endpoint),
                body(endpoint),
            )


class OwnershipTests(unittest.TestCase):
    """Only the known fixture's active local TLS listener may receive the five GETs."""

    def test_owned_services_and_exact_loopback_binding_pass(self) -> None:
        """The verifier inspects precisely the two passed immutable container IDs."""
        with patch.object(policy, "inspect_container", side_effect=inspection) as inspect:
            policy.owned_fixture(fixture())
        self.assertEqual([call.args[0] for call in inspect.call_args_list], [CADDY, APP])

    def test_foreign_stopped_or_public_service_is_rejected(self) -> None:
        """An existing local service is not authorization to run the policy check."""
        cases: list[JsonObject] = []
        stopped = inspection(CADDY)
        stopped["running"] = False
        cases.append(stopped)
        foreign = inspection(CADDY)
        object_value(foreign["labels"])[policy.LABEL] = "another-fixture"
        cases.append(foreign)
        public = inspection(CADDY)
        public["ports"] = {"443/tcp": [{"HostIp": "0.0.0.0", "HostPort": "443"}]}  # noqa: S104 -- Inert refusal fixture; no listener is opened.
        cases.append(public)
        forwarded = inspection(CADDY)
        forwarded["ports"] = {"443/tcp": [{"HostIp": "127.0.0.1", "HostPort": "8443"}]}
        cases.append(forwarded)
        for record in cases:
            with (
                patch.object(policy, "inspect_container", return_value=record),
                self.assertRaises(policy.PolicyError),
            ):
                policy.owned_fixture(fixture())

    def test_ca_digest_and_file_shape_are_checked_before_tls_parsing(self) -> None:
        """A changed, linked or excessive CA cannot replace the fixture's authority."""
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "ca.crt"
            _ = path.write_bytes(b"synthetic-ca-bytes")
            with (
                patch.object(ssl, "SSLContext") as constructor,
                self.assertRaisesRegex(policy.PolicyError, "ca_digest"),
            ):
                _ = policy.tls_context(fixture(path))
            constructor.assert_not_called()
            link = path.with_name("ca-link.crt")
            link.symlink_to(path)
            with self.assertRaises(OSError):
                _ = policy.tls_context(fixture(link))
            _ = path.write_bytes(b"x" * (policy.MAX_CA + 1))
            with self.assertRaisesRegex(policy.PolicyError, "ca_file"):
                _ = policy.tls_context(fixture(path))

    def test_five_requests_omit_credentials_and_require_ownership_afterward(self) -> None:
        """Successful HTTP results cannot mask an ownership change during the check."""
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        with (
            patch.object(policy, "owned_fixture") as owned,
            patch.object(policy, "tls_context", return_value=context),
            patch.object(policy, "request") as request,
        ):
            self.assertEqual(policy.check(fixture()), policy.success_report())
        self.assertEqual(owned.call_count, 2)
        self.assertEqual([call.args[0] for call in request.call_args_list], list(policy.ENDPOINTS))
        with (
            patch.object(
                policy, "owned_fixture", side_effect=policy.PolicyError("foreign_fixture")
            ),
            patch.object(policy, "tls_context") as tls,
            patch.object(policy, "request") as request,
            self.assertRaisesRegex(policy.PolicyError, "foreign_fixture"),
        ):
            _ = policy.check(fixture())
        tls.assert_not_called()
        request.assert_not_called()
        with (
            patch.object(
                policy, "owned_fixture", side_effect=[None, policy.PolicyError("fixture_changed")]
            ),
            patch.object(policy, "tls_context", return_value=context),
            patch.object(policy, "request"),
            self.assertRaisesRegex(policy.PolicyError, "fixture_changed"),
        ):
            _ = policy.check(fixture())

    def test_cli_failure_redacts_details_and_restores_deadline(self) -> None:
        """The published outcome contains no certificate path, exception body or cookie data."""
        arguments = [
            "--fixture-token",
            TOKEN,
            "--caddy-id",
            CADDY,
            "--app-id",
            APP,
            "--ca",
            "/private-fixture-ca",
            "--ca-sha256",
            "d" * 64,
        ]
        output = io.StringIO()
        with (
            patch.object(sys, "platform", "linux"),
            patch.object(os, "geteuid", return_value=0),
            patch.object(policy, "check", side_effect=OSError("PRIVATE-FIXTURE")),
            patch.object(signal, "signal"),
            patch.object(signal, "setitimer") as timer,
            redirect_stdout(output),
        ):
            self.assertEqual(policy.main(arguments), 1)
        self.assertNotIn("PRIVATE", output.getvalue())
        self.assertEqual(
            json.loads(output.getvalue()), {"passed": False, "code": "http_policy_failed"}
        )
        self.assertEqual(timer.call_args_list[-1].args, (signal.ITIMER_REAL, 0))


class FakeResponse:
    """A bounded in-memory response with observable read sizes."""

    def __init__(self, endpoint: policy.Endpoint, values: dict[str, str] | None = None) -> None:
        """Store synthetic data without opening a socket."""
        self.endpoint: policy.Endpoint = endpoint
        self.status: int = endpoint.status
        self.values: dict[str, str] = values or headers(endpoint)
        self.reads: list[int] = []

    def getheaders(self) -> list[tuple[str, str]]:
        """Return realistic repeated-header capable wire metadata."""
        return list(self.values.items())

    def read(self, limit: int) -> bytes:
        """Record the caller's finite read ceiling."""
        self.reads.append(limit)
        return body(self.endpoint)[:limit]


class FakeConnection:
    """Observe the plain HTTP protocol written over the separately verified TLS socket."""

    def __init__(self, response: FakeResponse) -> None:
        """Retain request arguments and cleanup state only."""
        self.response: FakeResponse = response
        self.sock: object = None
        self.calls: list[tuple[str, str, dict[str, str]]] = []
        self.closed: bool = False

    def request(self, method: str, path: str, *, headers: dict[str, str]) -> None:
        """Capture exactly what the passive client would send."""
        self.calls.append((method, path, headers))

    def getresponse(self) -> FakeResponse:
        """Provide synthetic response bytes without executing an application."""
        return self.response

    def close(self) -> None:
        """Track cleanup on both accepted and rejected responses."""
        self.closed = True


class TransportAndHarnessTests(unittest.TestCase):
    """The real client and canonical harness preserve the fixed target and request budget."""

    def test_transport_pins_loopback_tls_name_and_sends_no_authentication(self) -> None:
        """No URL, resolver name, proxy or ambient credential selects the destination."""
        endpoint = policy.ENDPOINTS[1]
        response = FakeResponse(endpoint)
        connection = FakeConnection(response)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        with (
            patch.object(socket, "create_connection") as connect,
            patch.object(context, "wrap_socket") as wrap,
            patch.object(http.client, "HTTPConnection", return_value=connection),
        ):
            policy.request(endpoint, context)
        self.assertEqual(connect.call_args.args, (("127.0.0.1", 443),))
        self.assertEqual(connect.call_args.kwargs["timeout"], policy.REQUEST_SECONDS)
        self.assertEqual(wrap.call_args.kwargs["server_hostname"], "localhost")
        self.assertEqual(
            connection.calls,
            [
                (
                    "GET",
                    endpoint.path,
                    {
                        "Host": "localhost",
                        "Accept": "application/json",
                        "Accept-Encoding": "identity",
                        "Connection": "close",
                    },
                )
            ],
        )
        self.assertEqual(response.reads, [endpoint.limit + 1])
        self.assertTrue(connection.closed)

    def test_excessive_announced_body_is_rejected_without_reading(self) -> None:
        """The Content-Length ceiling is enforced before retaining body bytes."""
        endpoint = policy.ENDPOINTS[0]
        response = FakeResponse(
            endpoint, {**headers(endpoint), "content-length": str(endpoint.limit + 1)}
        )
        connection = FakeConnection(response)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        with (
            patch.object(socket, "create_connection"),
            patch.object(context, "wrap_socket"),
            patch.object(http.client, "HTTPConnection", return_value=connection),
            self.assertRaisesRegex(policy.PolicyError, "content_length"),
        ):
            policy.request(endpoint, context)
        self.assertEqual(response.reads, [])
        self.assertTrue(connection.closed)

    def test_harness_invokes_fixed_child_and_accepts_only_complete_sanitized_report(self) -> None:
        """The canonical fixture binds its owned identities and keeps raw output private."""
        with tempfile.TemporaryDirectory() as temporary:
            current = harness.Harness(Path(temporary), "fixture-image")
            current.token = TOKEN
            current.ca_digest = "d" * 64

            calls: list[tuple[list[str], CommandOptions]] = []

            def run(args: list[str], **options: Unpack[CommandOptions]) -> TextResult:
                calls.append((args, options))
                return TextResult(json.dumps(policy.success_report()))

            commands = FakeCommands(run=FakeRun(side_effect=run))
            current.commands = commands
            with patch.object(current, "service", side_effect=[{"id": CADDY}, {"id": APP}]):
                current.response_policy()
            self.assertEqual(
                object_value(current.report["cases"])["httpResponsePolicy"], policy.success_report()
            )
            self.assertEqual(len(calls), 1)
            args, options = calls[0]
            self.assertIn(str(ROOT / "build/release_http_policy.py"), args)
            self.assertEqual(options.get("timeout"), 35)

            for response in (
                {**policy.success_report(), "private": "PRIVATE-FIXTURE"},
                {"passed": True},
            ):
                with (
                    patch.object(commands, "run", return_value=TextResult(json.dumps(response))),
                    patch.object(current, "service", side_effect=[{"id": CADDY}, {"id": APP}]),
                    self.assertRaisesRegex(harness.CheckError, "did not pass completely"),
                ):
                    current.response_policy()
