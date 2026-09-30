"""Verify ruleset activation cannot accept incomplete or unrelated CI evidence."""

from __future__ import annotations

import copy
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import test_support

# isort: split
import security_rulesets as rules
from release_json import JsonObject, JsonValue, array_value, decode_json, json_value
from security_tools import ToolError

REVISION = "1" * 40
OTHER = "2" * 40


class RulesetTests(unittest.TestCase):
    """Use synthetic API records; no test contacts or changes GitHub."""

    @staticmethod
    def checks() -> JsonObject:
        """Return one successful check from the selected GitHub Actions revision."""
        return {
            "total_count": 1,
            "check_runs": [
                {
                    "name": rules.GATE,
                    "head_sha": REVISION,
                    "status": "completed",
                    "conclusion": "success",
                    "app": {"id": rules.GITHUB_ACTIONS},
                }
            ],
        }

    @staticmethod
    def analyses() -> list[JsonValue]:
        """Return all five exact security categories, independently of quality scans."""
        return [
            {
                "commit_sha": REVISION,
                "tool": {"name": "CodeQL"},
                "error": "",
                "ref": "refs/heads/main",
                "category": f"/language:{language}/security",
            }
            for language in sorted(rules.LANGUAGES)
        ]

    def test_check_identity_status_issuer_and_completeness(self) -> None:
        """Reject skipped, forged, stale and truncated check records."""
        valid = self.checks()
        rules.healthy_checks(valid, REVISION)
        for key, value in (
            ("name", "other"),
            ("head_sha", OTHER),
            ("status", "queued"),
            ("conclusion", "skipped"),
            ("conclusion", "neutral"),
            ("app", {"id": 7}),
        ):
            with self.subTest(key=key, value=value):
                changed = self.checks()
                item = test_support.obj(test_support.objects(changed, "check_runs")[0])
                item[key] = json_value(value)
                changed["check_runs"] = [item]
                with self.assertRaises(ToolError):
                    rules.healthy_checks(changed, REVISION)
        valid["total_count"] = 2
        with self.assertRaises(ToolError):
            rules.healthy_checks(valid, REVISION)

    def test_duplicate_gate_is_ambiguous(self) -> None:
        """Do not select a passing duplicate beside a competing check."""
        value = self.checks()
        value["check_runs"] = list[JsonValue](test_support.objects(value, "check_runs")) * 2
        value["total_count"] = 2
        with self.assertRaises(ToolError):
            rules.healthy_checks(value, REVISION)

    def test_all_exact_languages_are_required(self) -> None:
        """Advisory, different-commit, missing or failed analysis cannot activate rules."""
        rules.healthy_analyses(self.analyses(), REVISION)
        with self.assertRaises(ToolError):
            rules.healthy_analyses(self.analyses()[:-1], REVISION)
        for key, value in (
            ("commit_sha", OTHER),
            ("ref", "refs/pull/1/merge"),
            ("category", "/language:actions/quality-advisory"),
            ("error", "failed"),
        ):
            with self.subTest(key=key):
                entries = self.analyses()
                item = test_support.obj(entries[0])
                item[key] = value
                entries[0] = item
                with self.assertRaises(ToolError):
                    rules.healthy_analyses(entries, REVISION)

    def test_full_history_window_cannot_hide_missing_current_coverage(self) -> None:
        """Old scans filling the page neither block complete coverage nor replace a language."""
        current = self.analyses()
        old = {**test_support.obj(current[0]), "commit_sha": OTHER}
        window: list[JsonValue] = [*current, *[old] * (rules.PAGE_LIMIT - len(current))]
        rules.healthy_analyses(window, REVISION)
        window[0] = old
        with self.assertRaisesRegex(ToolError, "ruleset_language_coverage"):
            rules.healthy_analyses(window, REVISION)

    def test_maintained_policy_splits_review_from_mandatory_gates(self) -> None:
        """The sole-owner review exception never grants a security-check bypass."""
        repository, identifier, policies = rules.load_policy()
        self.assertEqual(repository, "swiftraccoon/simplestChat")
        self.assertEqual(identifier, 1155881649)
        gates = next(item for item in policies if item["name"] == "Main security gates")
        self.assertEqual(gates["bypass_actors"], [])
        by_type = {
            test_support.string(item["type"]): item for item in test_support.objects(gates, "rules")
        }
        self.assertEqual(
            set(by_type),
            {"deletion", "non_fast_forward", "required_status_checks", "code_scanning"},
        )
        self.assertEqual(
            test_support.at(
                by_type["required_status_checks"], "parameters", "required_status_checks"
            ),
            [{"context": rules.GATE, "integration_id": rules.GITHUB_ACTIONS}],
        )
        self.assertEqual(
            test_support.at(by_type["code_scanning"], "parameters", "code_scanning_tools"),
            [
                {
                    "tool": "CodeQL",
                    "security_alerts_threshold": "high_or_higher",
                    "alerts_threshold": "none",
                }
            ],
        )

    def test_bypass_or_wider_target_fails_before_api(self) -> None:
        """Reject a policy that silently broadens the activation scope."""
        policy = test_support.obj(decode_json(rules.POLICY.read_bytes()))
        for field, value in (("bypass_actors", [{"actor_id": 5}]), ("enforcement", "disabled")):
            with self.subTest(field=field), tempfile.TemporaryDirectory() as temporary:
                changed = copy.deepcopy(policy)
                entries = test_support.objects(changed, "rulesets")
                entries[0][field] = json_value(value)
                changed["rulesets"] = list[JsonValue](entries)
                path = Path(temporary) / "rules.json"
                _ = path.write_text(json.dumps(changed))
                with patch.object(rules, "POLICY", path), self.assertRaises(ToolError):
                    _ = rules.load_policy()

    def test_dirty_checkout_stops_before_network(self) -> None:
        """Local unreviewed changes cannot activate remote rules."""
        with (
            patch.object(rules, "command", side_effect=[(REVISION + "\n").encode(), b" M file"]),
            patch.object(rules, "api") as api,
        ):
            with self.assertRaises(ToolError):
                rules.ready("swiftraccoon/simplestChat", REVISION)
            api.assert_not_called()

    def test_comparison_ignores_metadata_and_rule_order(self) -> None:
        """Repeated apply is idempotent for equivalent GitHub policy responses."""
        _, _, policies = rules.load_policy()
        observed = copy.deepcopy(policies[0])
        observed["id"] = 99
        observed["rules"] = list(reversed(array_value(observed["rules"])))
        self.assertEqual(rules.comparable(observed), rules.comparable(policies[0]))


if __name__ == "__main__":
    _ = unittest.main()
