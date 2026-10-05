"""Protect source history and current cache namespaces during bounded cache maintenance."""

from __future__ import annotations

import io
import sys
import unittest
from dataclasses import replace
from datetime import UTC, datetime, timedelta
from typing import TYPE_CHECKING
from unittest.mock import patch

from test_support import ROOT, at, obj, objects, string, yaml_value

# isort: split
import ci_cache_retention as retention
from security_tools import ToolError

# isort: split
import bounded_process

if TYPE_CHECKING:
    from release_json import JsonObject

NOW = datetime(2026, 10, 5, tzinfo=UTC)
REVISION = "a" * 40


def entry(
    identifier: int, prefix: str = "codeql-analyzed-v2-rust-main-linux-x86_64"
) -> retention.Entry:
    """Build distinct old, ordinary main caches in a fixed test namespace."""
    timestamp = NOW - timedelta(days=2, minutes=-identifier)
    return retention.Entry(
        identifier,
        prefix + "-" + f"{identifier:064x}",
        retention.REFERENCE,
        "f" * 64,
        1024,
        timestamp,
        timestamp,
    )


def metadata(item: retention.Entry) -> JsonObject:
    """Represent only ordinary API fields consumed by the cache metadata parser."""
    return {
        "id": item.identifier,
        "key": item.key,
        "ref": item.reference,
        "version": item.version,
        "size_in_bytes": item.size,
        "created_at": item.created.isoformat().replace("+00:00", "Z"),
        "last_accessed_at": item.accessed.isoformat().replace("+00:00", "Z"),
    }


class RetentionTests(unittest.TestCase):
    """Test concrete deletion decisions without issuing network mutations."""

    def test_latest_caches_unknown_families_and_other_refs_are_protected(self) -> None:
        """Keep two databases and one Buildx entry, leaving all unrecognized caches alone."""
        caches = [entry(index) for index in range(1, 5)]
        caches.extend(entry(index, "buildx-v3-main-Linux-X64") for index in range(5, 8))
        caches += [entry(8, "unrecognized"), replace(entry(9), reference="refs/pull/7/merge")]
        self.assertEqual([item.identifier for item in retention.plan(caches, NOW)], [1, 2, 5, 6])

    def test_namespaces_and_migration_cleanup_remain_precise(self) -> None:
        """A new Linux AMD64 database never removes the latest ARM or untrusted database."""
        caches = [entry(1, "codeql-analyzed-v1-rust"), entry(2), entry(3)]
        caches += [entry(4, "codeql-analyzed-v2-rust-main-linux-aarch64")]
        caches += [entry(5, "codeql-analyzed-v2-rust-untrusted-linux-x86_64")]
        caches += [entry(6, "codeql-analyzed-v2-c-cpp-main-linux-x86_64")]
        self.assertEqual([item.identifier for item in retention.plan(caches, NOW)], [1])
        self.assertEqual(
            retention.family("grype-db-v1-main-" + "a" * 64 + "-2026-10-05"), ("grype-main", 2)
        )
        self.assertIsNone(retention.family("codeql-analyzed-v1-rust-not-a-digest"))
        self.assertIsNotNone(retention.family("native-compiled-v1-asan-main-arm64-" + "a" * 64))

    def test_recent_creation_or_access_wins_over_retention_and_budget(self) -> None:
        """Protect active/new cache entries even when protected data exceeds the soft limit."""
        caches = [entry(index) for index in range(1, 5)]
        caches[0] = replace(caches[0], accessed=NOW - timedelta(minutes=5))
        caches[1] = replace(caches[1], created=NOW + timedelta(minutes=5))
        with patch.object(retention, "SOFT_BYTES", 1):
            self.assertEqual([item.identifier for item in retention.plan(caches, NOW)], [3, 4])

    def test_soft_budget_discards_old_optional_duplicate_but_keeps_latest(self) -> None:
        """A storage target cannot remove the last allowed entry or an unknown cache."""
        caches = [entry(1), entry(2), entry(3, "unknown")]
        with patch.object(retention, "SOFT_BYTES", 1024):
            self.assertEqual([item.identifier for item in retention.plan(caches, NOW)], [1])
        with patch.object(retention, "MAX_DELETIONS", 1):
            self.assertEqual(len(retention.plan([entry(i) for i in range(1, 8)], NOW)), 1)

    def test_inventory_rejects_partial_duplicate_and_oversized_lists(self) -> None:
        """An incomplete API list cannot cause unsafe retention decisions."""
        client = retention.Github()
        responses: list[JsonObject] = [
            {"total_count": 2, "actions_caches": []},
            {"total_count": 2, "actions_caches": [metadata(entry(1)), metadata(entry(1))]},
            {"total_count": retention.MAX_CACHES + 1, "actions_caches": []},
        ]
        for response in responses:
            with (
                patch.object(client, "request", return_value=response),
                self.assertRaises(ToolError),
            ):
                _ = client.inventory()
        with patch.object(
            client,
            "request",
            return_value={"total_count": 1, "actions_caches": [metadata(entry(1))]},
        ):
            self.assertEqual(client.inventory(), [entry(1)])

    def test_apply_requires_exact_successful_main_push_and_current_head(self) -> None:
        """A fork, pull request, failed run or stale SHA cannot authorize deletion."""
        client = retention.Github(apply=True)
        run: JsonObject = {
            "id": 123,
            "name": "CI",
            "event": "push",
            "status": "completed",
            "conclusion": "success",
            "head_branch": "main",
            "head_sha": REVISION,
            "path": ".github/workflows/ci.yml",
            "repository": {"full_name": retention.REPOSITORY},
            "head_repository": {"full_name": retention.REPOSITORY},
        }
        for key, value in (
            ("event", "pull_request"),
            ("conclusion", "failure"),
            ("head_branch", "other"),
            ("head_sha", "b" * 40),
        ):
            with (
                patch.object(client, "request", return_value={**run, key: value}),
                self.assertRaises(ToolError),
            ):
                client.trusted_run(123, REVISION)
        with patch.object(client, "request", side_effect=[run, {"object": {"sha": REVISION}}]):
            client.trusted_run(123, REVISION)
        with (
            patch.object(client, "request", side_effect=[run, {"object": {"sha": "b" * 40}}]),
            self.assertRaisesRegex(ToolError, "stale_run"),
        ):
            client.trusted_run(123, REVISION)

    def test_delete_refreshes_identity_use_and_only_targets_one_cache_id(self) -> None:
        """A changed or recently accessed candidate cannot be removed from an old plan."""
        client = retention.Github(apply=True)
        candidate = entry(1)
        with (
            patch.object(client, "current_head") as head,
            patch.object(client, "request") as request,
            patch.object(
                client, "inventory", return_value=[candidate, entry(2), entry(3)]
            ) as inventory,
        ):
            self.assertTrue(client.remove(candidate, REVISION))
            head.assert_called_once_with(REVISION)
            inventory.assert_called_once_with()
            request.assert_called_once_with("actions/caches/1", delete=True)
            self.assertEqual(client.deleted, [1])
            request.reset_mock()
            inventory.return_value = [
                replace(candidate, accessed=datetime.now(UTC)),
                entry(2),
                entry(3),
            ]
            self.assertFalse(client.remove(candidate, REVISION))
            request.assert_not_called()
            inventory.return_value = [replace(candidate, version="b" * 64), entry(2), entry(3)]
            with self.assertRaisesRegex(ToolError, "changed"):
                _ = client.remove(candidate, REVISION)
            request.assert_not_called()
            inventory.return_value = [candidate]
            self.assertFalse(client.remove(candidate, REVISION))
            request.assert_not_called()

    def test_api_refuses_source_artifact_and_run_deletion_or_dry_run_mutations(self) -> None:
        """An endpoint whitelist makes accidental broader GitHub deletion impossible."""
        for apply in (False, True):
            client = retention.Github(apply=apply)
            with patch.object(bounded_process, "run") as run:
                for suffix in (
                    "actions/runs/123",
                    "actions/artifacts/123",
                    "git/refs/heads/main",
                    "../another/actions/caches/1",
                ):
                    with self.assertRaisesRegex(ToolError, "endpoint"):
                        _ = client.request(suffix, delete=True)
                if not apply:
                    with self.assertRaisesRegex(ToolError, "endpoint"):
                        _ = client.request("actions/caches/1", delete=True)
                run.assert_not_called()

    def test_local_default_is_read_only_and_workflow_requires_successful_main_ci(self) -> None:
        """Maintenance is separate from CI checks and never checks out a PR artifact."""
        client = retention.Github()
        with (
            patch.object(sys, "argv", ["retention"]),
            patch.object(sys, "stdout", io.StringIO()),
            patch.object(retention, "Github", return_value=client),
            patch.object(client, "inventory", return_value=[entry(1)]),
            patch.object(client, "trusted_run") as trusted,
            patch.object(client, "remove") as remove,
        ):
            self.assertEqual(retention.main(), 0)
            trusted.assert_not_called()
            remove.assert_not_called()
        workflow = yaml_value(
            (ROOT / ".github/workflows/cache-retention.yml").read_text(), scalars_as_strings=True
        )
        self.assertEqual(at(workflow, "on", "workflow_run", "workflows"), ["CI"])
        job = obj(workflow, "jobs", "retain")
        for guard in (
            "conclusion == 'success'",
            "event == 'push'",
            "head_branch == 'main'",
            "head_repository.full_name == github.repository",
        ):
            self.assertIn(guard, string(job, "if"))
        self.assertEqual(job["permissions"], {"contents": "read", "actions": "write"})
        steps = objects(job, "steps")
        self.assertEqual(at(steps[0], "with", "persist-credentials"), "false")
        self.assertEqual(at(steps[0], "with", "ref"), "${{ github.event.workflow_run.head_sha }}")
        self.assertIn("--apply", string(steps[1], "run"))


if __name__ == "__main__":
    _ = unittest.main()
