"""Offline typed seed/API fixtures; no privileged files or network access."""

from __future__ import annotations

import http.client
import io
import json
import os
import stat
import unittest
import urllib.request
from collections.abc import Iterable, Iterator, Mapping
from dataclasses import dataclass
from email.message import Message
from pathlib import Path
from typing import override
from unittest import mock

from test_support import ROOT

# isort: split

import seed_public_room as seed
from release_json import JsonObject, JsonValue, string_value

OWNER: seed.Owner = {
    "email": "owner@chat.example.com",
    "password": "Private-fixture-password-" + "x" * 32,
}
OWNER_ID = "11111111-1111-4111-8111-111111111111"
AUTH: JsonObject = {
    "token": "aaaa.bbbb.cccc",
    "user": {"id": OWNER_ID, "email": OWNER["email"]},
}
NEW_ROOM: JsonObject = {
    "id": "lobby",
    "ownerId": OWNER_ID,
    "secret": False,
    "passwordProtected": False,
    "requireRegistration": False,
    "lobbyEnabled": False,
    "guestsAllowed": True,
    "guestsCanBroadcast": True,
}
type Response = tuple[int, JsonValue]
type Call = tuple[str, str, Mapping[str, object] | None]


class FakeApi:
    """Replay only the explicitly supplied replies and retain typed request records."""

    def __init__(self, responses: Iterable[Response]) -> None:
        """Start an unauthenticated deterministic API conversation."""
        self.responses: Iterator[Response] = iter(responses)
        self.calls: list[Call] = []
        self.token: str | None = None
        self.authenticated: bool = False

    def request(
        self,
        method: str,
        path: str,
        body: Mapping[str, object] | None = None,
    ) -> Response:
        """Record the exact operation before yielding its next fixture response."""
        self.calls.append((method, path, body))
        return next(self.responses)

    def has_refresh_cookie(self) -> bool:
        """Model a session whose refresh credential is available for cleanup."""
        return True


@dataclass(frozen=True)
class CookieResponse(http.client.HTTPResponse):
    """The cookie jar needs only the response's header interface."""

    fixture_headers: http.client.HTTPMessage

    @override
    def info(self) -> http.client.HTTPMessage:
        """Return synthetic cookie headers without constructing a network response."""
        return self.fixture_headers

    @override
    def close(self) -> None:
        """No transport is owned by this header-only response fixture."""


def report() -> seed.SeedReport:
    """Create an unchanged mutation summary for one seed attempt."""
    return {"ownerCreated": False, "roomCreated": False, "existingSettingsPreserved": False}


def metadata(
    *,
    mode: int = stat.S_IFREG | 0o600,
    uid: int = 0,
    links: int = 1,
    size: int = 100,
) -> os.stat_result:
    """Build real stat-result values without touching a privileged filesystem."""
    return os.stat_result((mode, 0, 0, links, uid, 0, size, 0, 0, 0))


def cookie_response(cookie: str) -> CookieResponse:
    """Wrap a synthetic Set-Cookie header in the cookie jar's response interface."""
    headers = http.client.HTTPMessage()
    headers.add_header("Set-Cookie", cookie)
    return CookieResponse(headers)


class PublicSeedTests(unittest.TestCase):
    """Keep credential parsing, identity enforcement, and cleanup independently typed."""

    def test_literal_origin_and_owner_secret_parsing(self) -> None:
        """Only one literal HTTPS origin and a bounded printable owner secret are accepted."""
        self.assertEqual(
            Path(seed.__file__).resolve(), ROOT / "ops/ansible/files/seed_public_room.py"
        )
        origin, owner = seed.owner_configuration(
            "OTHER=private\nALLOWED_ORIGINS=https://chat.example.com\n",
            {"owner_password": OWNER["password"], "jwt": "private"},
        )
        self.assertEqual(origin, "https://chat.example.com")
        self.assertEqual(owner, OWNER)
        for value in (
            "http://chat.example.com",
            "https://user:secret@chat.example.com",
            "https://chat.example.com/a",
            "https://chat.example.com?secret",
            "https://chat.example.com#secret",
            "https://chat.example.com,https://another.example.com",
            "https://chat.example.com:99999",
            '"https://chat.example.com"',
            "https://chat..example.com",
        ):
            with self.subTest(value=value), self.assertRaises(seed.SeedError):
                _ = seed.owner_configuration(
                    f"ALLOWED_ORIGINS={value}\n", {"owner_password": OWNER["password"]}
                )
        with self.assertRaises(seed.SeedError):
            _ = seed.owner_configuration(
                "ALLOWED_ORIGINS=https://chat.example.com\n" * 2,
                {"owner_password": OWNER["password"]},
            )
        for password in (None, "short", "x" * 129, "x" * 31 + "\n"):
            with self.assertRaises(seed.SeedError):
                _ = seed.owner_configuration(
                    "ALLOWED_ORIGINS=https://chat.example.com", {"owner_password": password}
                )

    def test_duplicate_json_and_changed_owner_credentials_are_rejected(self) -> None:
        """Ambiguous JSON and any changed retained credential never trigger an overwrite."""
        with self.assertRaisesRegex(seed.SeedError, "^duplicate_json_key$"):
            _ = seed.parse_json('{"owner_password":"first","owner_password":"second"}')
        with self.assertRaisesRegex(seed.SeedError, "^invalid_json$"):
            _ = seed.parse_json("not JSON")
        owner: JsonObject = {"email": OWNER["email"], "password": OWNER["password"]}
        self.assertTrue(seed.same_owner(owner, OWNER))
        candidates: list[JsonValue] = [
            None,
            {},
            {**owner, "password": "wrong"},
            {**owner, "email": "other@example.com"},
            {**owner, "extra": "unexpected"},
        ]
        for value in candidates:
            self.assertFalse(seed.same_owner(value, OWNER))
        with (
            mock.patch.object(seed, "read_private", return_value=json.dumps(OWNER)),
            mock.patch.object(os, "open", side_effect=AssertionError("Do not rewrite credentials")),
        ):
            self.assertFalse(seed.retain_owner(123, OWNER))

    def test_private_metadata_requires_one_root_owned_regular_file(self) -> None:
        """Reject loose permissions, foreign ownership, links, symlinks, and oversized files."""
        seed.private_stat(metadata())
        for change in (
            {"uid": 501},
            {"mode": stat.S_IFREG | 0o644},
            {"mode": stat.S_IFLNK | 0o600},
            {"links": 2},
            {"size": 65537},
        ):
            with self.assertRaises(seed.SeedError):
                seed.private_stat(metadata(**change))

    def test_new_owner_and_lobby_confirm_identity_and_ownership(self) -> None:
        """Create only the owned public lobby and independently confirm its ownership."""
        api = FakeApi(
            [(401, None), (200, AUTH), (200, []), (200, NEW_ROOM), (200, [{"id": "lobby"}])]
        )
        outcome = report()
        seed.seed(api, OWNER, outcome)
        self.assertEqual(
            outcome, {"ownerCreated": True, "roomCreated": True, "existingSettingsPreserved": False}
        )
        self.assertEqual(
            api.calls[1],
            ("POST", "/api/auth/register", {**OWNER, "display_name": "Test room owner"}),
        )
        self.assertEqual(api.calls[3], ("POST", "/api/rooms", seed.ROOM_REQUEST))
        self.assertFalse(seed.ROOM_REQUEST["secret"])
        self.assertTrue(seed.ROOM_REQUEST["guests_can_broadcast"])

    def test_existing_owned_room_is_preserved(self) -> None:
        """An existing room's user changes survive without registration or update requests."""
        api = FakeApi(
            [
                (200, AUTH),
                (200, [{"id": "lobby", "secret": True, "display_name": "User changed this"}]),
            ]
        )
        outcome = report()
        seed.seed(api, OWNER, outcome)
        self.assertEqual(
            outcome,
            {"ownerCreated": False, "roomCreated": False, "existingSettingsPreserved": True},
        )
        self.assertEqual(
            [(method, path) for method, path, _body in api.calls],
            [("POST", "/api/auth/login"), ("GET", "/api/rooms/mine")],
        )

    def test_conflicts_and_foreign_rooms_never_trigger_reset(self) -> None:
        """Authentication conflicts and foreign room ownership stop without corrective writes."""
        cases: list[tuple[list[Response], str]] = [
            ([(401, None), (409, None)], "owner_credentials_conflict"),
            ([(200, AUTH), (200, []), (500, None)], "room_create_failed_or_conflict"),
            (
                [(200, AUTH), (200, []), (200, {**NEW_ROOM, "ownerId": "foreign"})],
                "room_ownership_mismatch",
            ),
        ]
        for responses, code in cases:
            api = FakeApi(responses)
            with self.subTest(code=code), self.assertRaisesRegex(seed.SeedError, f"^{code}$"):
                seed.seed(api, OWNER, report())
            self.assertFalse(
                any(
                    method in {"PATCH", "DELETE"} or "password" in path
                    for method, path, _body in api.calls
                )
            )

    def test_cookie_is_restricted_to_exact_private_hop(self) -> None:
        """The secure refresh cookie is replayed only over the fixed loopback connection."""
        api = seed.PrivateApi("https://chat.example.com")
        # These request objects only exercise CookieJar; no opener or socket is used.
        request = urllib.request.Request(seed.BACKEND + "/api/auth/login")  # noqa: S310
        raw = (
            f"{seed.COOKIE_NAME}=private-cookie; HttpOnly; Secure; SameSite=Strict; "
            "Path=/; Max-Age=604800"
        )
        api.jar.extract_cookies(cookie_response(raw), request)
        self.assertTrue(api.has_refresh_cookie())
        for url, allowed in (
            (seed.BACKEND + "/api/auth/logout", True),
            ("http://127.0.0.1:3001/api/auth/logout", False),
            ("http://other.example.com/api/auth/logout", False),
            ("https://127.0.0.1:3000/api/auth/logout", False),
        ):
            target = urllib.request.Request(url)  # noqa: S310 -- Cookie policy fixture, never opened.
            api.jar.add_cookie_header(target)
            self.assertEqual(target.has_header("Cookie"), allowed)
        api.jar.extract_cookies(
            cookie_response(f"{seed.COOKIE_NAME}=; HttpOnly; Secure; Path=/; Max-Age=0"), request
        )
        self.assertFalse(api.has_refresh_cookie())
        with io.BytesIO() as response:
            self.assertIsNone(
                seed.NoRedirects().redirect_request(
                    request, response, 302, "Found", Message(), "https://foreign.example.com"
                )
            )

    def test_client_refuses_unapproved_paths_and_cleans_up_logout_failure(self) -> None:
        """Rejected routes never reach the opener, and failed logout clears local credentials."""
        api = seed.PrivateApi("https://chat.example.com")
        with mock.patch.object(api.opener, "open", side_effect=AssertionError("No network")):
            for method, path in (
                ("GET", "https://foreign.example.com"),
                ("DELETE", "/api/rooms/lobby"),
                ("POST", "/api/auth/password"),
            ):
                with self.assertRaises(seed.SeedError):
                    _ = api.request(method, path)
        api.authenticated = True
        api.token = string_value(AUTH["token"])
        with self.assertRaisesRegex(seed.SeedError, "refresh_cookie_missing"):
            _ = api.logout()
        self.assertIsNone(api.token)


if __name__ == "__main__":
    _ = unittest.main()
