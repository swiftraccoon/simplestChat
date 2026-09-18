"""Seed the owned lobby through the fixed private API; never print credentials."""

from __future__ import annotations

import fcntl
import hmac
import http.client
import http.cookiejar
import json
import os
import re
import secrets
import signal
import stat
import sys
import urllib.error
import urllib.parse
import urllib.request
import uuid
from http import HTTPStatus
from typing import TYPE_CHECKING, Protocol, TypedDict, cast, override

from release_json import DuplicateJsonError, JsonValue, decode_json

if TYPE_CHECKING:
    from collections.abc import Mapping
    from email.message import Message
    from types import FrameType
    from typing import IO

CONFIG_DIRECTORY = "/etc/simplestchat-public"
BACKEND = "http://127.0.0.1:3000"
MAX_PRIVATE_BYTES = 64 * 1024
MAX_RESPONSE_BYTES = 256 * 1024
COOKIE_NAME = "__Host-refresh_token"
MAX_DOMAIN_LENGTH = 249
MAX_PORT = 65535
MIN_PASSWORD_BYTES = 32
MAX_PASSWORD_BYTES = 128
MIN_PASSWORD_CHARACTER = 33
MAX_PASSWORD_CHARACTER = 126
MAX_COOKIE_BYTES = 1024
MAX_OWNED_ROOMS = 100
MAX_TOKEN_BYTES = 8192
PRIVATE_FILE_MODE = 0o600
ROOM_REQUEST: dict[str, JsonValue] = {
    "id": "lobby",
    "display_name": "SimplestChat Test Lobby",
    "require_registration": False,
    "max_participants": 30,
    "max_broadcasters": 6,
    "moderated": False,
    "secret": False,
    "lobby_enabled": False,
    "guests_allowed": True,
    "guests_can_broadcast": True,
    "topic": "Public test room. Test data may be reset; do not share sensitive information.",
}


class SeedError(Exception):
    """Only fixed, non-sensitive codes may cross the CLI boundary."""


class Owner(TypedDict):
    """The only credential fields retained in the immutable private owner file."""

    email: str
    password: str


class SeedReport(TypedDict):
    """Mutation indicators shared by the real API and deterministic fixtures."""

    ownerCreated: bool
    roomCreated: bool
    existingSettingsPreserved: bool


class OperationReport(SeedReport):
    """The fixed, credential-free CLI result schema."""

    schemaVersion: int
    passed: bool
    changed: bool
    ownerFileCreated: bool
    refreshSessionRevoked: bool | None
    failure: str | None
    ownerCredentialFile: str


class SeedApi(Protocol):
    """The small authentication and room API used by the seed transaction."""

    token: str | None
    authenticated: bool

    def request(
        self,
        method: str,
        path: str,
        body: Mapping[str, object] | None = None,
    ) -> tuple[int, JsonValue]:
        """Perform one approved request and return its status and decoded body."""
        ...

    def has_refresh_cookie(self) -> bool:
        """Confirm that the accepted session can subsequently be revoked."""
        ...


def require(condition: object, code: str) -> None:
    """Fail with a fixed, credential-free code when a precondition is false."""
    if not condition:
        raise SeedError(code)


def parse_json(raw: str | bytes) -> JsonValue:
    """Reject malformed or ambiguous input without exposing the raw bytes."""
    try:
        return decode_json(raw)
    except DuplicateJsonError:
        code = "duplicate_json_key"
        raise SeedError(code) from None
    except (ValueError, UnicodeError):
        code = "invalid_json"
        raise SeedError(code) from None


def owner_configuration(environment: str, secret_values: JsonValue) -> tuple[str, Owner]:
    """Read a single literal HTTPS origin, never execute or export an env file."""
    candidates = [
        line.partition("=")[2]
        for line in environment.splitlines()
        if line.startswith("ALLOWED_ORIGINS=")
    ]
    require(len(candidates) == 1, "invalid_origin_configuration")
    origin = candidates[0]
    require(
        re.fullmatch(r"https://[a-z0-9.-]+(?::[0-9]{1,5})?/?", origin) is not None,
        "invalid_origin_configuration",
    )
    try:
        parsed = urllib.parse.urlsplit(origin)
        port = parsed.port
    except ValueError:
        code = "invalid_origin_configuration"
        raise SeedError(code) from None
    domain = parsed.hostname or ""
    require(
        len(domain) <= MAX_DOMAIN_LENGTH
        and "." in domain
        and all(
            re.fullmatch(r"[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?", label)
            for label in domain.split(".")
        ),
        "invalid_origin_configuration",
    )
    require(port is None or 1 <= port <= MAX_PORT, "invalid_origin_configuration")
    if not isinstance(secret_values, dict):
        code = "invalid_owner_secret"
        raise SeedError(code)
    password = secret_values.get("owner_password")
    if not isinstance(password, str):
        code = "invalid_owner_secret"
        raise SeedError(code)
    require(
        MIN_PASSWORD_BYTES <= len(password.encode("utf-8")) <= MAX_PASSWORD_BYTES
        and all(
            MIN_PASSWORD_CHARACTER <= ord(character) <= MAX_PASSWORD_CHARACTER
            for character in password
        ),
        "invalid_owner_secret",
    )
    return origin.rstrip("/"), {"email": f"owner@{domain}", "password": password}


def private_stat(information: os.stat_result) -> None:
    """Require a bounded root-owned regular file with no additional hard links."""
    require(
        stat.S_ISREG(information.st_mode)
        and information.st_uid == 0
        and stat.S_IMODE(information.st_mode) == PRIVATE_FILE_MODE
        and information.st_nlink == 1
        and information.st_size <= MAX_PRIVATE_BYTES,
        "unsafe_private_file",
    )


def read_private(directory_fd: int, name: str) -> str:
    """Read one protected file relative to an already verified directory."""
    descriptor = os.open(name, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=directory_fd)
    try:
        private_stat(os.fstat(descriptor))
        with os.fdopen(descriptor, "rb", closefd=False) as source:
            raw = source.read(MAX_PRIVATE_BYTES + 1)
        require(len(raw) <= MAX_PRIVATE_BYTES, "private_file_too_large")
        return raw.decode("utf-8")
    finally:
        os.close(descriptor)


def same_owner(actual: JsonValue, expected: Owner) -> bool:
    """Compare only the exact retained owner schema, using a constant-time secret check."""
    if not isinstance(actual, dict) or set(actual) != {"email", "password"}:
        return False
    password = actual.get("password")
    return (
        actual.get("email") == expected["email"]
        and isinstance(password, str)
        and hmac.compare_digest(password.encode(), expected["password"].encode())
    )


def retain_owner(directory_fd: int, owner: Owner) -> bool:
    """Publish complete credentials exclusively; existing credentials are immutable."""
    try:
        existing = parse_json(read_private(directory_fd, "owner.json"))
    except FileNotFoundError:
        existing = None
    else:
        require(same_owner(existing, owner), "owner_credentials_conflict")
        return False

    pending = f"owner.pending.{secrets.token_hex(12)}"
    descriptor = os.open(
        pending, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=directory_fd
    )
    try:
        with os.fdopen(descriptor, "w", closefd=False, encoding="utf-8") as destination:
            json.dump(owner, destination, separators=(",", ":"))
            _ = destination.write("\n")
            destination.flush()
            os.fsync(descriptor)
        os.link(
            pending,
            "owner.json",
            src_dir_fd=directory_fd,
            dst_dir_fd=directory_fd,
            follow_symlinks=False,
        )
    finally:
        os.close(descriptor)
        os.unlink(pending, dir_fd=directory_fd)
    os.fsync(directory_fd)
    return True


class NoRedirects(urllib.request.HTTPRedirectHandler):
    """Refuse every redirect, including those leaving the fixed loopback backend."""

    @override
    def redirect_request(
        self,
        req: urllib.request.Request,
        fp: IO[bytes],
        code: int,
        msg: str,
        headers: Message,
        newurl: str,
    ) -> None:
        """Never construct a request from a response-supplied redirect location."""
        return


class PrivateCookiePolicy(http.cookiejar.DefaultCookiePolicy):
    """Permit the Secure refresh cookie only on this explicitly private HTTP hop.

    The application always sets Secure cookies. Default CookieJar would omit the
    cookie at logout over loopback HTTP, leaving its database session alive.
    No other host, port, cookie, or redirect is accepted by this client.
    """

    @override
    def set_ok(self, cookie: http.cookiejar.Cookie, request: urllib.request.Request) -> bool:
        """Accept only the bounded, host-only secure refresh cookie on the private hop."""
        return (
            request.full_url.startswith(BACKEND + "/")
            and cookie.name == COOKIE_NAME
            and cookie.domain == "127.0.0.1"
            and not cookie.domain_specified
            and cookie.path == "/"
            and cookie.secure
            and cookie.value is not None
            and len(cookie.value) <= MAX_COOKIE_BYTES
            and super().set_ok(cookie, request)
        )

    @override
    def return_ok_secure(
        self, cookie: http.cookiejar.Cookie, request: urllib.request.Request
    ) -> bool:
        """Permit replay of that secure cookie only to the exact private HTTP origin."""
        return (
            request.full_url.startswith(BACKEND + "/")
            and cookie.name == COOKIE_NAME
            and cookie.domain == "127.0.0.1"
            and cookie.secure
        )


class PrivateApi:
    """A bounded loopback-only client with no proxy, redirect, or credential logging."""

    def __init__(self, origin: str) -> None:
        """Create an isolated refresh-cookie jar and unauthenticated request context."""
        self.origin: str = origin
        self.jar: http.cookiejar.CookieJar = http.cookiejar.CookieJar(policy=PrivateCookiePolicy())
        self.opener: urllib.request.OpenerDirector = urllib.request.build_opener(
            urllib.request.ProxyHandler({}),
            NoRedirects(),
            urllib.request.HTTPCookieProcessor(self.jar),
        )
        self.token: str | None = None
        self.authenticated: bool = False

    def request(
        self,
        method: str,
        path: str,
        body: Mapping[str, object] | None = None,
    ) -> tuple[int, JsonValue]:
        """Execute one allowlisted operation and discard every error response payload."""
        require(
            (method, path)
            in {
                ("POST", "/api/auth/login"),
                ("POST", "/api/auth/register"),
                ("POST", "/api/auth/logout"),
                ("GET", "/api/rooms/mine"),
                ("POST", "/api/rooms"),
            },
            "unapproved_api_operation",
        )
        headers = {"Origin": self.origin, "Accept": "application/json"}
        if self.token is not None:
            headers["Authorization"] = f"Bearer {self.token}"
        data = None
        if body is not None:
            data = json.dumps(body, separators=(",", ":")).encode()
            headers["Content-Type"] = "application/json"
        # The origin is literal HTTP loopback; the path passed the fixed allowlist above.
        request = urllib.request.Request(BACKEND + path, data=data, headers=headers, method=method)  # noqa: S310
        try:
            # The URL is the fixed HTTP loopback backend; proxies and redirects are disabled.
            response = cast("object", self.opener.open(request, timeout=5))
        except urllib.error.HTTPError as error:
            # Error bodies, reasons, headers and URLs are never retained or printed.
            status = error.code
            error.close()
            return status, None
        except (OSError, urllib.error.URLError):
            code = "private_api_unavailable"
            raise SeedError(code) from None
        if not isinstance(response, http.client.HTTPResponse):
            code = "invalid_private_response"
            raise SeedError(code)
        with response:
            raw = response.read(MAX_RESPONSE_BYTES + 1)
            require(len(raw) <= MAX_RESPONSE_BYTES, "api_response_too_large")
            return response.status, parse_json(raw) if raw else None

    def has_refresh_cookie(self) -> bool:
        """Return whether this operation still has a refresh session to revoke."""
        return any(cookie.name == COOKIE_NAME for cookie in self.jar)

    def logout(self) -> bool | None:
        """Revoke the session and always remove local authentication material."""
        try:
            if not self.authenticated and not self.has_refresh_cookie():
                return None
            require(self.has_refresh_cookie(), "refresh_cookie_missing")
            status, _ = self.request("POST", "/api/auth/logout")
            require(
                status == HTTPStatus.NO_CONTENT and not self.has_refresh_cookie(),
                "refresh_session_cleanup_failed",
            )
            return True
        finally:
            self.token = None
            self.jar.clear()


def owned_lobby(rows: JsonValue) -> bool:
    """Find at most one lobby in a bounded, structurally validated owned-room list."""
    if not isinstance(rows, list) or len(rows) > MAX_OWNED_ROOMS:
        code = "invalid_owned_rooms"
        raise SeedError(code)
    matches = 0
    for room in rows:
        if not isinstance(room, dict) or not isinstance(room.get("id"), str):
            code = "invalid_owned_rooms"
            raise SeedError(code)
        matches += room["id"] == "lobby"
    require(matches <= 1, "ambiguous_owned_lobby")
    return bool(matches)


def seed(api: SeedApi, owner: Owner, report: SeedReport) -> None:
    """Authenticate the immutable owner and create only a missing owned lobby."""
    status, authentication = api.request("POST", "/api/auth/login", owner)
    if status == HTTPStatus.UNAUTHORIZED:
        status, authentication = api.request(
            "POST", "/api/auth/register", {**owner, "display_name": "Test room owner"}
        )
        require(status != HTTPStatus.CONFLICT, "owner_credentials_conflict")
        require(status == HTTPStatus.OK, "owner_registration_failed")
        report["ownerCreated"] = True
    else:
        require(status == HTTPStatus.OK, "owner_login_failed")
    api.authenticated = True
    if not isinstance(authentication, dict):
        code = "invalid_authentication_response"
        raise SeedError(code)
    user = authentication.get("user")
    if not isinstance(user, dict):
        code = "invalid_authentication_response"
        raise SeedError(code)
    require(user.get("email") == owner["email"], "owner_identity_mismatch")
    try:
        identifier = user.get("id")
        if not isinstance(identifier, str):
            code = "invalid_owner_identity"
            raise SeedError(code)
        owner_id = str(uuid.UUID(identifier))
    except (ValueError, TypeError, AttributeError):
        code = "invalid_owner_identity"
        raise SeedError(code) from None
    token = authentication.get("token")
    if not isinstance(token, str):
        code = "invalid_authentication_token"
        raise SeedError(code)
    require(
        re.fullmatch(r"[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+", token)
        and len(token) <= MAX_TOKEN_BYTES,
        "invalid_authentication_token",
    )
    api.token = token
    require(api.has_refresh_cookie(), "refresh_cookie_missing")

    status, rooms = api.request("GET", "/api/rooms/mine")
    require(status == HTTPStatus.OK, "owned_rooms_unavailable")
    if owned_lobby(rooms):
        report["existingSettingsPreserved"] = True
        return
    status, room = api.request("POST", "/api/rooms", ROOM_REQUEST)
    require(status == HTTPStatus.OK, "room_create_failed_or_conflict")
    report["roomCreated"] = True
    if not isinstance(room, dict):
        code = "room_ownership_mismatch"
        raise SeedError(code)
    require(
        room.get("id") == "lobby" and room.get("ownerId") == owner_id, "room_ownership_mismatch"
    )
    for field, value in {
        "secret": False,
        "passwordProtected": False,
        "requireRegistration": False,
        "lobbyEnabled": False,
        "guestsAllowed": True,
        "guestsCanBroadcast": True,
    }.items():
        require(room.get(field) is value, "unexpected_new_room_settings")
    status, rooms = api.request("GET", "/api/rooms/mine")
    require(status == HTTPStatus.OK and owned_lobby(rooms), "room_ownership_unconfirmed")


def deadline(_number: int, _frame: FrameType | None) -> None:
    """Interrupt work with a fixed code instead of exposing its request state."""
    code = "operation_deadline"
    raise SeedError(code)


def main() -> int:
    """Run one bounded seed operation and emit only its fixed, credential-free result."""
    report: OperationReport = {
        "schemaVersion": 1,
        "passed": False,
        "changed": False,
        "ownerCreated": False,
        "roomCreated": False,
        "ownerFileCreated": False,
        "existingSettingsPreserved": False,
        "refreshSessionRevoked": None,
        "failure": None,
        "ownerCredentialFile": CONFIG_DIRECTORY + "/owner.json",
    }
    directory_fd = None
    lock_fd = None
    api = None
    try:
        require(len(sys.argv) == 1, "arguments_not_supported")
        require(os.geteuid() == 0, "root_required")
        _ = os.umask(0o077)
        _ = signal.signal(signal.SIGALRM, deadline)
        _ = signal.setitimer(signal.ITIMER_REAL, 55)
        directory_fd = os.open(CONFIG_DIRECTORY, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        information = os.fstat(directory_fd)
        require(
            information.st_uid == 0 and not stat.S_IMODE(information.st_mode) & 0o022,
            "unsafe_configuration_directory",
        )
        lock_fd = os.open(
            "seed.lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600, dir_fd=directory_fd
        )
        private_stat(os.fstat(lock_fd))
        try:
            fcntl.flock(lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            code = "seed_already_running"
            raise SeedError(code) from None
        origin, owner = owner_configuration(
            read_private(directory_fd, "app.env"),
            parse_json(read_private(directory_fd, "secrets.json")),
        )
        report["ownerFileCreated"] = retain_owner(directory_fd, owner)
        api = PrivateApi(origin)
        seed(api, owner, report)
    except SeedError as error:
        report["failure"] = str(error)
    except Exception:  # noqa: BLE001 -- No raw credential-bearing exception may cross the CLI.
        report["failure"] = "seed_operation_failed"
    finally:
        if api is not None:
            try:
                _ = signal.setitimer(signal.ITIMER_REAL, 8)
                report["refreshSessionRevoked"] = api.logout()
            except Exception:  # noqa: BLE001 -- Cleanup errors must remain credential-free.
                report["failure"] = report["failure"] or "refresh_session_cleanup_failed"
                report["refreshSessionRevoked"] = False
        _ = signal.setitimer(signal.ITIMER_REAL, 0)
        if lock_fd is not None:
            os.close(lock_fd)
        if directory_fd is not None:
            os.close(directory_fd)
    report["changed"] = (
        report["ownerCreated"] or report["roomCreated"] or report["ownerFileCreated"]
    )
    report["passed"] = report["failure"] is None and report["refreshSessionRevoked"] is True
    print(json.dumps(report, separators=(",", ":")))  # noqa: T201 -- Fixed sanitized CLI response.
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
