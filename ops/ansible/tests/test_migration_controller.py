"""Offline migration boundaries; no GitHub, SSH, Ansible, HTTP or service actions."""

from __future__ import annotations

import json
import os
import stat
import tempfile
import time
import unittest
from copy import deepcopy
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, override
from unittest.mock import patch
from urllib.parse import urlsplit

from test_support import ROOT, array, obj, objects, yaml_value

# isort: split
import migration_controller as migration
import release_attestation
import release_build
import release_deploy
import release_fetch_controller
from release_json import JsonObject, decode_json, string_value

if TYPE_CHECKING:
    from collections.abc import Mapping, Sequence

REVISION = "a" * 40
SOURCE_ORIGIN = "https://chat.example.test"
TARGET_ORIGIN = "https://next.chat.example.test"
PRIVATE_VALUE = "PRIVATE_MIGRATION_FIXTURE_VALUE"


def graph(address: str, domain: str) -> JsonObject:
    """Return one explicit target, deliberately using the same alias in both inventories."""
    return {
        "benchmark_hosts": {"hosts": ["public"]},
        "_meta": {
            "hostvars": {
                "public": {
                    "ansible_host": address,
                    "ansible_user": "root",
                    "ansible_ssh_private_key_file": "/private/fixture/key",
                    "scpub_domain": domain,
                    "scpub_turn_enabled": True,
                    "scpub_backup_enabled": True,
                    "scmon_enabled": True,
                    "private_fixture": PRIVATE_VALUE,
                }
            }
        },
    }


@dataclass
class Command:
    """Retain bounded explicit command details for safety and ordering assertions."""

    argv: list[str]
    environment: Mapping[str, str] | None
    timeout: float


class FakeRunner:
    """A deterministic transport that rejects every unexpected external operation."""

    def __init__(self, fixture: MigrationTests, output: Path | None = None) -> None:
        """Bind command recording to this fixture without starting a process."""
        self.fixture: MigrationTests = fixture
        self.output: Path | None = output

    def run(  # noqa: C901, PLR0911, PLR0912, PLR0913 -- One explicit offline transport dispatcher.
        self,
        argv: Sequence[str],
        *,
        cwd: Path,
        timeout: float = 30,
        env: Mapping[str, str] | None = None,
        allow_failure: bool = False,
        capture: bool = True,
    ) -> tuple[int, str]:
        """Simulate selected read-only inputs and explicitly requested playbook outcomes."""
        del cwd, allow_failure, capture
        state = self.fixture
        state.calls.append(Command(list(argv), env, timeout))
        if argv[:2] == ["git", "rev-parse"]:
            return 0, REVISION
        if argv[:2] == ["git", "status"]:
            return 0, " M changed" if state.dirty else ""
        if argv[:3] == ["git", "remote", "get-url"]:
            return 0, state.repository_url
        if argv[:2] == ["git", "symbolic-ref"]:
            return 0, state.branch
        if argv[:2] == ["git", "check-ignore"]:
            if argv[-1] == state.args.source_inventory and not state.activation_ignored:
                return 1, ""
            return 0, argv[-1]
        if argv[0] == "/ansible-inventory":
            selected = argv[argv.index("-i") + 1]
            state.inventory_reads[selected] = state.inventory_reads.get(selected, 0) + 1
            value = deepcopy(state.inventories[selected])
            if state.change_inventory and state.inventory_reads[selected] > 1:
                obj(value, "_meta", "hostvars", "public")["ansible_host"] = "192.0.2.77"
            return 0, json.dumps(value)
        if argv[0] == "/ansible-playbook":
            extra = obj(decode_json(argv[argv.index("--extra-vars") + 1]))
            action = string_value(extra.get("scmig_action", Path(argv[3]).name))
            state.actions.append(action)
            if action == "inspect":
                release_build.write_json(
                    Path(string_value(extra["scmig_evidence"])) / "inspection.json",
                    state.inspection,
                )
            if action in state.fail_actions:
                message = "Private fixture command failure"
                raise release_build.BuildError(message)
            if action == "finalize-source" and state.change_active_inventory:
                _ = Path(state.args.source_inventory).write_text("independent operator update\n")
            return 0, ""
        if argv[:3] == ["gh", "variable", "set"]:
            if state.fail_canary_update:
                message = "Fixture canary update failure"
                raise release_build.BuildError(message)
            state.canary_value = argv[argv.index("--body") + 1]
            return 0, ""
        if argv[0] == "/curl":
            if state.fail_http:
                message = "Private fixture network failure"
                raise release_build.BuildError(message)
            path = urlsplit(argv[-1]).path
            body = (
                state.homepage
                if path == "/"
                else state.script
                if path == "/assets/index-test.js"
                else json.dumps(
                    {"status": "ok" if path == "/health" else "ready"}
                    if path in ("/health", "/ready")
                    else {"auth": True}
                )
            )
            _ = Path(argv[argv.index("--output") + 1]).write_text(body)
            return 0, state.http_status
        message = "Unexpected fixture command: " + repr(list(argv))
        raise AssertionError(message)


class MigrationTests(unittest.TestCase):
    """Verify trust-before-writes, independent target binding and the cutover boundary."""

    def __init__(self, method_name: str = "runTest") -> None:
        """Declare explicit fixture state for strict typed test helpers."""
        super().__init__(method_name)
        self.root: Path = ROOT
        self.args: migration.MigrationOptions = migration.MigrationOptions()
        self.inventories: dict[str, JsonObject] = {}
        self.inventory_reads: dict[str, int] = {}
        self.inspection: JsonObject = {}
        self.calls: list[Command] = []
        self.actions: list[str] = []
        self.fail_actions: set[str] = set()
        self.fail_http: bool = False
        self.http_status: str = "200"
        self.change_inventory: bool = False
        self.dirty: bool = False
        self.branch: str = "main"
        self.repository_url: str = "https://github.com/owner/repo.git"
        self.activation_ignored: bool = True
        self.change_active_inventory: bool = False
        self.canary_value: str = SOURCE_ORIGIN
        self.canary_reads: int = 0
        self.fail_canary_update: bool = False
        self.homepage: str = (
            '<!doctype html><html><script type="module" src="/assets/index-test.js">'
            + "</script></html>"
        )
        self.script: str = f'const telemetry=new T("{REVISION}");'

    @override
    def setUp(self) -> None:
        """Create only private local fixtures; every external boundary stays mocked."""
        temporary = tempfile.TemporaryDirectory(prefix="migration-controller.")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        source, target = self.root / "source.yml", self.root / "target.yml"
        for path in (source, target):
            _ = path.write_text("offline fixture\n")
        self.args = migration.MigrationOptions(
            inventory=str(target),
            source_inventory=str(source),
            repository="owner/repo",
            origin=TARGET_ORIGIN,
            source_origin=SOURCE_ORIGIN,
            wait_seconds=0,
        )
        self.inventories = {
            str(source): graph("192.0.2.10", "chat.example.test"),
            str(target): graph("192.0.2.11", "next.chat.example.test"),
        }
        self.inspection = {
            "source": {
                "machineId": "1" * 32,
                "domain": "chat.example.test",
                "rpId": "chat.example.test",
                "revision": "b" * 40,
            },
            "target": {"machineId": "2" * 32, "domain": None},
        }

    def execute(self, *, ci_error: bool = False, attestation_error: bool = False) -> JsonObject:
        """Run the actual controller transaction through deterministic external boundaries."""
        envelope: JsonObject = {"artifactId": 123, "ciRunId": 789, "revision": REVISION}

        def verified(_url: str, _path: Path, value: JsonObject) -> JsonObject:
            if attestation_error:
                message = "fixture_attestation_failed"
                raise ValueError(message)
            return value

        def runner(output: Path | None = None) -> FakeRunner:
            return FakeRunner(self, output)

        def api(endpoint: str) -> JsonObject:
            self.assertEqual(endpoint, "repos/owner/repo/actions/variables/CANARY_ORIGIN")
            self.canary_reads += 1
            return {"name": "CANARY_ORIGIN", "value": self.canary_value}

        with (
            patch.object(release_build, "Runner", side_effect=runner),
            patch.object(
                migration,
                "controller_tools",
                return_value=("/ansible-playbook", "/ansible-inventory", "/curl"),
            ),
            patch.object(
                release_deploy,
                "select_ci_artifact",
                return_value=envelope,
                side_effect=release_deploy.DeployError("ci_failed") if ci_error else None,
            ),
            patch.object(
                release_fetch_controller, "download_url", return_value="https://fixture.invalid"
            ),
            patch.object(release_attestation, "fetch_verify", side_effect=verified),
            patch.object(release_fetch_controller, "api", side_effect=api),
        ):
            previous = os.umask(0o077)
            try:
                return migration.execute(self.args, self.root)
            finally:
                _ = os.umask(previous)

    def test_successful_move_preserves_rp_and_freezes_private_targets(self) -> None:
        """Same local aliases remain two distinct machines and preserve exact RP identity."""
        report = self.execute()
        self.assertTrue(report["passed"])
        self.assertEqual(
            self.actions,
            [
                "inspect",
                "site.yml",
                "bootstrap",
                "stage",
                "public.yml",
                "transfer",
                "launch",
                "turn.yml",
                "turn.yml",
                "backup.yml",
                "monitoring.yml",
                "finalize-source",
            ],
        )
        evidence = Path(string_value(report["evidence"]))
        self.assertEqual(stat.S_IMODE(evidence.stat().st_mode), 0o700)
        for path in evidence.iterdir():
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o700 if path.is_dir() else 0o600)
        frozen = obj(
            decode_json((evidence / "migration-inventory.json").read_text()),
            "benchmark_hosts",
            "hosts",
        )
        self.assertEqual(set(frozen), {"migration_source", "migration_target"})
        self.assertNotEqual(
            obj(frozen, "migration_source")["ansible_host"],
            obj(frozen, "migration_target")["ansible_host"],
        )
        self.assertNotIn(PRIVATE_VALUE, json.dumps(report))
        for call in self.calls:
            if call.argv[0] != "/ansible-playbook":
                continue
            extra = obj(decode_json(call.argv[call.argv.index("--extra-vars") + 1]))
            self.assertNotIn(PRIVATE_VALUE, json.dumps(call.argv))
            if extra.get("scmig_action") != "inspect":
                self.assertEqual(extra["scpub_webauthn_rp_id"], "chat.example.test")
            self.assertEqual(extra["scpub_release_expected_revision"], REVISION)
            if Path(call.argv[3]).name == "public.yml":
                self.assertIs(extra["scpub_turn_enabled"], expr2=False)
                self.assertEqual(call.argv[call.argv.index("--limit") + 1], "migration_target")
        self.assertEqual(obj(decode_json((evidence / "outcome.json").read_text())), report)
        variables = obj(decode_json((evidence / "variables.json").read_text()))
        self.assertEqual(variables["scmig_run_id"], report["runId"])
        self.assertEqual(variables["scpub_webauthn_rp_id"], "chat.example.test")

    def test_transfer_deadline_covers_both_units_and_recovery(self) -> None:
        """The controller must not time out while a bounded data unit is still working."""
        report = self.execute()
        self.assertTrue(report["passed"])
        transfer = next(
            call
            for call in self.calls
            if call.argv[0] == "/ansible-playbook"
            and obj(decode_json(call.argv[call.argv.index("--extra-vars") + 1])).get("scmig_action")
            == "transfer"
        )
        plays = objects(yaml_value((ROOT / "ops/ansible/migration-data.yml").read_text()))
        seconds = 0
        for play in plays:
            for task in objects(play, "tasks"):
                if (
                    task.get("when") != "scmig_action == 'transfer'"
                    or "ansible.builtin.command" not in task
                ):
                    continue
                argv = array(task, "ansible.builtin.command", "argv")
                for argument in argv:
                    if isinstance(argument, str) and argument.startswith(
                        ("--property=RuntimeMaxSec=", "--property=TimeoutStopSec=")
                    ):
                        seconds += int(argument.rsplit("=", 1)[-1])
        self.assertGreater(seconds, 0)
        self.assertGreater(transfer.timeout, seconds + 300)

    def test_ci_or_attestation_failure_precedes_all_host_calls(self) -> None:
        """No inspection, preparation or host mutation is attempted with untrusted release bytes."""
        for selectors in ({"ci_error": True}, {"attestation_error": True}):
            with self.subTest(selectors=selectors):
                report = self.execute(**selectors)
                self.assertFalse(report["passed"])
                self.assertEqual(self.actions, [])

    def test_wrong_inventory_origin_and_same_address_fail_before_host_calls(self) -> None:
        """The inventory target must match its explicit origin and be a different host."""
        target = obj(self.inventories[self.args.inventory], "_meta", "hostvars", "public")
        for field, value in (
            ("scpub_domain", "wrong.example.test"),
            ("ansible_host", "192.0.2.10"),
        ):
            old = target[field]
            target[field] = value
            with self.subTest(field=field):
                report = self.execute()
                self.assertFalse(report["passed"])
                self.assertEqual(self.actions, [])
            target[field] = old

    def test_inventory_changes_during_attestation_are_rejected(self) -> None:
        """Re-reading both original targets prevents redirecting a verified operation."""
        self.change_inventory = True
        report = self.execute()
        self.assertEqual(report["failureClass"], "inventory_target_changed")
        self.assertEqual(self.actions, [])

    def test_physical_alias_or_incompatible_rp_stops_after_read_only_inspection(self) -> None:
        """Different DNS selectors do not prove separate machines or RP compatibility."""
        for section, field, value in (
            ("target", "machineId", "1" * 32),
            ("source", "rpId", "unrelated.example.test"),
            ("source", "domain", "wrong.example.test"),
        ):
            old = obj(self.inspection, section)[field]
            obj(self.inspection, section)[field] = value
            with self.subTest(field=field):
                self.actions = []
                report = self.execute()
                self.assertFalse(report["passed"])
                self.assertEqual(self.actions, ["inspect"])
            obj(self.inspection, section)[field] = old

    def test_transfer_failure_stops_target_before_resuming_source(self) -> None:
        """Recovery never starts two writable copies after a partial transfer failure."""
        self.fail_actions = {"transfer"}
        report = self.execute()
        self.assertFalse(report["passed"])
        self.assertEqual(self.actions[-3:], ["transfer", "stop-target", "resume-source"])
        self.assertEqual(report["recovery"], "source_resumed_target_stopped")
        self.assertFalse(report["destinationStartupAttempted"])

    def test_failed_target_stop_never_resumes_source(self) -> None:
        """Uncertain destination shutdown leaves recovery explicit, never a split brain."""
        self.fail_actions = {"transfer", "stop-target"}
        report = self.execute()
        self.assertFalse(report["passed"])
        self.assertNotIn("resume-source", self.actions)
        self.assertEqual(report["recovery"], "manual_recovery_required")

    def test_ambiguous_launch_failure_never_resumes_source(self) -> None:
        """The startup attempt is durable even if the launcher transport fails."""
        self.fail_actions = {"launch"}
        report = self.execute()
        self.assertTrue(report["destinationStartupAttempted"])
        self.assertFalse(report["destinationStarted"])
        self.assertNotIn("resume-source", self.actions)
        self.assertNotIn("stop-target", self.actions)
        evidence = Path(string_value(report["evidence"]))
        checkpoints = [obj(decode_json(path.read_text())) for path in evidence.glob("phase-*.json")]
        launch = next(item for item in checkpoints if item["phase"] == "launch")
        self.assertTrue(launch["destinationStartupAttempted"])

    def test_public_verification_failure_keeps_source_frozen(self) -> None:
        """A live destination may already accept writes before public checks fail."""
        self.fail_http = True
        report = self.execute()
        self.assertFalse(report["passed"])
        self.assertTrue(report["destinationStarted"])
        self.assertNotIn("resume-source", self.actions)
        self.assertNotIn("finalize-source", self.actions)

    def test_public_checks_are_only_bounded_tls_gets_without_chat_or_redirects(self) -> None:
        """Actual emitted network commands contain no message or credential operation."""
        self.assertTrue(self.execute()["passed"])
        calls = [call for call in self.calls if call.argv[0] == "/curl"]
        self.assertEqual(len(calls), len(migration.GET_PATHS) + 1)
        for call in calls:
            self.assertEqual(call.argv[1], "--disable")
            self.assertEqual(call.argv[call.argv.index("--request") + 1], "GET")
            self.assertIn(
                call.argv[-1],
                [TARGET_ORIGIN + path for path in (*migration.GET_PATHS, "/assets/index-test.js")],
            )
            self.assertLessEqual(call.timeout, migration.HTTP_SECONDS)
            self.assertLessEqual(
                float(call.argv[call.argv.index("--max-time") + 1]), migration.REQUEST_SECONDS
            )
            self.assertLessEqual(
                int(call.argv[call.argv.index("--retry-max-time") + 1]), migration.HTTP_SECONDS
            )
            self.assertIn("--retry-all-errors", call.argv)
            self.assertLessEqual(
                int(call.argv[call.argv.index("--max-filesize") + 1]), 2 * 1024 * 1024
            )
            for prohibited in ("--insecure", "--location", "--data", "--cookie", "--netrc"):
                self.assertNotIn(prohibited, call.argv)
            self.assertEqual(call.environment, {"PATH": os.defpath, "LC_ALL": "C"})
        self.assertFalse(
            any(
                "public-smoke.mjs" in part or part == "docker"
                for call in self.calls
                for part in call.argv
            )
        )

    def test_redirect_status_is_not_accepted_as_readiness(self) -> None:
        """TLS success with a redirect cannot mark migration complete."""
        self.http_status = "302"
        report = self.execute()
        self.assertFalse(report["passed"])
        self.assertEqual(report["failureClass"], "migration_https_response")

    def test_optional_services_follow_explicit_destination_inventory(self) -> None:
        """Disabled TURN, backup and monitoring do not become implicit new services."""
        target = obj(self.inventories[self.args.inventory], "_meta", "hostvars", "public")
        for key in ("scpub_turn_enabled", "scpub_backup_enabled", "scmon_enabled"):
            target[key] = False
        self.assertTrue(self.execute()["passed"])
        self.assertFalse({"turn.yml", "backup.yml", "monitoring.yml"} & set(self.actions))
        self.assertEqual(self.actions[-1], "finalize-source")

    def test_cli_reuses_strict_origin_and_inventory_validation(self) -> None:
        """Explicit selectors normalize canonical origins and reject same origins and patterns."""
        values = [
            "--source-inventory",
            self.args.source_inventory,
            "--source-origin",
            SOURCE_ORIGIN + "/",
            "--inventory",
            self.args.inventory,
            "--origin",
            TARGET_ORIGIN + ":443/",
            "--repository",
            "owner/repo",
        ]
        selected = migration.options(values)
        self.assertEqual(selected.source_origin, SOURCE_ORIGIN)
        self.assertEqual(selected.origin, TARGET_ORIGIN)
        for extra in (
            ["--origin", SOURCE_ORIGIN],
            ["--source-limit", "*"],
            ["--origin", "https://user:password@next.chat.example.test"],
            ["--origin", "https://bad..example.test"],
        ):
            with (
                self.subTest(extra=extra),
                self.assertRaises((migration.MigrationError, release_deploy.DeployError)),
            ):
                _ = migration.options([*values, *extra])

    def test_dirty_wrong_repository_or_nonmain_checkout_cannot_contact_hosts(self) -> None:
        """The shared checkout gate plus main selection precedes migration work."""
        cases = (
            (True, self.repository_url, "main"),
            (False, "https://github.com/other/repo.git", "main"),
            (False, self.repository_url, "feature"),
        )
        for dirty, repository_url, branch in cases:
            self.dirty, self.repository_url, self.branch = dirty, repository_url, branch
            with self.subTest(dirty=dirty, repository=repository_url, branch=branch):
                self.assertFalse(self.execute()["passed"])
                self.assertEqual(self.actions, [])

    def test_selector_changes_require_explicit_flags(self) -> None:
        """An ordinary migration does not modify the user's inventory or GitHub variable."""
        original = Path(self.args.source_inventory).read_bytes()
        report = self.execute()
        self.assertTrue(report["passed"])
        self.assertFalse(report["inventoryActivated"])
        self.assertFalse(report["canaryUpdated"])
        self.assertEqual(self.canary_reads, 0)
        self.assertEqual(Path(self.args.source_inventory).read_bytes(), original)

    def test_wrong_old_canary_origin_stops_before_any_host_call(self) -> None:
        """The existing monitor target must match the explicitly selected source."""
        self.args.update_canary = True
        self.canary_value = "https://another.example.test"
        report = self.execute()
        self.assertFalse(report["passed"])
        self.assertEqual(report["failureClass"], "migration_canary_origin_mismatch")
        self.assertEqual(self.actions, [])

    def test_explicit_selector_switch_preserves_alias_opt_ins_and_original_bytes(self) -> None:
        """After finalization the canonical inventory and existing canary select the new site."""
        path = Path(self.args.source_inventory)
        path.chmod(0o600)
        original = path.read_bytes()
        self.args.activate_inventory = str(path)
        self.args.update_canary = True
        report = self.execute()
        self.assertTrue(report["passed"])
        self.assertTrue(report["inventoryActivated"])
        self.assertTrue(report["canaryUpdated"])
        selected = obj(decode_json(path.read_bytes()), "benchmark_hosts", "hosts")
        self.assertEqual(set(selected), {"public"})
        host = obj(selected, "public")
        self.assertEqual(host["ansible_host"], "192.0.2.11")
        self.assertEqual(host["scpub_webauthn_rp_id"], "chat.example.test")
        self.assertEqual(host["scbench_revision"], REVISION)
        self.assertTrue(host["scpub_release_deploy"])
        self.assertTrue(host["scpub_turn_enabled"])
        self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
        evidence = Path(string_value(report["evidence"]))
        self.assertEqual((evidence / "original-inventory").read_bytes(), original)
        update = next(call for call in self.calls if call.argv[:3] == ["gh", "variable", "set"])
        self.assertEqual(
            update.argv,
            [
                "gh",
                "variable",
                "set",
                "CANARY_ORIGIN",
                "--repo",
                "owner/repo",
                "--body",
                TARGET_ORIGIN,
            ],
        )
        finalize = next(
            call
            for call in self.calls
            if call.argv[0] == "/ansible-playbook" and "finalize-source" in call.argv[-1]
        )
        self.assertGreater(self.calls.index(update), self.calls.index(finalize))
        self.assertEqual(self.canary_value, TARGET_ORIGIN)

    def test_changed_active_inventory_is_not_overwritten_after_cutover(self) -> None:
        """A concurrent operator edit survives; the new destination remains the only writer."""
        path = Path(self.args.source_inventory)
        path.chmod(0o600)
        self.args.activate_inventory = str(path)
        self.args.update_canary = True
        self.change_active_inventory = True
        report = self.execute()
        self.assertFalse(report["passed"])
        self.assertEqual(report["failureClass"], "migration_activation_inventory_changed")
        self.assertTrue(report["destinationStarted"])
        self.assertFalse(report["inventoryActivated"])
        self.assertFalse(report["canaryUpdateAttempted"])
        self.assertEqual(path.read_text(), "independent operator update\n")
        self.assertNotIn("resume-source", self.actions)

    def test_canary_failure_records_partial_selector_switch_without_source_restart(self) -> None:
        """An already activated inventory stays explicit when the subsequent API update fails."""
        path = Path(self.args.source_inventory)
        path.chmod(0o600)
        self.args.activate_inventory = str(path)
        self.args.update_canary = True
        self.fail_canary_update = True
        report = self.execute()
        self.assertFalse(report["passed"])
        self.assertTrue(report["inventoryActivated"])
        self.assertTrue(report["canaryUpdateAttempted"])
        self.assertFalse(report["canaryUpdated"])
        self.assertEqual(report["failedPhase"], "update_canary")
        self.assertNotIn("resume-source", self.actions)

    def test_activation_rejects_wrong_nonprivate_or_tracked_inventory(self) -> None:
        """Only the selected ordinary private ignored source inventory may be replaced."""
        path = Path(self.args.source_inventory)
        for selected, mode, ignored in (
            (self.args.inventory, 0o600, True),
            (str(path), 0o644, True),
            (str(path), 0o600, False),
        ):
            self.args.activate_inventory = selected
            path.chmod(mode)
            self.activation_ignored = ignored
            with self.subTest(path=selected, mode=mode, ignored=ignored):
                self.assertFalse(self.execute()["passed"])
                self.assertEqual(self.actions, [])

    def test_missing_stale_or_ambiguous_frontend_revision_fails_serving_check(self) -> None:
        """Healthy APIs cannot hide a stale, development or unrecognized frontend build."""
        for script in (
            "const telemetry={};",
            f'new T("{"b" * 40}")',
            'new T("development")',
            self.script + self.script,
        ):
            self.script = script
            with self.subTest(script=script):
                report = self.execute()
                self.assertFalse(report["passed"])
                self.assertEqual(report["failureClass"], "migration_frontend_revision")
                self.assertNotIn("resume-source", self.actions)

    def test_foreign_or_ambiguous_script_paths_are_never_requested(self) -> None:
        """The homepage can select only one known same-origin Vite asset path."""
        for markup in (
            '<script type="module" src="https://foreign.example/index-test.js"></script>',
            '<script type="module" src="//foreign.example/index-test.js"></script>',
            '<script type="module" src="/assets/../index-test.js"></script>',
            '<script type="module" src="/assets/index-test.js"></script>' * 2,
        ):
            self.homepage = "<!doctype html>" + markup
            with self.subTest(markup=markup):
                report = self.execute()
                self.assertFalse(report["passed"])
                self.assertEqual(report["failureClass"], "migration_frontend_asset_path")
        self.assertFalse(any("foreign.example" in call.argv[-1] for call in self.calls))

    def test_all_https_requests_share_one_overall_deadline(self) -> None:
        """A slow certificate request cannot grant each subsequent GET another 90 seconds."""
        output = self.root / "https-deadline"
        output.mkdir(mode=0o700)
        runner = FakeRunner(self, output)
        with (
            patch.object(time, "monotonic", side_effect=[0, 1, 91]),
            self.assertRaisesRegex(migration.MigrationError, "migration_https_timeout"),
        ):
            migration.verify_https(runner, self.root, "/curl", TARGET_ORIGIN, REVISION)
        self.assertEqual(len(self.calls), 1)
        self.assertEqual(self.calls[0].timeout, 89)
