"""Check five ordinary responses from the owned disposable release fixture only.

No origin or arbitrary path is accepted. Docker ownership, the exact loopback TLS
publication and the fixture CA are verified before connecting to 127.0.0.1:443.
Requests contain no credentials and never follow redirects, discover endpoints,
create accounts or send chat. The parent harness also bounds this entire process.
"""

from __future__ import annotations

import argparse
import hashlib
import http.client
import json
import os
import re
import signal
import socket
import ssl
import stat
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "ops/ansible/files"))
import bounded_process
from release_json import JsonObject, decode_json, object_value

if TYPE_CHECKING:
    from collections.abc import Sequence
    from types import FrameType

MAX_HEADERS = 16 * 1024
MAX_HEADER_COUNT = 64
MAX_CA = 64 * 1024
MAX_HTML = 128 * 1024
MAX_JSON = 4096
REQUEST_SECONDS = 3
TOTAL_SECONDS = 25
LABEL = "simplestchat.release-test"
HEX_ID = re.compile(r"[a-f0-9]{64}")
DOCKER = ("/usr/bin/docker", "--host", "unix:///var/run/docker.sock")
ENV = {"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "LC_ALL": "C"}
CSP = {
    "default-src": "'self'",
    "base-uri": "'none'",
    "object-src": "'none'",
    "frame-ancestors": "'none'",
    "form-action": "'self'",
    "script-src": "'self'",
    "style-src": "'self' 'unsafe-inline'",
    "img-src": "'self' data: blob:",
    "media-src": "'self' blob:",
    "connect-src": "'self'",
    "font-src": "'self'",
    "upgrade-insecure-requests": "",
}
FIXED_HEADERS = {
    "x-content-type-options": "nosniff",
    "x-frame-options": "DENY",
    "referrer-policy": "no-referrer",
    "cross-origin-opener-policy": "same-origin",
    "cross-origin-resource-policy": "same-origin",
    "permissions-policy": "camera=(self), microphone=(self), "
    + "display-capture=(self), geolocation=()",
}


class PolicyError(Exception):
    """A fixed code that never includes response bytes, cookies or local credentials."""


def require(condition: object, code: str) -> None:
    """Fail closed using a fixed public diagnostic."""
    if not condition:
        raise PolicyError(code)


@dataclass(frozen=True)
class Endpoint:
    """One fixed nonmutating fixture request and its response budget."""

    path: str
    status: int
    content_type: str
    limit: int = MAX_JSON
    no_store: bool = False


ENDPOINTS = (
    Endpoint("/", 200, "text/html", MAX_HTML),
    Endpoint("/api/capabilities", 200, "application/json", no_store=True),
    Endpoint("/api/auth/profile", 401, "application/json"),
    Endpoint("/metrics", 404, "text/plain"),
    Endpoint("/diagnostics/media", 404, "text/plain"),
)


@dataclass(frozen=True)
class Fixture:
    """Only exact harness-created identities and an authenticated local CA are accepted."""

    token: str
    caddy_id: str
    app_id: str
    ca: Path
    ca_sha256: str


def inspect_container(identity: str) -> JsonObject:
    """Inspect a fixed local Docker daemon without inherited context or credentials."""
    require(HEX_ID.fullmatch(identity), "fixture_container_identity")
    status, output, error = bounded_process.run(
        [
            *DOCKER,
            "inspect",
            "--format",
            '{"id":{{json .Id}},"running":{{json .State.Running}},'
            + '"labels":{{json .Config.Labels}},"ports":{{json .NetworkSettings.Ports}}}',
            identity,
        ],
        env=ENV,
        limits=bounded_process.Limits(timeout=5, stdout=65536, stderr=4096),
    )
    require(status == 0 and not error, "fixture_container_inspection")
    return object_value(decode_json(output))


def owned_fixture(fixture: Fixture) -> None:
    """Require both running fixture services and exactly the harness's HTTPS publication."""
    require(re.fullmatch(r"[a-f0-9]{32}", fixture.token), "fixture_token")
    require(fixture.caddy_id != fixture.app_id, "fixture_distinct_services")
    for identity, service in ((fixture.caddy_id, "caddy"), (fixture.app_id, "simplestchat")):
        record = inspect_container(identity)
        labels = object_value(record["labels"])
        require(
            record["id"] == identity
            and record["running"] is True
            and labels.get(LABEL) == fixture.token
            and labels.get("com.docker.compose.project") == "simplestchat-public"
            and labels.get("com.docker.compose.service") == service,
            "fixture_container_ownership",
        )
        if service == "caddy":
            ports = object_value(record["ports"])
            require(
                ports.get("443/tcp") == [{"HostIp": "127.0.0.1", "HostPort": "443"}]
                and all(value is None for key, value in ports.items() if key != "443/tcp"),
                "fixture_https_publication",
            )


def tls_context(fixture: Fixture) -> ssl.SSLContext:
    """Read the owned CA once, bind its bytes, and trust no unrelated CA store."""
    require(HEX_ID.fullmatch(fixture.ca_sha256), "fixture_ca_identity")
    descriptor = os.open(fixture.ca, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as source:
        info = os.fstat(source.fileno())
        require(stat.S_ISREG(info.st_mode) and 0 < info.st_size <= MAX_CA, "fixture_ca_file")
        data = source.read(MAX_CA + 1)
    require(
        len(data) == info.st_size and hashlib.sha256(data).hexdigest() == fixture.ca_sha256,
        "fixture_ca_digest",
    )
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    context.load_verify_locations(cadata=data.decode("ascii"))
    require(context.check_hostname and context.verify_mode == ssl.CERT_REQUIRED, "fixture_tls")
    return context


def header_map(values: Sequence[tuple[str, str]]) -> dict[str, str]:
    """Reject ambiguous or excessive headers before any response body is retained."""
    require(len(values) <= MAX_HEADER_COUNT, "http_header_count")
    require(
        sum(len(key) + len(value) + 4 for key, value in values) <= MAX_HEADERS, "http_headers_size"
    )
    headers: dict[str, str] = {}
    for key, value in values:
        name = key.lower()
        require(name not in headers, "http_duplicate_header")
        require(not any(character in key + value for character in "\r\n\0"), "http_header_control")
        headers[name] = value.strip()
    return headers


def security_headers(headers: dict[str, str]) -> None:
    """Check the complete reviewed proxy policy, including CSP fallback overrides."""
    require(
        all(headers.get(key) == value for key, value in FIXED_HEADERS.items()),
        "http_security_headers",
    )
    require("server" not in headers, "http_server_header")
    require("set-cookie" not in headers, "http_anonymous_cookie")
    hsts = headers.get("strict-transport-security", "")
    require(hsts == "max-age=31536000", "http_hsts")
    directives: dict[str, str] = {}
    for directive in headers.get("content-security-policy", "").split(";"):
        words = directive.strip().split()
        if not words:
            continue
        name = words[0].lower()
        require(name not in directives, "http_csp_duplicate")
        directives[name] = " ".join(words[1:])
    require(directives == CSP, "http_csp")


def validate_response(
    endpoint: Endpoint, status: int, headers: dict[str, str], body: bytes
) -> None:
    """Validate fixed public responses without retaining their data in a report."""
    require(endpoint in ENDPOINTS, "http_unlisted_endpoint")
    require(status == endpoint.status, "http_status")
    security_headers(headers)
    require(
        headers.get("content-type", "").split(";", 1)[0].lower() == endpoint.content_type,
        "http_content_type",
    )
    require(headers.get("content-encoding", "identity") == "identity", "http_content_encoding")
    require(0 < len(body) <= endpoint.limit, "http_body_size")
    if endpoint.no_store:
        require(headers.get("cache-control") == "no-store", "http_cache_policy")
    if endpoint.path == "/":
        require(b"<!doctype html>" in body[:256].lower(), "http_homepage")
    elif endpoint.path == "/api/auth/profile":
        require(
            object_value(decode_json(body)) == {"error": "Missing authorization"},
            "http_auth_omission",
        )
    elif endpoint.path == "/api/capabilities":
        value = object_value(decode_json(body))
        require(
            set(value)
            == {
                "version",
                "accounts",
                "passwordLogin",
                "passkeyLogin",
                "passwordRegistration",
                "passkeyRegistration",
                "roomDirectory",
                "roomCreation",
                "adHocRooms",
            }
            and type(value["version"]) is int
            and value["version"] == 1
            and all(
                type(value[key]) is bool
                for key in (
                    "accounts",
                    "passwordLogin",
                    "passkeyLogin",
                    "roomDirectory",
                    "roomCreation",
                    "adHocRooms",
                )
            )
            and value["passwordRegistration"] in ("disabled", "open", "invite")
            and value["passkeyRegistration"] in ("disabled", "open"),
            "http_public_capabilities",
        )
    else:
        require(body.strip() == b"Not Found", "http_private_endpoint")


def request(endpoint: Endpoint, context: ssl.SSLContext) -> None:
    """Connect to the fixed loopback socket while verifying the localhost TLS identity."""
    require(endpoint in ENDPOINTS, "http_unlisted_endpoint")
    connection = http.client.HTTPConnection("localhost", 443, timeout=REQUEST_SECONDS)
    try:
        raw = socket.create_connection(("127.0.0.1", 443), timeout=REQUEST_SECONDS)
        try:
            connection.sock = context.wrap_socket(raw, server_hostname="localhost")
        except BaseException:
            raw.close()
            raise
        connection.request(
            "GET",
            endpoint.path,
            headers={
                "Host": "localhost",
                "Accept": endpoint.content_type,
                "Accept-Encoding": "identity",
                "Connection": "close",
            },
        )
        response = connection.getresponse()
        headers = header_map(response.getheaders())
        length = headers.get("content-length")
        require(
            length is None or (length.isdecimal() and int(length) <= endpoint.limit),
            "http_content_length",
        )
        body = response.read(endpoint.limit + 1)
        validate_response(endpoint, response.status, headers, body)
    finally:
        connection.close()


def success_report() -> JsonObject:
    """Expose only fixed verified policy names and a fixed request count."""
    return {
        "passed": True,
        "requests": len(ENDPOINTS),
        "scope": "owned-loopback-release-fixture",
        "securityHeaders": True,
        "contentSecurityPolicy": True,
        "jsonNoStore": True,
        "anonymousCookiesAbsent": True,
        "missingAuthorizationRejected": True,
        "privateEndpointsOmitted": True,
        "responseLimits": True,
    }


def check(fixture: Fixture) -> JsonObject:
    """Verify ownership before and after exactly five credential-free GET requests."""
    owned_fixture(fixture)
    context = tls_context(fixture)
    for endpoint in ENDPOINTS:
        request(endpoint, context)
    owned_fixture(fixture)
    return success_report()


class Arguments(argparse.Namespace):
    """Keep the narrow fixed-target command interface statically typed."""

    fixture_token: str = ""
    caddy_id: str = ""
    app_id: str = ""
    ca: Path = Path()
    ca_sha256: str = ""


def deadline(_number: int, _frame: FrameType | None) -> None:
    """Interrupt slow headers, TLS, ownership inspection or body reads without a retry."""
    code = "http_policy_deadline"
    raise PolicyError(code)


def main(argv: Sequence[str] | None = None) -> int:
    """Expose fixture identities only; no target URL, arbitrary path or credentials option."""
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ("--fixture-token", "--caddy-id", "--app-id", "--ca-sha256"):
        _ = parser.add_argument(flag, required=True)
    _ = parser.add_argument("--ca", type=Path, required=True)
    arguments = parser.parse_args(argv, namespace=Arguments())
    previous = signal.signal(signal.SIGALRM, deadline)
    _ = signal.setitimer(signal.ITIMER_REAL, TOTAL_SECONDS)
    try:
        require(sys.platform == "linux" and os.geteuid() == 0, "fixture_host")
        fixture = Fixture(
            arguments.fixture_token,
            arguments.caddy_id,
            arguments.app_id,
            arguments.ca,
            arguments.ca_sha256,
        )
        result = check(fixture)
    except PolicyError as error:
        result = {"passed": False, "code": str(error)}
    except (OSError, ValueError, KeyError, TypeError, RuntimeError, http.client.HTTPException):
        result = {"passed": False, "code": "http_policy_failed"}
    finally:
        _ = signal.setitimer(signal.ITIMER_REAL, 0)
        _ = signal.signal(signal.SIGALRM, previous)
    _ = sys.stdout.write(json.dumps(result, sort_keys=True) + "\n")
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
