"""Offline seed/API fixtures only; no privileged files or network connections."""

from email.message import Message
import importlib.util
from pathlib import Path
import types
import unittest
from unittest import mock
import urllib.request

SOURCE = Path(__file__).resolve().parents[1] / "files/seed-public-room.py"
SPEC = importlib.util.spec_from_file_location("public_seed", SOURCE)
seed = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(seed)
OWNER = {"email": "owner@chat.example.com", "password": "Private-fixture-password-" + "x" * 32}
OWNER_ID = "11111111-1111-4111-8111-111111111111"
AUTH = {"token": "aaaa.bbbb.cccc", "user": {"id": OWNER_ID, "email": OWNER["email"]}}
NEW_ROOM = {"id": "lobby", "ownerId": OWNER_ID, "secret": False, "passwordProtected": False,
            "requireRegistration": False, "lobbyEnabled": False, "guestsAllowed": True,
            "guestsCanBroadcast": True}


class FakeApi:
    def __init__(self, responses):
        self.responses = iter(responses)
        self.calls = []
        self.token = None
        self.authenticated = False

    def request(self, method, path, body=None):
        self.calls.append((method, path, body))
        return next(self.responses)

    def has_refresh_cookie(self):
        return True


def report():
    return {"ownerCreated": False, "roomCreated": False, "existingSettingsPreserved": False}


class PublicSeedTests(unittest.TestCase):
    def test_literal_origin_and_owner_secret_parsing(self):
        origin, owner = seed.owner_configuration("OTHER=private\nALLOWED_ORIGINS=https://chat.example.com\n",
                                                 {"owner_password": OWNER["password"], "jwt": "private"})
        self.assertEqual(origin, "https://chat.example.com")
        self.assertEqual(owner, OWNER)
        for value in ["http://chat.example.com", "https://user:secret@chat.example.com",
                      "https://chat.example.com/a", "https://chat.example.com?secret", "https://chat.example.com#secret",
                      "https://chat.example.com,https://another.example.com", "https://chat.example.com:99999",
                      '"https://chat.example.com"', "https://chat..example.com"]:
            with self.subTest(value=value), self.assertRaises(seed.SeedFailure):
                seed.owner_configuration(f"ALLOWED_ORIGINS={value}\n", {"owner_password": OWNER["password"]})
        with self.assertRaises(seed.SeedFailure):
            seed.owner_configuration("ALLOWED_ORIGINS=https://chat.example.com\n" * 2, {"owner_password": OWNER["password"]})
        for password in [None, "short", "x" * 129, "x" * 31 + "\n"]:
            with self.assertRaises(seed.SeedFailure):
                seed.owner_configuration("ALLOWED_ORIGINS=https://chat.example.com", {"owner_password": password})

    def test_duplicate_json_keys_and_changed_owner_credentials_are_rejected(self):
        with self.assertRaises(seed.SeedFailure):
            seed.parse_json('{"owner_password":"first","owner_password":"second"}')
        self.assertTrue(seed.same_owner(dict(OWNER), OWNER))
        for value in [None, {}, {**OWNER, "password": "wrong"}, {**OWNER, "email": "other@example.com"},
                      {**OWNER, "extra": "unexpected"}]:
            self.assertFalse(seed.same_owner(value, OWNER))
        with mock.patch.object(seed, "read_private", return_value=seed.json.dumps(OWNER)), \
                mock.patch.object(seed.os, "open", side_effect=AssertionError("Do not rewrite existing credentials")):
            self.assertFalse(seed.retain_owner(123, OWNER))

    def test_private_metadata_requires_single_root_owned_regular_0600_file(self):
        metadata = {"st_mode": seed.stat.S_IFREG | 0o600, "st_uid": 0, "st_nlink": 1, "st_size": 100}
        seed.private_stat(types.SimpleNamespace(**metadata))
        for change in [{"st_uid": 501}, {"st_mode": seed.stat.S_IFREG | 0o644},
                       {"st_mode": seed.stat.S_IFLNK | 0o600}, {"st_nlink": 2}, {"st_size": 65537}]:
            with self.assertRaises(seed.SeedFailure):
                seed.private_stat(types.SimpleNamespace(**{**metadata, **change}))

    def test_new_owner_and_lobby_use_real_api_fields_and_confirm_ownership(self):
        api = FakeApi([(401, None), (200, AUTH), (200, []), (200, NEW_ROOM), (200, [{"id": "lobby"}])])
        outcome = report()
        seed.seed(api, OWNER, outcome)
        self.assertEqual(outcome, {"ownerCreated": True, "roomCreated": True, "existingSettingsPreserved": False})
        self.assertEqual(api.calls[1], ("POST", "/api/auth/register", {**OWNER, "display_name": "Test room owner"}))
        self.assertEqual(api.calls[3], ("POST", "/api/rooms", seed.ROOM_REQUEST))
        self.assertFalse(seed.ROOM_REQUEST["secret"])
        self.assertTrue(seed.ROOM_REQUEST["guests_can_broadcast"])

    def test_existing_owned_room_is_preserved_without_registration_or_update(self):
        api = FakeApi([(200, AUTH), (200, [{"id": "lobby", "secret": True, "display_name": "User changed this"}])])
        outcome = report()
        seed.seed(api, OWNER, outcome)
        self.assertEqual(outcome, {"ownerCreated": False, "roomCreated": False, "existingSettingsPreserved": True})
        self.assertEqual([call[:2] for call in api.calls], [("POST", "/api/auth/login"), ("GET", "/api/rooms/mine")])

    def test_wrong_password_conflicts_and_foreign_rooms_never_trigger_reset_or_mutation(self):
        cases = [([(401, None), (409, None)], "owner_credentials_conflict"),
                 ([(200, AUTH), (200, []), (500, None)], "room_create_failed_or_conflict"),
                 ([(200, AUTH), (200, []), (200, {**NEW_ROOM, "ownerId": "foreign"})], "room_ownership_mismatch")]
        for responses, code in cases:
            api = FakeApi(responses)
            with self.subTest(code=code), self.assertRaisesRegex(seed.SeedFailure, f"^{code}$"):
                seed.seed(api, OWNER, report())
            self.assertFalse(any(call[0] in ["PATCH", "DELETE"] or "password" in call[1] for call in api.calls))

    def test_refresh_cookie_is_restricted_to_exact_private_hop_and_cleared_by_logout_response(self):
        api = seed.PrivateApi("https://chat.example.com")
        request = urllib.request.Request(seed.BACKEND + "/api/auth/login")

        def response(cookie):
            headers = Message()
            headers.add_header("Set-Cookie", cookie)
            return types.SimpleNamespace(info=lambda: headers)

        raw = f"{seed.COOKIE_NAME}=private-cookie; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=604800"
        api.jar.extract_cookies(response(raw), request)
        self.assertTrue(api.has_refresh_cookie())
        for url, allowed in [(seed.BACKEND + "/api/auth/logout", True),
                             ("http://127.0.0.1:3001/api/auth/logout", False),
                             ("http://other.example.com/api/auth/logout", False),
                             ("https://127.0.0.1:3000/api/auth/logout", False)]:
            target = urllib.request.Request(url)
            api.jar.add_cookie_header(target)
            self.assertEqual(target.has_header("Cookie"), allowed)
        api.jar.extract_cookies(response(f"{seed.COOKIE_NAME}=; HttpOnly; Secure; Path=/; Max-Age=0"), request)
        self.assertFalse(api.has_refresh_cookie())
        self.assertIsNone(seed.NoRedirects().redirect_request(None, None, 302, None, None, "https://foreign.example.com"))

    def test_client_refuses_arbitrary_paths_before_network_and_logout_retains_failure(self):
        api = seed.PrivateApi("https://chat.example.com")
        with mock.patch.object(api.opener, "open", side_effect=AssertionError("No network")):
            for method, path in [("GET", "https://foreign.example.com"), ("DELETE", "/api/rooms/lobby"),
                                 ("POST", "/api/auth/password")]:
                with self.assertRaises(seed.SeedFailure):
                    api.request(method, path)
        api.authenticated = True
        api.token = "private-token"
        with self.assertRaisesRegex(seed.SeedFailure, "refresh_cookie_missing"):
            api.logout()
        self.assertIsNone(api.token)
        self.assertEqual(list(api.jar), [])


if __name__ == "__main__":
    unittest.main()
