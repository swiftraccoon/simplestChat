"""Offline one-command deploy tests; no GitHub, SSH, Ansible or service actions."""

# Fixture failures intentionally explain the violated command contract.
# ruff: noqa: EM102, TRY003

from __future__ import annotations

import io
import json
import os
import stat
import tempfile
import time
import unittest
from contextlib import redirect_stdout
from copy import deepcopy
from pathlib import Path
from typing import TYPE_CHECKING, TypedDict, override
from unittest.mock import AsyncMock, MagicMock, patch

from test_support import ROOT

# Bootstrap flat checkout imports before loading helpers.
# isort: split
import release_build as BUILD  # noqa: N812 -- Keep established helper aliases.
import release_deploy as DEPLOY  # noqa: N812 -- Keep established helper aliases.
import release_fetch_controller as FETCH  # noqa: N812 -- Keep established helper aliases.
from release_json import JsonObject, JsonValue, array_value, decode_json, object_value, string_value

if TYPE_CHECKING:
    from collections.abc import Mapping, Sequence

REVISION = "a" * 40
SECRET = "PRIVATE_CONTROLLER_INVENTORY_SENTINEL"  # noqa: S105 -- Non-secret sentinel verifies credential redaction.


def ci_run(**changes: JsonValue) -> JsonObject:
    """Return a trusted CI-run fixture with explicit per-test mutations."""
    return dict(
        {
            "id": 789,
            "name": "CI",
            "path": ".github/workflows/ci.yml",
            "event": "push",
            "head_branch": "main",
            "head_sha": REVISION,
            "status": "completed",
            "conclusion": "success",
            "repository": {"full_name": "owner/repo"},
            "head_repository": {"full_name": "owner/repo"},
        },
        **changes,
    )


def artifact(**changes: JsonValue) -> JsonObject:
    """Return artifact metadata bound to the fixture revision and CI run."""
    return dict(
        {
            "id": 123,
            "expired": False,
            "name": f"simplestchat-production-{REVISION}",
            "size_in_bytes": 12345,
            "digest": "sha256:" + "b" * 64,
            "workflow_run": {
                "id": 789,
                "head_sha": REVISION,
                "repository_id": 42,
                "head_repository_id": 42,
            },
        },
        **changes,
    )


def api_records() -> list[JsonObject]:
    """Return the ordered GitHub API responses for a successful fixture selection."""
    return [
        {"workflow_runs": [ci_run()]},
        ci_run(),
        {"total_count": 1, "artifacts": [artifact()]},
        artifact(),
        ci_run(),
    ]


def inventory() -> JsonObject:
    """Return an isolated inventory with known host and connection variables."""
    return {
        "benchmark_hosts": {"hosts": ["public"]},
        "_meta": {
            "hostvars": {
                "public": {
                    "scpub_domain": "chat.example.test",
                    "ansible_host": "192.0.2.10",
                    "ansible_user": "root",
                    "ansible_ssh_private_key_file": "/private/key",
                    "controller_fixture": SECRET,
                }
            }
        },
    }


class RunConfiguration(TypedDict):
    """The exact subprocess options observed by the command fixture."""

    cwd: Path
    timeout: float
    env: Mapping[str, str] | None
    allow_failure: bool
    capture: bool


type RunCall = tuple[list[str], RunConfiguration]


def hostvars(value: JsonObject) -> JsonObject:
    """Read resolved host variables from the typed inventory fixture."""
    return object_value(object_value(value["_meta"])["hostvars"])


def public_hostvars(value: JsonObject) -> JsonObject:
    """Select the public host variables used by inventory mutation tests."""
    return object_value(hostvars(value)["public"])


def benchmark_hosts(value: JsonObject) -> list[JsonValue]:
    """Read the typed list of fixture benchmark hosts."""
    return array_value(object_value(value["benchmark_hosts"])["hosts"])


class FakeRunner:
    """A deterministic command boundary that never invokes external tools."""

    def __init__(self, fixture: DeployTests, output: Path | None = None) -> None:
        """Initialize explicit fixture or transport state before use."""
        self.fixture: DeployTests = fixture
        self.output: Path | None = output

    def run(  # noqa: PLR0913, PLR0911 -- Explicit boundary options preserve the subprocess contract.
        self,
        argv: Sequence[str],
        *,
        cwd: Path,
        timeout: float = 30,
        env: Mapping[str, str] | None = None,
        allow_failure: bool = False,
        capture: bool = True,
    ) -> tuple[int, str]:
        """Execute the requested command and retain its bounded result."""
        state = self.fixture
        state.calls.append(
            (
                list(argv),
                {
                    "cwd": cwd,
                    "timeout": timeout,
                    "env": env,
                    "allow_failure": allow_failure,
                    "capture": capture,
                },
            )
        )
        if argv[:2] == ["git", "rev-parse"]:
            state.revision_reads += 1
            return 0, REVISION if state.revision_reads == 1 else state.final_revision
        if argv[:2] == ["git", "status"]:
            return 0, " M changed-file" if state.dirty else ""
        if argv[:3] == ["git", "remote", "get-url"]:
            return 0, state.remote
        if argv[:2] == ["git", "check-ignore"]:
            return 0, argv[-1]
        if argv == ["/node", "--version"]:
            return 0, "v22.12.0"
        if argv[0] == "/ansible-inventory":
            state.inventory_reads += 1
            value = state.inventory if state.inventory_reads == 1 else state.final_inventory
            return 0, json.dumps(value)
        if argv[0] == "/ansible-playbook":
            if state.release_error is not None:
                raise state.release_error
            return 0, ""
        if argv[0] == "/node":
            return 0, json.dumps(
                {
                    "passed": state.smoke_passed,
                    "origin": "https://chat.example.test",
                    "room": "lobby",
                }
            )
        raise AssertionError(f"Unexpected test command: {argv}")


class DeployTests(unittest.TestCase):
    """Verify exact CI selection, frozen host identity and single-attempt deployment."""

    def __init__(self, method_name: str = "runTest") -> None:
        """Initialize explicit fixture or transport state before use."""
        super().__init__(method_name)
        self.root: Path = ROOT
        self.inventory_path: Path = ROOT / "inventory.local.yml"
        self.args: DEPLOY.DeployOptions = DEPLOY.DeployOptions()
        self.calls: list[RunCall] = []
        self.inventory: JsonObject = inventory()
        self.final_inventory: JsonObject = inventory()
        self.inventory_reads: int = 0
        self.revision_reads: int = 0
        self.final_revision: str = REVISION
        self.remote: str = ""
        self.dirty: bool = False
        self.release_error: BaseException | None = None
        self.smoke_passed: bool = True

    @override
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="simplestchat-deploy-controller.")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.inventory_path = self.root / "inventory.local.yml"
        _ = self.inventory_path.write_text("unused offline inventory fixture\n")
        self.args = DEPLOY.DeployOptions(
            inventory=str(self.inventory_path),
            repository="owner/repo",
            origin="https://chat.example.test",
            room="lobby",
            limit=None,
            wait_seconds=30,
            install_helpers=False,
            ansible_playbook=None,
        )
        self.calls = []
        self.inventory = inventory()
        self.final_inventory = deepcopy(self.inventory)
        self.inventory_reads = 0
        self.revision_reads = 0
        self.final_revision = REVISION
        self.remote = "https://github.com/owner/repo.git"
        self.dirty = False
        self.release_error = None
        self.smoke_passed = True

    def execute(
        self, records: list[JsonObject] | None = None
    ) -> tuple[JsonObject, MagicMock | AsyncMock]:
        """Deploy the pinned artifact once to a frozen host, then run public verification."""

        def runner(output: Path | None = None) -> FakeRunner:
            return FakeRunner(self, output)

        with (
            patch.object(BUILD, "Runner", side_effect=runner),
            patch.object(
                DEPLOY,
                "controller_tools",
                return_value=("/ansible-playbook", "/ansible-inventory", "/node"),
            ),
            patch.object(
                FETCH, "api", side_effect=records if records is not None else api_records()
            ) as api,
        ):
            old_umask = os.umask(0o077)
            try:
                result = DEPLOY.execute(self.args, self.root)
            finally:
                _ = os.umask(old_umask)
        return result, api

    def deployments(self) -> list[RunCall]:
        """Select recorded Ansible deployment commands for side-effect assertions."""
        return [call for call in self.calls if call[0][0] == "/ansible-playbook"]

    def smokes(self) -> list[RunCall]:
        """Select recorded public verification commands for ordering assertions."""
        return [
            call
            for call in self.calls
            if call[0][:2] == ["/node", str(self.root / "build/public-smoke.mjs")]
        ]

    def test_clean_exact_ci_image_deploys_once_then_smokes_with_private_evidence(self) -> None:
        """Verify clean exact ci image deploys once then smokes with private evidence."""
        report, api = self.execute()
        self.assertTrue(report["passed"] and report["deployed"])
        self.assertEqual(report["artifactId"], 123)
        self.assertEqual(report["ciRunId"], 789)
        self.assertEqual(len(self.deployments()), 1)
        self.assertEqual(len(self.smokes()), 1)
        self.assertEqual(self.revision_reads, 2)
        self.assertEqual(self.inventory_reads, 2)
        command, configuration = self.deployments()[0]
        extra = object_value(decode_json(command[command.index("--extra-vars") + 1]))
        self.assertEqual(
            extra,
            {
                "scpub_release_repository": "owner/repo",
                "scpub_release_artifact_id": 123,
                "scpub_release_expected_revision": REVISION,
                "scpub_release_ci_run": 789,
                "scpub_release_prepared": True,
                "scpub_release_deploy": True,
            },
        )
        self.assertEqual(configuration["timeout"], 2400)
        self.assertEqual(command[command.index("--limit") + 1], "public")
        self.assertTrue(
            self.calls.index(self.deployments()[0]) < self.calls.index(self.smokes()[0])
        )
        self.assertFalse(
            any(call[0][0] in ("docker", "ssh") or "push" in call[0] for call in self.calls)
        )
        self.assertEqual(api.call_count, 5)
        evidence = Path(string_value(report["evidence"]))
        self.assertEqual(stat.S_IMODE(evidence.stat().st_mode), 0o700)
        frozen = Path(command[command.index("-i") + 1])
        self.assertEqual(frozen.parent, evidence)
        self.assertEqual(
            decode_json(frozen.read_text()),
            {"benchmark_hosts": {"hosts": {"public": public_hostvars(self.inventory)}}},
        )
        for path in evidence.iterdir():
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
            if path != frozen:
                self.assertNotIn(SECRET, path.read_text())
        self.assertNotIn(SECRET, json.dumps(report))
        self.assertNotIn(SECRET, json.dumps(command))
        self.assertEqual(decode_json((evidence / "outcome.json").read_text()), report)

    def test_helper_installation_is_only_selected_explicitly(self) -> None:
        """Verify helper installation is only selected explicitly."""
        self.args.install_helpers = True
        report, _ = self.execute()
        self.assertTrue(report["passed"])
        command = self.deployments()[0][0]
        self.assertFalse(
            object_value(decode_json(command[command.index("--extra-vars") + 1]))[
                "scpub_release_prepared"
            ]
        )

    def test_dirty_checkout_or_wrong_origin_never_queries_ci_or_deploys(self) -> None:
        """Verify dirty checkout or wrong origin never queries ci or deploys."""
        for dirty, remote in [
            (True, self.remote),
            (False, "https://github.com/other/repo.git"),
            (False, "https://credential@github.com/owner/repo.git"),
        ]:
            self.dirty, self.remote = dirty, remote
            self.revision_reads = 0
            with self.subTest(dirty=dirty, remote=remote):
                report, api = self.execute()
                self.assertFalse(report["passed"])
                api.assert_not_called()
        self.assertEqual(self.deployments(), [])
        self.assertFalse((self.root / "results").exists())

    def test_checkout_changed_during_ci_wait_cannot_deploy(self) -> None:
        """Verify checkout changed during ci wait cannot deploy."""
        self.final_revision = "c" * 40
        report, _ = self.execute()
        self.assertEqual(report["failureClass"], "checkout_changed")
        self.assertEqual(self.deployments(), [])
        self.assertEqual(report["remoteOutcome"], "not_started")

    def test_same_alias_cannot_switch_connection_details_during_ci_wait(self) -> None:
        """Verify same alias cannot switch connection details during ci wait."""
        for field, value in [
            ("ansible_host", "192.0.2.20"),
            ("ansible_user", "other"),
            ("ansible_ssh_private_key_file", "/different/key"),
            ("ansible_port", 2222),
        ]:
            self.inventory_reads = self.revision_reads = 0
            self.final_inventory = deepcopy(self.inventory)
            public_hostvars(self.final_inventory)[field] = value
            with self.subTest(field=field):
                report, _ = self.execute()
                self.assertEqual(report["failureClass"], "inventory_target_changed")
                self.assertEqual(self.deployments(), [])

    def test_host_group_name_collision_cannot_expand_the_frozen_inventory(self) -> None:
        """Verify host group name collision cannot expand the frozen inventory."""
        benchmark_hosts(self.inventory).append("second")
        self.inventory["public"] = {"hosts": ["public", "second"]}
        hostvars(self.inventory)["second"] = {"scpub_domain": "chat.example.test"}
        self.final_inventory = deepcopy(self.inventory)
        self.args.limit = "public"
        report, _ = self.execute()
        self.assertTrue(report["passed"])
        command = self.deployments()[0][0]
        frozen = object_value(decode_json(Path(command[command.index("-i") + 1]).read_text()))
        self.assertEqual(
            list(object_value(object_value(frozen["benchmark_hosts"])["hosts"])), ["public"]
        )

    def test_multiple_targets_and_wrong_smoke_origin_fail_before_ci(self) -> None:
        """Verify multiple targets and wrong smoke origin fail before ci."""
        for mode in ["multiple", "origin"]:
            self.inventory = inventory()
            self.inventory_reads = self.revision_reads = 0
            if mode == "multiple":
                benchmark_hosts(self.inventory).append("second")
            else:
                public_hostvars(self.inventory)["scpub_domain"] = "other.example.test"
            with self.subTest(mode=mode):
                report, api = self.execute()
                self.assertFalse(report["passed"])
                api.assert_not_called()
        self.assertEqual(self.deployments(), [])

    def test_release_failure_or_interrupt_never_retries_or_runs_smoke(self) -> None:
        """Verify release failure or interrupt never retries or runs smoke."""
        for error in [
            BUILD.BuildError(SECRET),
            DEPLOY.DeployError("interrupted"),
            KeyboardInterrupt(),
        ]:
            self.calls = []
            self.inventory_reads = self.revision_reads = 0
            self.release_error = error
            with self.subTest(error=type(error).__name__):
                report, _ = self.execute()
                self.assertFalse(report["passed"] or report["deployed"])
                self.assertEqual(report["phase"], "deploy")
                self.assertEqual(report["remoteOutcome"], "inspect_if_interrupted")
                self.assertEqual(len(self.deployments()), 1)
                self.assertEqual(self.smokes(), [])
                self.assertNotIn(SECRET, json.dumps(report))

    def test_smoke_failure_retains_successful_deployment_without_retry_or_rollback(self) -> None:
        """Verify smoke failure retains successful deployment without retry or rollback."""
        self.smoke_passed = False
        report, _ = self.execute()
        self.assertFalse(report["passed"])
        self.assertTrue(report["deployed"])
        self.assertEqual(report["failureClass"], "public_smoke_failed")
        self.assertEqual(len(self.deployments()), 1)
        self.assertEqual(len(self.smokes()), 1)

    def test_missing_failed_cancelled_or_expired_ci_never_deploys(self) -> None:
        """Verify missing failed cancelled or expired ci never deploys."""
        failed: list[JsonObject] = [{"workflow_runs": [ci_run()]}, ci_run(conclusion="failure")]
        cancelled: list[JsonObject] = [
            {"workflow_runs": [ci_run()]},
            ci_run(conclusion="cancelled"),
        ]
        expired = api_records()
        expired[3]["expired"] = True
        cases: list[tuple[list[JsonObject], str]] = [
            ([{"workflow_runs": []}], "ci_missing_push_required"),
            (failed, "ci_failed"),
            (cancelled, "ci_failed"),
            (expired, "artifact_identity_mismatch"),
        ]
        for records, code in cases:
            self.inventory_reads = self.revision_reads = 0
            with self.subTest(code=code):
                report, _ = self.execute(records)
                self.assertEqual(report["failureClass"], code)
                self.assertEqual(self.deployments(), [])

    def test_wait_is_bounded_and_pins_one_run_without_dispatching_or_switching(self) -> None:
        """Verify wait is bounded and pins one run without dispatching or switching."""
        now = [0.0]
        records = api_records()
        records.insert(1, ci_run(status="in_progress", conclusion=None))

        def advance(seconds: float) -> None:
            now[0] += seconds

        with (
            patch.object(time, "monotonic", side_effect=lambda: now[0]),
            patch.object(time, "sleep", side_effect=advance) as sleep,
            patch.object(FETCH, "api", side_effect=records) as api,
            redirect_stdout(io.StringIO()),
        ):
            selected = DEPLOY.select_ci_artifact(self.args, REVISION)
        self.assertEqual(selected["ciRunId"], 789)
        sleep.assert_called_once_with(15)
        self.assertTrue(all("dispatch" not in call.args[0] for call in api.call_args_list))
        self.assertEqual(
            sum("/workflows/ci.yml/runs?" in call.args[0] for call in api.call_args_list), 1
        )

        self.args.wait_seconds = 0
        with (
            patch.object(
                FETCH,
                "api",
                side_effect=[
                    {"workflow_runs": [ci_run()]},
                    ci_run(status="queued", conclusion=None),
                ],
            ),
            patch.object(time, "sleep") as sleep,
            self.assertRaisesRegex(DEPLOY.DeployError, "ci_wait_timeout"),
        ):
            _ = DEPLOY.select_ci_artifact(self.args, REVISION)
        sleep.assert_not_called()

    def test_wrong_workflow_revision_repository_or_artifact_cannot_be_selected(self) -> None:
        """Verify wrong workflow revision repository or artifact cannot be selected."""
        changesets: list[JsonObject] = [
            {"event": "pull_request"},
            {"head_branch": "feature"},
            {"head_sha": "b" * 40},
            {"path": ".github/workflows/other.yml"},
            {"name": "Other"},
            {"head_repository": {"full_name": "fork/repo"}},
        ]
        for changes in changesets:
            with (
                self.subTest(changes=changes),
                patch.object(FETCH, "api", return_value={"workflow_runs": [ci_run(**changes)]}),
                self.assertRaises(DEPLOY.DeployError),
            ):
                _ = DEPLOY.select_ci_artifact(self.args, REVISION)
        records = api_records()
        object_value(records[3]["workflow_run"])["id"] = 456
        with patch.object(FETCH, "api", side_effect=records), self.assertRaises(FETCH.FetchError):
            _ = DEPLOY.select_ci_artifact(self.args, REVISION)
        records = api_records()
        records[2]["total_count"] = 2
        with (
            patch.object(FETCH, "api", side_effect=records),
            self.assertRaisesRegex(DEPLOY.DeployError, "ci_artifact_missing_or_ambiguous"),
        ):
            _ = DEPLOY.select_ci_artifact(self.args, REVISION)

    def test_ssh_origin_and_controller_configuration_are_explicit(self) -> None:
        """Verify ssh origin and controller configuration are explicit."""
        for remote in [
            "git@github.com:owner/repo.git",
            "ssh://git@github.com/owner/repo.git",
            "https://github.com/owner/repo",
        ]:
            self.remote = remote
            self.assertEqual(
                DEPLOY.checkout_identity(FakeRunner(self), self.root, "owner/repo"), REVISION
            )
        with patch.dict(
            os.environ,
            {
                "ANSIBLE_HOST_KEY_CHECKING": "False",
                "ANSIBLE_CONFIG": "/unreviewed",
                "GH_TOKEN": SECRET,
            },
        ):
            environment = DEPLOY.ansible_environment(self.root)
        self.assertEqual(environment["ANSIBLE_HOST_KEY_CHECKING"], "True")
        self.assertEqual(environment["ANSIBLE_CONFIG"], str(self.root / "ops/ansible/ansible.cfg"))
        self.assertEqual(environment["GH_TOKEN"], SECRET)

    def test_help_does_not_inspect_git_or_start_any_command(self) -> None:
        """Verify help does not inspect git or start any command."""
        with patch.object(DEPLOY, "execute") as execute, redirect_stdout(io.StringIO()) as output:
            self.assertEqual(DEPLOY.main(["--help"]), 0)
        execute.assert_not_called()
        self.assertIn("--install-helpers", output.getvalue())


if __name__ == "__main__":
    _ = unittest.main()
