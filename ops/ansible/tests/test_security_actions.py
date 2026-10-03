"""Require exact action identities and complete public metadata before local approval."""

from __future__ import annotations

import unittest
from pathlib import Path
from tempfile import TemporaryDirectory
from typing import TYPE_CHECKING, cast
from unittest.mock import patch

import test_support

# isort: split
import security_actions as actions
from security_tools import ToolError

if TYPE_CHECKING:
    from release_json import JsonObject, JsonValue

REVISION = "1" * 40
OTHER = "2" * 40
PIN = "Owner/Action/path@" + REVISION
FINDING: JsonObject = {"ghsa_id": "GHSA-aaaa-bbbb-cccc", "withdrawn_at": None}


class ActionDependencyTests(unittest.TestCase):
    """Use inert workflow and metadata fixtures without contacting GitHub."""

    def test_local_action_metadata_outside_github_is_reviewed_in_both_trees(self) -> None:
        """A local composite action cannot hide changed remote pins outside .github."""
        name = "build/actions/example/action.yml"
        source = ("runs:\n  using: composite\n  steps:\n    - uses: " + PIN + "\n").encode()
        with patch.object(actions, "command", side_effect=[(name + "\0").encode(), source]):
            self.assertEqual(actions.inventory(None, REVISION), {PIN})
        with TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / name
            target.parent.mkdir(parents=True)
            _ = target.write_bytes(source)
            self.assertEqual(actions.inventory(root, REVISION), {PIN})

    def test_nested_composite_reusable_and_local_actions(self) -> None:
        """Discover remote uses throughout YAML without treating shell text as actions."""
        source = (
            "jobs:\n  reusable:\n    uses: "
            + PIN
            + "\n  test:\n    steps:\n"
            + "      - uses: ./local\n      - run: 'uses: shell text'\n"
            + "      - uses: other/action@"
            + OTHER
            + "\n"
        )
        self.assertEqual(actions.pins(source.encode()), {PIN, "other/action@" + OTHER})

    def test_aliases_are_bounded_and_unsupported_remote_uses_fail(self) -> None:
        """Cyclic aliases terminate; mutable refs, expressions and container actions fail."""
        self.assertEqual(actions.pins(b"root: &root\n  child: *root\n  uses: ./local\n"), set())
        for value in ("true", "owner/action@main", "${{ inputs.action }}", "docker://image:latest"):
            with self.subTest(value=value), self.assertRaises(ToolError):
                _ = actions.pins(("uses: " + value + "\n").encode())

    def test_unknown_or_disallowed_license_stops_before_advisory_lookup(self) -> None:
        """A missing declaration cannot become an allowed license by default."""
        for value in ("NOASSERTION", "GPL-3.0-only", "MIT AND GPL-3.0-only"):
            with (
                self.subTest(license=value),
                patch.object(actions, "api", return_value={"license": {"spdx_id": value}}),
                patch.object(actions, "advisories") as advisory,
                self.assertRaisesRegex(ToolError, "actions_license_blocked"),
            ):
                _ = actions.review(PIN, {"MIT"})
            advisory.assert_not_called()

    def test_no_advisories_does_not_require_an_invented_version(self) -> None:
        """Commit-only projects pass only after both package scopes have no advisories."""
        with (
            patch.object(actions, "api", return_value={"license": {"spdx_id": "MIT"}}) as api,
            patch.object(actions, "advisories", return_value=[]) as advisory,
            patch.object(actions, "release_versions") as versions,
        ):
            result = actions.review(PIN, {"MIT"})
        self.assertTrue(result["passed"])
        self.assertEqual(result["versions"], [])
        api.assert_called_once_with("/repos/owner/action/license", {"ref": REVISION})
        self.assertEqual(
            [call.args for call in advisory.call_args_list],
            [("owner/action",), ("owner/action/path",)],
        )
        versions.assert_not_called()

    def test_known_advisory_requires_release_binding_and_current_version_check(self) -> None:
        """Historical findings do not block a proven fixed version or authorize an unknown one."""

        def lookup(_package: str, version: str | None = None) -> list[JsonObject]:
            return [] if version == "2.3.4" else [FINDING]

        with (
            patch.object(actions, "api", return_value={"license": {"spdx_id": "MIT"}}),
            patch.object(actions, "advisories", side_effect=lookup) as advisory,
            patch.object(actions, "release_versions", return_value=["2.3.4"]) as versions,
        ):
            result = actions.review(PIN, {"MIT"})
        self.assertTrue(result["passed"])
        versions.assert_called_once_with("owner/action", REVISION)
        self.assertEqual(advisory.call_count, 4)
        with (
            patch.object(actions, "api", return_value={"license": {"spdx_id": "MIT"}}),
            patch.object(actions, "advisories", return_value=[FINDING]),
            patch.object(actions, "release_versions", return_value=["1.0.0"]),
            self.assertRaisesRegex(ToolError, "actions_advisory_blocked"),
        ):
            _ = actions.review(PIN, {"MIT"})

    def test_tag_objects_are_peeled_and_only_full_versions_are_eligible(self) -> None:
        """A tag-object SHA or mutable major alias is not a commit's release version."""
        data = (
            OTHER
            + "\trefs/tags/v2.3.4\n"
            + REVISION
            + "\trefs/tags/v2.3.4^{}\n"
            + REVISION
            + "\trefs/tags/v2\n"
            + REVISION
            + "\trefs/tags/stable\n"
            + OTHER
            + "\trefs/tags/v2.3.5\n"
        )
        with patch.object(actions, "command", return_value=data.encode()):
            self.assertEqual(actions.release_versions("owner/action", REVISION), ["2.3.4"])
        for data in (
            REVISION + "\trefs/tags/v2\n",
            REVISION + "\trefs/tags/v2.3.4\n" + OTHER + "\trefs/tags/v2.3.4^{}\n",
        ):
            with (
                patch.object(actions, "command", return_value=data.encode()),
                self.assertRaises(ToolError),
            ):
                _ = actions.release_versions("owner/action", REVISION)

    def test_reviewed_and_malware_feeds_are_both_required(self) -> None:
        """All severities remain eligible; a missing second feed cannot produce success."""
        with patch.object(actions, "api", side_effect=[[], [FINDING]]) as api:
            self.assertEqual(actions.advisories("owner/action", "1.2.3"), [FINDING])
        self.assertEqual(
            [call.args[1]["type"] for call in api.call_args_list], ["reviewed", "malware"]
        )
        for call in api.call_args_list:
            self.assertEqual(call.args[1]["affects"], "owner/action@1.2.3")
            self.assertNotIn("severity", cast("dict[str, str]", call.args[1]))

    def test_incomplete_malformed_or_withdrawn_results_fail(self) -> None:
        """Do not silently treat truncated or unexpected responses as an empty feed."""
        cases: tuple[list[JsonValue], ...] = (
            [FINDING] * actions.PAGE_SIZE,
            [{}],
            [{"ghsa_id": "GHSA-aaaa-bbbb-cccc", "withdrawn_at": "2026-01-01"}],
        )
        for value in cases:
            with (
                self.subTest(value=value[:1]),
                patch.object(actions, "api", return_value=value),
                self.assertRaises(ToolError),
            ):
                _ = actions.advisories("owner/action")

    def test_api_transport_is_bounded_public_and_nonredirecting(self) -> None:
        """Public lookups cannot inherit curl config or follow responses to another host."""
        with patch.object(actions, "command", return_value=b"[]") as command:
            self.assertEqual(actions.api("/advisories", {"affects": "owner/action@1.2.3"}), [])
        argv = test_support.strings(cast("JsonValue", command.call_args.args[0]))
        self.assertEqual(argv[1], "--disable")
        self.assertIn("--max-time", argv)
        self.assertIn("--max-filesize", argv)
        self.assertNotIn("--location", argv)
        self.assertNotIn("Authorization", " ".join(argv))
        self.assertTrue(argv[-1].startswith("https://api.github.com/advisories?"))
        self.assertIn("owner%2Faction%401.2.3", argv[-1])


if __name__ == "__main__":
    _ = unittest.main()
