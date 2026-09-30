"""Verify ruleset activation cannot accept incomplete or unrelated CI evidence."""

from __future__ import annotations

import copy
import io
import json
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
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
                "warning": "",
                "rules_count": 1,
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
        with self.assertRaisesRegex(ToolError, "ruleset_analysis_not_current"):
            rules.healthy_analyses(window, REVISION)

    def test_newest_category_cannot_fall_back_to_an_older_revision_or_success(self) -> None:
        """Newest-first API order is authoritative before filtering revision or errors."""
        older = self.analyses()
        newest = copy.deepcopy(older)
        for item in newest:
            test_support.obj(item)["commit_sha"] = OTHER
        with self.assertRaisesRegex(ToolError, "ruleset_analysis_not_current"):
            rules.healthy_analyses(newest + older, REVISION)
        newest = copy.deepcopy(older)
        test_support.obj(newest[0])["error"] = "failed"
        with self.assertRaisesRegex(ToolError, "ruleset_analysis_failed"):
            rules.healthy_analyses(newest + older, REVISION)
        rules.healthy_analyses(older + newest, REVISION)

    def test_partial_or_empty_analysis_cannot_activate_protection(self) -> None:
        """Require explicit clean diagnostics and real query coverage for every language."""
        for key, value in (
            ("warning", "partial extraction"),
            ("warning", None),
            ("error", None),
            ("rules_count", 0),
            ("rules_count", None),
            ("rules_count", True),
        ):
            with self.subTest(key=key, value=value):
                records = self.analyses()
                test_support.obj(records[0])[key] = json_value(value)
                with self.assertRaises(ToolError):
                    rules.healthy_analyses(records, REVISION)

    def test_current_head_requires_exact_ref_commit_type_and_sha(self) -> None:
        """A matching SHA under another ref or object type does not establish current main."""
        valid: JsonObject = {
            "ref": "refs/heads/main",
            "object": {"type": "commit", "sha": REVISION},
        }
        with patch.object(rules, "api", return_value=valid) as api:
            rules.require_main_head("owner/repository", REVISION)
            api.assert_called_once_with("owner/repository", "git/ref/heads/main")
        for key, value in (("ref", "refs/heads/other"), ("type", "tag"), ("sha", OTHER)):
            with self.subTest(key=key):
                changed = copy.deepcopy(valid)
                if key == "ref":
                    changed[key] = value
                else:
                    test_support.obj(changed["object"])[key] = value
                with patch.object(rules, "api", return_value=changed), self.assertRaises(ToolError):
                    rules.require_main_head("owner/repository", REVISION)

    def test_apply_rechecks_head_before_every_mutation_and_both_sides_of_readback(self) -> None:
        """Moving main stops subsequent writes or success without reverting protective changes."""
        repository, identifier, policies = rules.load_policy()
        observed = {str(item["name"]): item for item in policies}
        for moved_at in range(len(policies) + 2):
            with self.subTest(moved_at=moved_at):
                states: list[ToolError | None] = [None] * moved_at + [
                    ToolError("ruleset_remote_head_changed")
                ]
                with (
                    redirect_stdout(io.StringIO()),
                    redirect_stderr(io.StringIO()),
                    patch.object(rules, "ready"),
                    patch.object(rules, "inventory", side_effect=[{}, observed]) as inventory,
                    patch.object(rules, "require_main_head", side_effect=states) as head,
                    patch.object(
                        rules, "api", return_value={"id": identifier, "default_branch": "main"}
                    ) as api,
                ):
                    self.assertEqual(rules.main(["apply", "--revision", REVISION]), 1)
                    self.assertEqual(head.call_count, moved_at + 1)
                    self.assertEqual(api.call_count, 1 + min(moved_at, len(policies)))
                    self.assertEqual(
                        inventory.call_count, 2 if moved_at == len(policies) + 1 else 1
                    )
                    self.assertEqual(api.call_args_list[0].args[:2], (repository, ""))

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

    @staticmethod
    def recorded_review() -> JsonObject:
        """Retain the complete current GitHub review-rule shape observed on 2026-09-30."""
        return {
            "id": 24277397,
            "name": "Main pull request review",
            "target": "branch",
            "enforcement": "active",
            "bypass_actors": [
                {"actor_id": 47705353, "actor_type": "User", "bypass_mode": "pull_request"}
            ],
            "conditions": {"ref_name": {"include": ["refs/heads/main"], "exclude": []}},
            "rules": [
                {
                    "type": "pull_request",
                    "parameters": {
                        "allowed_merge_methods": ["merge", "squash", "rebase"],
                        "dismiss_stale_reviews_on_push": True,
                        "require_code_owner_review": False,
                        "require_extra_approval_for_unattributed_changes": True,
                        "require_last_push_approval": True,
                        "required_approving_review_count": 1,
                        "required_review_thread_resolution": True,
                        "required_reviewers": [],
                    },
                }
            ],
        }

    def test_recorded_review_response_is_current_policy_without_normalization(self) -> None:
        """The actual complete API shape must match without dropping nested parameters."""
        _, identifier, policies = rules.load_policy()
        observed = {str(item["name"]): item for item in policies}
        observed["Main pull request review"] = self.recorded_review()
        output = io.StringIO()
        with (
            redirect_stdout(output),
            patch.object(rules, "inventory", return_value=observed),
            patch.object(
                rules, "api", return_value={"id": identifier, "default_branch": "main"}
            ) as api,
        ):
            self.assertEqual(rules.main(["check"]), 0)
        self.assertTrue(test_support.obj(decode_json(output.getvalue()))["matches"])
        self.assertEqual(api.call_count, 1)
        self.assertEqual(len(api.call_args.args), 2)

    def test_changed_missing_or_new_review_parameters_fail_apply_readback(self) -> None:
        """Explicit current API fields cannot hide changed or previously unknown semantics."""
        _, identifier, policies = rules.load_policy()
        cases: tuple[tuple[str, JsonValue], ...] = (
            ("require_extra_approval_for_unattributed_changes", False),
            ("required_reviewers", [{"reviewer": {"id": 1, "type": "Team"}}]),
            ("unreviewed_parameter", True),
            ("required_reviewers", None),
        )
        for key, replacement in cases:
            with self.subTest(key=key, replacement=replacement):
                response = self.recorded_review()
                parameters = test_support.obj(response, "rules", 0, "parameters")
                if replacement is None:
                    _ = parameters.pop(key)
                else:
                    parameters[key] = replacement
                observed = {str(item["name"]): item for item in policies}
                observed["Main pull request review"] = response
                error = io.StringIO()
                with (
                    redirect_stdout(io.StringIO()),
                    redirect_stderr(error),
                    patch.object(rules, "ready"),
                    patch.object(rules, "require_main_head"),
                    patch.object(rules, "inventory", side_effect=[{}, observed]),
                    patch.object(
                        rules, "api", return_value={"id": identifier, "default_branch": "main"}
                    ),
                ):
                    self.assertEqual(rules.main(["apply", "--revision", REVISION]), 1)
                self.assertEqual(
                    error.getvalue(), "Ruleset reconciliation failed: ruleset_readback_differs\n"
                )

    def test_failures_keep_fixed_codes_without_reflecting_other_exception_details(self) -> None:
        """Command failures and readback drift remain distinguishable from private parse data."""
        for failure, code in (
            (ToolError("ruleset_command_failed"), "ruleset_command_failed"),
            (ValueError("private-canary"), "ValueError"),
            (OSError("private-canary"), "OSError"),
        ):
            with self.subTest(code=code):
                output, error = io.StringIO(), io.StringIO()
                with (
                    redirect_stdout(output),
                    redirect_stderr(error),
                    patch.object(rules, "load_policy", side_effect=failure),
                ):
                    self.assertEqual(rules.main(["check"]), 1)
                self.assertEqual(output.getvalue(), "")
                self.assertEqual(error.getvalue(), f"Ruleset reconciliation failed: {code}\n")
                self.assertNotIn("private-canary", error.getvalue())


if __name__ == "__main__":
    _ = unittest.main()
