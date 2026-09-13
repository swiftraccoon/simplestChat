#!/usr/bin/env python3
"""Seed the owned lobby through the fixed private API; never print credentials."""

import fcntl
import hmac
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

CONFIG_DIRECTORY = "/etc/simplestchat-public"
BACKEND = "http://127.0.0.1:3000"
MAX_PRIVATE_BYTES = 64 * 1024
MAX_RESPONSE_BYTES = 256 * 1024
COOKIE_NAME = "__Host-refresh_token"
ROOM_REQUEST = {
    "id": "lobby", "display_name": "SimplestChat Test Lobby",
    "require_registration": False, "max_participants": 30, "max_broadcasters": 6,
    "moderated": False, "secret": False, "lobby_enabled": False,
    "guests_allowed": True, "guests_can_broadcast": True,
    "topic": "Public test room. Test data may be reset; do not share sensitive information.",
}


class SeedFailure(Exception):
    """Only fixed, non-sensitive codes may cross the CLI boundary."""


def require(condition, code):
    if not condition:
        raise SeedFailure(code)


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "duplicate_json_key")
        result[key] = value
    return result


def parse_json(raw):
    try:
        return json.loads(raw, object_pairs_hook=unique_object)
    except (ValueError, UnicodeError):
        raise SeedFailure("invalid_json") from None


def owner_configuration(environment, secret_values):
    """Read a single literal HTTPS origin, never execute or export an env file."""
    candidates = [line.partition("=")[2] for line in environment.splitlines()
                  if line.startswith("ALLOWED_ORIGINS=")]
    require(len(candidates) == 1, "invalid_origin_configuration")
    origin = candidates[0]
    require(re.fullmatch(r"https://[a-z0-9.-]+(?::[0-9]{1,5})?/?", origin) is not None,
            "invalid_origin_configuration")
    try:
        parsed = urllib.parse.urlsplit(origin)
        port = parsed.port
    except ValueError:
        raise SeedFailure("invalid_origin_configuration") from None
    domain = parsed.hostname or ""
    require(len(domain) <= 249 and "." in domain and all(
        re.fullmatch(r"[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?", label)
        for label in domain.split(".")), "invalid_origin_configuration")
    require(port is None or 1 <= port <= 65535, "invalid_origin_configuration")
    require(isinstance(secret_values, dict), "invalid_owner_secret")
    password = secret_values.get("owner_password")
    require(isinstance(password, str) and 32 <= len(password.encode("utf-8")) <= 128
            and all(33 <= ord(character) <= 126 for character in password), "invalid_owner_secret")
    return origin.rstrip("/"), {"email": f"owner@{domain}", "password": password}


def private_stat(information):
    require(stat.S_ISREG(information.st_mode) and information.st_uid == 0
            and stat.S_IMODE(information.st_mode) == 0o600 and information.st_nlink == 1
            and information.st_size <= MAX_PRIVATE_BYTES, "unsafe_private_file")


def read_private(directory_fd, name):
    descriptor = os.open(name, os.O_RDONLY | os.O_NOFOLLOW, dir_fd=directory_fd)
    try:
        private_stat(os.fstat(descriptor))
        with os.fdopen(descriptor, "rb", closefd=False) as source:
            raw = source.read(MAX_PRIVATE_BYTES + 1)
        require(len(raw) <= MAX_PRIVATE_BYTES, "private_file_too_large")
        return raw.decode("utf-8")
    finally:
        os.close(descriptor)


def same_owner(actual, expected):
    return (isinstance(actual, dict) and set(actual) == {"email", "password"}
            and actual.get("email") == expected["email"]
            and isinstance(actual.get("password"), str)
            and hmac.compare_digest(actual["password"].encode(), expected["password"].encode()))


def retain_owner(directory_fd, owner):
    """Publish complete credentials exclusively; existing credentials are immutable."""
    try:
        existing = parse_json(read_private(directory_fd, "owner.json"))
    except FileNotFoundError:
        existing = None
    else:
        require(same_owner(existing, owner), "owner_credentials_conflict")
        return False

    pending = f"owner.pending.{secrets.token_hex(12)}"
    descriptor = os.open(pending, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                         0o600, dir_fd=directory_fd)
    try:
        with os.fdopen(descriptor, "w", closefd=False, encoding="utf-8") as destination:
            json.dump(owner, destination, separators=(",", ":"))
            destination.write("\n")
            destination.flush()
            os.fsync(descriptor)
        os.link(pending, "owner.json", src_dir_fd=directory_fd, dst_dir_fd=directory_fd,
                follow_symlinks=False)
    finally:
        os.close(descriptor)
        os.unlink(pending, dir_fd=directory_fd)
    os.fsync(directory_fd)
    return True


class NoRedirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, new_url):
        return None


class PrivateCookiePolicy(http.cookiejar.DefaultCookiePolicy):
    """Permit the Secure refresh cookie only on this explicitly private HTTP hop.

    The application always sets Secure cookies. Default CookieJar would omit the
    cookie at logout over loopback HTTP, leaving its database session alive.
    No other host, port, cookie, or redirect is accepted by this client.
    """

    def set_ok(self, cookie, request):
        return (request.full_url.startswith(BACKEND + "/")
                and cookie.name == COOKIE_NAME and cookie.domain == "127.0.0.1"
                and not cookie.domain_specified and cookie.path == "/" and cookie.secure
                and len(cookie.value) <= 1024 and super().set_ok(cookie, request))

    def return_ok_secure(self, cookie, request):
        return (request.full_url.startswith(BACKEND + "/") and cookie.name == COOKIE_NAME
                and cookie.domain == "127.0.0.1" and cookie.secure)


class PrivateApi:
    def __init__(self, origin):
        self.origin = origin
        self.jar = http.cookiejar.CookieJar(policy=PrivateCookiePolicy())
        self.opener = urllib.request.build_opener(
            urllib.request.ProxyHandler({}), NoRedirects(), urllib.request.HTTPCookieProcessor(self.jar))
        self.token = None
        self.authenticated = False

    def request(self, method, path, body=None):
        require((method, path) in {
            ("POST", "/api/auth/login"), ("POST", "/api/auth/register"),
            ("POST", "/api/auth/logout"), ("GET", "/api/rooms/mine"),
            ("POST", "/api/rooms"),
        }, "unapproved_api_operation")
        headers = {"Origin": self.origin, "Accept": "application/json"}
        if self.token is not None:
            headers["Authorization"] = f"Bearer {self.token}"
        data = None
        if body is not None:
            data = json.dumps(body, separators=(",", ":")).encode()
            headers["Content-Type"] = "application/json"
        request = urllib.request.Request(BACKEND + path, data=data, headers=headers, method=method)
        try:
            response = self.opener.open(request, timeout=5)
        except urllib.error.HTTPError as error:
            # Error bodies, reasons, headers and URLs are never retained or printed.
            status = error.code
            error.close()
            return status, None
        except (OSError, urllib.error.URLError):
            raise SeedFailure("private_api_unavailable") from None
        with response:
            raw = response.read(MAX_RESPONSE_BYTES + 1)
            require(len(raw) <= MAX_RESPONSE_BYTES, "api_response_too_large")
            return response.status, parse_json(raw) if raw else None

    def has_refresh_cookie(self):
        return any(cookie.name == COOKIE_NAME for cookie in self.jar)

    def logout(self):
        try:
            if not self.authenticated and not self.has_refresh_cookie():
                return None
            require(self.has_refresh_cookie(), "refresh_cookie_missing")
            status, _ = self.request("POST", "/api/auth/logout")
            require(status == 204 and not self.has_refresh_cookie(), "refresh_session_cleanup_failed")
            return True
        finally:
            self.token = None
            self.jar.clear()


def owned_lobby(rows):
    require(isinstance(rows, list) and len(rows) <= 100
            and all(isinstance(room, dict) and isinstance(room.get("id"), str) for room in rows),
            "invalid_owned_rooms")
    matching = [room for room in rows if room["id"] == "lobby"]
    require(len(matching) <= 1, "ambiguous_owned_lobby")
    return bool(matching)


def seed(api, owner, report):
    status, authentication = api.request("POST", "/api/auth/login", owner)
    if status == 401:
        status, authentication = api.request("POST", "/api/auth/register",
                                             {**owner, "display_name": "Test room owner"})
        require(status != 409, "owner_credentials_conflict")
        require(status == 200, "owner_registration_failed")
        report["ownerCreated"] = True
    else:
        require(status == 200, "owner_login_failed")
    api.authenticated = True
    require(isinstance(authentication, dict) and isinstance(authentication.get("user"), dict),
            "invalid_authentication_response")
    user = authentication["user"]
    require(user.get("email") == owner["email"], "owner_identity_mismatch")
    try:
        owner_id = str(uuid.UUID(user.get("id", "")))
    except (ValueError, TypeError, AttributeError):
        raise SeedFailure("invalid_owner_identity") from None
    token = authentication.get("token")
    require(isinstance(token, str) and re.fullmatch(r"[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+", token)
            and len(token) <= 8192, "invalid_authentication_token")
    api.token = token
    require(api.has_refresh_cookie(), "refresh_cookie_missing")

    status, rooms = api.request("GET", "/api/rooms/mine")
    require(status == 200, "owned_rooms_unavailable")
    if owned_lobby(rooms):
        report["existingSettingsPreserved"] = True
        return
    status, room = api.request("POST", "/api/rooms", ROOM_REQUEST)
    require(status == 200, "room_create_failed_or_conflict")
    report["roomCreated"] = True
    require(isinstance(room, dict) and room.get("id") == "lobby" and room.get("ownerId") == owner_id,
            "room_ownership_mismatch")
    for field, value in {"secret": False, "passwordProtected": False, "requireRegistration": False,
                         "lobbyEnabled": False, "guestsAllowed": True, "guestsCanBroadcast": True}.items():
        require(room.get(field) is value, "unexpected_new_room_settings")
    status, rooms = api.request("GET", "/api/rooms/mine")
    require(status == 200 and owned_lobby(rooms), "room_ownership_unconfirmed")


def deadline(_number, _frame):
    raise SeedFailure("operation_deadline")


def main():
    report = {"schemaVersion": 1, "passed": False, "changed": False, "ownerCreated": False,
              "roomCreated": False, "ownerFileCreated": False, "existingSettingsPreserved": False,
              "refreshSessionRevoked": None, "failure": None,
              "ownerCredentialFile": CONFIG_DIRECTORY + "/owner.json"}
    directory_fd = None
    lock_fd = None
    api = None
    try:
        require(len(sys.argv) == 1, "arguments_not_supported")
        require(os.geteuid() == 0, "root_required")
        os.umask(0o077)
        signal.signal(signal.SIGALRM, deadline)
        signal.setitimer(signal.ITIMER_REAL, 55)
        directory_fd = os.open(CONFIG_DIRECTORY, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        information = os.fstat(directory_fd)
        require(information.st_uid == 0 and not stat.S_IMODE(information.st_mode) & 0o022,
                "unsafe_configuration_directory")
        lock_fd = os.open("seed.lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600, dir_fd=directory_fd)
        private_stat(os.fstat(lock_fd))
        try:
            fcntl.flock(lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise SeedFailure("seed_already_running") from None
        origin, owner = owner_configuration(read_private(directory_fd, "app.env"),
                                             parse_json(read_private(directory_fd, "secrets.json")))
        report["ownerFileCreated"] = retain_owner(directory_fd, owner)
        api = PrivateApi(origin)
        seed(api, owner, report)
    except SeedFailure as error:
        report["failure"] = str(error)
    except Exception:
        report["failure"] = "seed_operation_failed"
    finally:
        if api is not None:
            try:
                signal.setitimer(signal.ITIMER_REAL, 8)
                report["refreshSessionRevoked"] = api.logout()
            except Exception:
                report["failure"] = report["failure"] or "refresh_session_cleanup_failed"
                report["refreshSessionRevoked"] = False
        signal.setitimer(signal.ITIMER_REAL, 0)
        if lock_fd is not None:
            os.close(lock_fd)
        if directory_fd is not None:
            os.close(directory_fd)
    report["changed"] = report["ownerCreated"] or report["roomCreated"] or report["ownerFileCreated"]
    report["passed"] = report["failure"] is None and report["refreshSessionRevoked"] is True
    print(json.dumps(report, separators=(",", ":")))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
