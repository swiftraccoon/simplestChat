"""Ensure security reviews cannot silently widen or survive their expiry."""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from datetime import date
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "build"))

import security_policy as policy
from security_tools import ToolError

TODAY = date(2026, 9, 30)


def review() -> dict[str, str]:
    """Return a complete, narrow, unexpired review fixture."""
    return {
        "scanner": "cargo-audit",
        "fingerprint": "RUSTSEC-2024-0384",
        "scope": "instant@0.1.13",
        "owner": "maintainer",
        "rationale": "Target-specific dependency awaiting upstream replacement.",
        "reachability": "The reviewed runtime target does not compile this dependency.",
        "expires": "2026-10-30",
        "review": "https://example.org/reviews/exact-finding",
    }


class ReviewTests(unittest.TestCase):
    """Exception matches are exact; invalid records fail the whole policy."""

    def test_similar_findings_do_not_inherit_review(self) -> None:
        """Changing the scanner, package version or advisory requires another review."""
        entry = policy.parse_exception(review(), TODAY)
        self.assertTrue(policy.permitted([entry], entry.scanner, entry.fingerprint, entry.scope))
        for scanner, fingerprint, scope in [
            ("cargo-deny", entry.fingerprint, entry.scope),
            (entry.scanner, "RUSTSEC-2024-0385", entry.scope),
            (entry.scanner, entry.fingerprint, "instant@0.1.14"),
        ]:
            with self.subTest(scanner=scanner, fingerprint=fingerprint, scope=scope):
                self.assertFalse(policy.permitted([entry], scanner, fingerprint, scope))

    def test_invalid_or_expired_records_are_rejected(self) -> None:
        """Unowned, broad, expired and unauthenticated review references fail closed."""
        cases = {
            "scanner": "unknown-scanner",
            "fingerprint": "*",
            "scope": "all",
            "owner": "TODO",
            "expires": "2026-09-29",
            "review": "https://user:password@example.org/review",
        }
        for key, value in cases.items():
            with self.subTest(key=key):
                raw = review()
                raw[key] = value
                with self.assertRaises(ToolError):
                    _ = policy.parse_exception(raw, TODAY)

    def test_complete_policy_rejects_duplicate_and_unknown_fields(self) -> None:
        """Neither a later duplicate nor a misspelled scope field is ignored."""
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "exceptions.json"
            _ = path.write_text(
                json.dumps({"schemaVersion": 1, "exceptions": [review(), review()]})
            )
            with self.assertRaisesRegex(ToolError, "duplicate_security_exception"):
                _ = policy.read_exceptions(path, today=TODAY)
            invalid = review()
            invalid["scpoe"] = "other"
            with self.assertRaisesRegex(ToolError, "invalid_tool_record_fields"):
                _ = policy.parse_exception(invalid, TODAY)


if __name__ == "__main__":
    _ = unittest.main()
