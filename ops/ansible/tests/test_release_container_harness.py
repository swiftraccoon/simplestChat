"""Offline refusal, evidence and owned-cleanup checks; never contact Docker."""

from copy import deepcopy
from contextlib import ExitStack, redirect_stderr, redirect_stdout
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch


PROJECT = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location("release_container_harness_tests", PROJECT / "build/test-release-container.py")
HARNESS = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = HARNESS
sys.path.insert(0, str(PROJECT / "build"))
try:
    SPEC.loader.exec_module(HARNESS)
finally:
    sys.path.pop(0)

TOKEN = "c" * 32
CONTAINER = "a" * 64
NETWORK = "b" * 64
IMAGE = "sha256:" + "d" * 64
FAILED_IMAGE = "sha256:" + "e" * 64
REVISION = "f" * 40


class TextResult:
    def __init__(self, text="", code=0):
        self.value, self.code = text, code

    def text(self):
        return self.value


def owned(identity=CONTAINER, service="simplestchat", *, running=True, image=IMAGE, exit_code=0):
    return {
        "id": identity, "image": image, "restarts": 0,
        "state": {"Running": running, "ExitCode": exit_code, "OOMKilled": False,
                  "Error": "", "StartedAt": "2026-09-13T00:00:00Z", "Pid": 42 if running else 0},
        "labels": {HARNESS.LABEL: TOKEN, "com.docker.compose.project": "simplestchat-public",
                   "com.docker.compose.service": service},
    }


class CleanupCommands:
    """Model only the exact local inventory and single-ID cleanup operations."""

    def __init__(self, containers=(), networks=(), *, remaining="", update_failure=False):
        self.containers = {value["id"]: deepcopy(value) for value in containers}
        self.networks = {value["Id"]: deepcopy(value) for value in networks}
        self.remaining, self.update_failure = remaining, update_failure
        self.calls = []
        self.settlement_unconfirmed = False
        self.last_failure = None

    def docker(self, *args, **kwargs):
        self.calls.append((args, kwargs))
        if args[0] == "ps":
            allowed = {"--all", "--quiet", "--no-trunc", "--filter", f"label={HARNESS.LABEL}={TOKEN}"}
            if not set(args[1:]).issubset(allowed):
                raise AssertionError(f"Unexpected container inventory: {args}")
            return TextResult("\n".join(self.containers) if "--filter" in args else self.remaining)
        if args[:2] == ("network", "ls"):
            allowed = {"--quiet", "--no-trunc", "--filter", f"label={HARNESS.LABEL}={TOKEN}"}
            if not set(args[2:]).issubset(allowed):
                raise AssertionError(f"Unexpected network inventory: {args}")
            return TextResult("\n".join(self.networks))
        if args[0] == "inspect":
            return TextResult(json.dumps(self.containers[args[-1]]))
        if args[:2] == ("network", "inspect"):
            return TextResult(json.dumps(self.networks[args[-1]]))
        if args[0] == "logs":
            if args != ("logs", "--tail", "150", args[-1]) or args[-1] not in self.containers:
                raise AssertionError(f"Unexpected log capture: {args}")
            return TextResult()
        if args[0] == "stop":
            if args != ("stop", "--time", "30", args[-1]) or kwargs != {"timeout": 45}:
                raise AssertionError(f"Unexpected stop: {args}, {kwargs}")
            self.containers[args[-1]]["state"].update({"Running": False, "Pid": 0})
            return TextResult()
        if args[0] == "rm" and len(args) == 2:
            del self.containers[args[-1]]
            return TextResult()
        if args[:2] == ("network", "rm") and len(args) == 3:
            del self.networks[args[-1]]
            return TextResult()
        raise AssertionError(f"Unexpected Docker mutation: {args}")

    def run(self, args, **kwargs):
        self.calls.append((tuple(args), kwargs))
        if args != ["/usr/sbin/update-ca-certificates"] or kwargs != {"timeout": 30}:
            raise AssertionError(f"Unexpected host command: {args}, {kwargs}")
        if self.update_failure:
            raise HARNESS.CheckError("Owned fixture trust-store refresh failed")
        return TextResult()


class HarnessTestCase(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="simplestchat-release-harness.")
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name).resolve()
        # An unexpected implementation change must not contact a daemon or
        # execute a host command from this offline suite.
        process = patch.object(HARNESS.subprocess, "Popen", side_effect=AssertionError("No child commands in offline tests"))
        process.start()
        self.addCleanup(process.stop)

    def harness(self):
        output = self.directory / f"output-{len(list(self.directory.iterdir()))}"
        output.mkdir(mode=0o700)
        (output / "private").mkdir(mode=0o700)
        harness = HARNESS.Harness(output, "simplestchat-ci:production")
        harness.token = TOKEN
        harness.fixture_images["failed"] = {"image": FAILED_IMAGE}
        return harness


class HarnessInputTests(HarnessTestCase):
    def test_socket_directory_remains_traversable_under_private_process_umask(self):
        initial_umask = os.umask(0o077)
        try:
            with patch.object(HARNESS.os, "chown") as chown:
                for name, uid, mode in (("postgres", 999, 0o700), ("postgres-socket", 999, 0o755),
                                        ("caddy-data", 10001, 0o700), ("caddy-config", 10001, 0o700)):
                    path = self.directory / name
                    HARNESS.create_data_directory(path, uid, mode)
                    self.assertEqual(stat.S_IMODE(path.stat().st_mode), mode)
                    chown.assert_called_with(path, uid, uid)
                with self.assertRaises(FileExistsError):
                    HARNESS.create_data_directory(self.directory / "postgres-socket", 999, 0o755)
        finally:
            os.umask(initial_umask)

    def test_cli_requires_explicit_disposable_opt_in_and_all_inputs(self):
        valid = ["--disposable-host", "--image", "simplestchat-ci:production", "--output", "/tmp/fresh-output"]
        for arguments in (valid[1:], valid[:1] + valid[3:], valid[:3]):
            with self.subTest(arguments=arguments), redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
                HARNESS.options(arguments)
            self.assertEqual(error.exception.code, 2)
        arguments = HARNESS.options(valid)
        self.assertIs(arguments.disposable_host, True)
        self.assertEqual(arguments.image, "simplestchat-ci:production")

    def test_cli_refuses_image_options_shell_text_whitespace_and_oversized_selectors(self):
        for image in ("", "--privileged", "image;true", "image\nother", "image with space", "$(id)", "x" * 257):
            with self.subTest(image=image), redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                HARNESS.options(["--disposable-host", "--image=" + image, "--output", "/tmp/fresh-output"])
        self.assertEqual(list(self.directory.iterdir()), [])

    def test_output_is_new_and_private_before_any_host_side_effect(self):
        output = self.directory / "report"
        self.assertEqual(HARNESS.fresh_output(str(output)), output)
        self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE((output / "private").stat().st_mode), 0o700)
        self.assertEqual(list(output.iterdir()), [output / "private"])
        with self.assertRaises(HARNESS.CheckError):
            HARNESS.fresh_output(str(output))

    def test_existing_file_symlink_parent_and_dangling_output_are_preserved(self):
        existing = self.directory / "existing"
        existing.write_text("retained evidence")
        linked = self.directory / "link"
        linked.symlink_to(self.directory, target_is_directory=True)
        dangling = self.directory / "dangling"
        dangling.symlink_to(self.directory / "missing")
        for output in (existing, linked / "new", dangling):
            with self.subTest(output=output), self.assertRaises(HARNESS.CheckError):
                HARNESS.fresh_output(str(output))
        self.assertEqual(existing.read_text(), "retained evidence")
        self.assertTrue(dangling.is_symlink())
        self.assertFalse((self.directory / "new").exists())

    def test_output_rejects_relative_missing_parent_controls_and_fixture_storage(self):
        fixed = self.directory / "public"
        with patch.object(HARNESS, "FIXED_PATHS", (fixed,)):
            for output in ("relative-output", str(self.directory / "missing" / "child"),
                           str(self.directory / "bad\nname"), str(fixed)):
                with self.subTest(output=output), self.assertRaises(HARNESS.CheckError):
                    HARNESS.fresh_output(output)
            fixed.mkdir()
            with self.assertRaises(HARNESS.CheckError):
                HARNESS.fresh_output(str(fixed / "child"))
        self.assertEqual(list(fixed.iterdir()), [])


class HarnessPreflightTests(HarnessTestCase):
    def test_wrong_platform_architecture_or_uid_refuses_before_tool_or_socket_checks(self):
        for platform, machine, uid in (("darwin", "arm64", 0), ("linux", "aarch64", 0), ("linux", "x86_64", 501)):
            with self.subTest(platform=platform, machine=machine, uid=uid), ExitStack() as stack:
                stack.enter_context(patch.object(HARNESS.sys, "platform", platform))
                stack.enter_context(patch.object(HARNESS.platform, "machine", return_value=machine))
                stack.enter_context(patch.object(HARNESS.os, "geteuid", return_value=uid))
                access = stack.enter_context(patch.object(HARNESS.os, "access"))
                sockets = stack.enter_context(patch.object(HARNESS.socket, "socket"))
                with self.assertRaisesRegex(HARNESS.CheckError, "fresh disposable Linux"):
                    HARNESS.host_preflight()
                access.assert_not_called()
                sockets.assert_not_called()

    def test_existing_public_or_benchmark_path_refuses_without_contacting_services(self):
        for dangling in (False, True):
            existing = self.directory / ("dangling" if dangling else "public")
            if dangling:
                existing.symlink_to(self.directory / "missing")
            else:
                existing.mkdir()
            with self.subTest(dangling=dangling), ExitStack() as stack:
                stack.enter_context(patch.object(HARNESS.sys, "platform", "linux"))
                stack.enter_context(patch.object(HARNESS.platform, "machine", return_value="x86_64"))
                stack.enter_context(patch.object(HARNESS.os, "geteuid", return_value=0))
                stack.enter_context(patch.object(HARNESS, "FIXED_PATHS", (existing,)))
                access = stack.enter_context(patch.object(HARNESS.os, "access"))
                sockets = stack.enter_context(patch.object(HARNESS.socket, "socket"))
                with self.assertRaisesRegex(HARNESS.CheckError, "Existing service or benchmark path"):
                    HARNESS.host_preflight()
                access.assert_not_called()
                sockets.assert_not_called()
            self.assertTrue(existing.is_symlink() if dangling else existing.is_dir())

    def test_missing_tooling_fails_before_port_checks(self):
        with patch.object(HARNESS.sys, "platform", "linux"), \
                patch.object(HARNESS.platform, "machine", return_value="x86_64"), \
                patch.object(HARNESS.os, "geteuid", return_value=0), \
                patch.object(HARNESS, "FIXED_PATHS", ()), \
                patch.object(HARNESS.os, "access", return_value=False), \
                patch.object(HARNESS.socket, "socket") as sockets:
            with self.assertRaisesRegex(HARNESS.CheckError, "tooling is missing"):
                HARNESS.host_preflight()
            sockets.assert_not_called()


class HarnessOwnershipTests(HarnessTestCase):
    def test_unconfirmed_command_settlement_forbids_all_cleanup_mutations(self):
        harness = self.harness()
        harness.created_paths = True
        harness.commands = CleanupCommands([owned()])
        harness.commands.settlement_unconfirmed = True
        ca = self.directory / "retained-trust.crt"
        ca.write_text("owned fixture certificate")
        harness.ca_path, harness.ca_digest = ca, HARNESS.RELEASE.sha256_file(ca)
        harness.cleanup()
        self.assertIs(harness.report["cleanupPassed"], False)
        self.assertEqual(harness.report["cleanupFailures"], ["CommandSettlementUnconfirmed"])
        self.assertEqual(harness.commands.calls, [])
        self.assertEqual(ca.read_text(), "owned fixture certificate")

    def test_incomplete_ca_publication_is_reported_and_never_deleted(self):
        harness = self.harness()
        harness.commands = CleanupCommands()
        ca = self.directory / "partial-trust.crt"
        ca.write_text("partial certificate")
        harness.ca_path = ca
        self.assertIsNone(harness.ca_digest)
        harness.cleanup()
        self.assertIs(harness.report["cleanupPassed"], False)
        self.assertEqual(harness.report["cleanupFailures"], ["FixtureCaPublicationIncomplete"])
        self.assertEqual(harness.commands.calls, [])
        self.assertEqual(ca.read_text(), "partial certificate")

    def test_container_identity_must_be_full_length_before_any_inspection(self):
        commands = Mock()
        for identity in ("a" * 12, "sha256:" + CONTAINER, CONTAINER + "\n", "--all", ""):
            with self.subTest(identity=identity), self.assertRaises(HARNESS.CheckError):
                HARNESS.inspect_owned(commands, identity, TOKEN)
        commands.docker.assert_not_called()

    def test_exact_identity_and_all_ownership_labels_are_required(self):
        for field, replacement in (("id", "b" * 64), (HARNESS.LABEL, "other-fixture"),
                                   ("com.docker.compose.project", "unrelated"),
                                   ("com.docker.compose.service", "other")):
            value = owned()
            if field == "id":
                value[field] = replacement
            else:
                value["labels"][field] = replacement
            commands = Mock()
            commands.docker.return_value = TextResult(json.dumps(value))
            with self.subTest(field=field), self.assertRaises(HARNESS.CheckError):
                HARNESS.inspect_owned(commands, CONTAINER, TOKEN)
        commands = Mock()
        commands.docker.return_value = TextResult(json.dumps(owned()))
        self.assertEqual(HARNESS.inspect_owned(commands, CONTAINER, TOKEN), owned())

    def test_healthy_exit_distinguishes_the_deliberate_failure_from_real_damage(self):
        HARNESS.assert_healthy_exit(owned(running=False))
        HARNESS.assert_healthy_exit(owned(running=False, exit_code=42), deliberate_failure=True)
        for field, replacement in (("Running", True), ("OOMKilled", True), ("Error", "daemon failure"), ("ExitCode", 137)):
            value = owned(running=False)
            value["state"][field] = replacement
            with self.subTest(field=field), self.assertRaises(HARNESS.CheckError):
                HARNESS.assert_healthy_exit(value)
        with self.assertRaises(HARNESS.CheckError):
            HARNESS.assert_healthy_exit(owned(running=False, exit_code=42))

    def test_cleanup_verifies_every_container_before_stopping_or_removing_any(self):
        harness = self.harness()
        harness.created_paths = True
        wrong = owned("e" * 64, "postgres")
        wrong["labels"][HARNESS.LABEL] = "another-run"
        harness.commands = CleanupCommands([owned(), wrong])
        harness.cleanup()
        self.assertIs(harness.report["cleanupPassed"], False)
        self.assertEqual(harness.report["cleanupFailures"], ["CheckError"])
        self.assertFalse(any(args[0] in ("stop", "rm", "logs") for args, _ in harness.commands.calls))

    def test_cleanup_uses_only_verified_ids_in_service_shutdown_order(self):
        harness = self.harness()
        harness.created_paths = True
        containers = [owned("4" * 64, "postgres"), owned("3" * 64, "caddy"),
                      owned("2" * 64, "migrate"), owned("1" * 64, "simplestchat")]
        network = {"Id": NETWORK, "Labels": {HARNESS.LABEL: TOKEN,
                   "com.docker.compose.project": "simplestchat-public"}, "Containers": {}}
        harness.commands = CleanupCommands(containers, [network])
        harness.cleanup()
        self.assertIs(harness.report["cleanupPassed"], True)
        self.assertEqual([args[-1] for args, _ in harness.commands.calls if args[0] == "stop"],
                         [character * 64 for character in "1234"])
        self.assertEqual([args for args, _ in harness.commands.calls if args[0] == "rm"],
                         [("rm", character * 64) for character in "1234"])
        self.assertIn((("network", "rm", NETWORK), {}), harness.commands.calls)
        inventories = [args for args, _ in harness.commands.calls if "--filter" in args]
        self.assertEqual(len(inventories), 2)
        self.assertTrue(all("--no-trunc" in args for args in inventories))
        self.assertNotIn("prune", str(harness.commands.calls))
        self.assertNotIn("--force", str(harness.commands.calls))

    def test_unexpected_exit_preserves_container_and_marks_cleanup_failed(self):
        harness = self.harness()
        harness.created_paths = True
        harness.commands = CleanupCommands([owned(running=False, exit_code=137)])
        harness.cleanup()
        self.assertIs(harness.report["cleanupPassed"], False)
        self.assertIn(CONTAINER, harness.commands.containers)
        self.assertFalse(any(args[0] == "rm" for args, _ in harness.commands.calls))

    def test_deliberate_failed_image_can_be_removed_only_with_expected_exit(self):
        harness = self.harness()
        harness.created_paths = True
        harness.commands = CleanupCommands([owned(running=False, image=FAILED_IMAGE, exit_code=42)])
        harness.cleanup()
        self.assertIs(harness.report["cleanupPassed"], True)
        self.assertNotIn(CONTAINER, harness.commands.containers)

    def test_network_with_other_ownership_or_attached_containers_is_retained(self):
        for wrong_owner in (False, True):
            harness = self.harness()
            harness.created_paths = True
            network = {"Id": NETWORK, "Labels": {HARNESS.LABEL: "other" if wrong_owner else TOKEN,
                       "com.docker.compose.project": "simplestchat-public"},
                       "Containers": {} if wrong_owner else {CONTAINER: {"Name": "still-running"}}}
            harness.commands = CleanupCommands([], [network])
            harness.cleanup()
            self.assertIs(harness.report["cleanupPassed"], False)
            self.assertIn(NETWORK, harness.commands.networks)
            self.assertNotIn((("network", "rm", NETWORK), {}), harness.commands.calls)

    def test_unowned_container_remaining_is_reported_without_deleting_it(self):
        harness = self.harness()
        harness.created_paths = True
        harness.commands = CleanupCommands(remaining="9" * 64)
        harness.cleanup()
        self.assertIs(harness.report["cleanupPassed"], False)
        self.assertFalse(any(args[0] in ("stop", "rm") for args, _ in harness.commands.calls))

    def test_no_created_fixture_means_no_container_or_host_commands(self):
        harness = self.harness()
        harness.commands = CleanupCommands()
        harness.cleanup()
        self.assertIs(harness.report["cleanupPassed"], True)
        self.assertEqual(harness.commands.calls, [])

    def test_ca_cleanup_requires_unchanged_regular_file_and_refreshes_trust(self):
        harness = self.harness()
        harness.commands = CleanupCommands()
        ca = self.directory / "owned-ca.crt"
        ca.write_text("owned fixture certificate")
        harness.ca_path, harness.ca_digest = ca, HARNESS.RELEASE.sha256_file(ca)
        harness.cleanup()
        self.assertIs(harness.report["cleanupPassed"], True)
        self.assertFalse(ca.exists())
        self.assertEqual(harness.commands.calls, [(("/usr/sbin/update-ca-certificates",), {"timeout": 30})])

    def test_changed_or_symlinked_ca_is_never_removed(self):
        for symlink in (False, True):
            harness = self.harness()
            harness.commands = CleanupCommands()
            target = self.directory / f"certificate-{symlink}"
            target.write_text("owned fixture certificate")
            harness.ca_digest = HARNESS.RELEASE.sha256_file(target)
            ca = self.directory / f"trust-{symlink}"
            if symlink:
                ca.symlink_to(target)
            else:
                ca.write_text("different certificate")
            harness.ca_path = ca
            harness.cleanup()
            self.assertIs(harness.report["cleanupPassed"], False)
            self.assertTrue(ca.is_symlink() if symlink else ca.exists())
            self.assertEqual(harness.commands.calls, [])

    def test_trust_refresh_failure_preserves_failed_cleanup_outcome(self):
        harness = self.harness()
        harness.commands = CleanupCommands(update_failure=True)
        ca = self.directory / "certificate"
        ca.write_text("owned fixture certificate")
        harness.ca_path, harness.ca_digest = ca, HARNESS.RELEASE.sha256_file(ca)
        harness.cleanup()
        self.assertIs(harness.report["cleanupPassed"], False)
        self.assertEqual(harness.report["cleanupFailures"], ["CheckError"])


class HarnessEvidenceTests(HarnessTestCase):
    def test_original_release_summary_includes_only_the_explicit_public_allowlist(self):
        harness = self.harness()
        root = self.directory / "private-service"
        (root / "results").mkdir(parents=True)
        summary = {"action": "deploy", "revision": REVISION, "phase": "startup",
                   "passed": False, "failure": "Candidate readiness failed",
                   "rollbackAttempted": True, "rollbackPassed": True}
        private_value = "PRIVATE_DATABASE_PASSWORD_AND_BACKUP_CONTENT"
        outcome = dict(summary, backupSha256="a" * 64, databaseDump=private_value,
                       databasePassword=private_value, environment={"JWT_SECRET": private_value},
                       arbitraryExtra=private_value)

        def invoke(_args, **_kwargs):
            attempt = root / "results" / "release.owned"
            attempt.mkdir()
            (attempt / "outcome.json").write_text(json.dumps(outcome))
            return TextResult(code=1)

        harness.commands = Mock()
        harness.commands.run.side_effect = invoke
        with patch.object(HARNESS, "ROOT", root):
            attempt, actual = harness.invoke_release("deploy", {"revision": REVISION}, failure=True)
        self.assertEqual(actual, outcome, "Private original evidence must remain complete")
        self.assertEqual(json.loads((attempt / "outcome.json").read_text()), outcome)
        self.assertEqual(harness.report["lastRelease"], summary)
        self.assertNotIn(private_value, json.dumps(harness.report))
        self.assertNotIn("backupSha256", harness.report["lastRelease"])

    def test_release_status_requires_matching_original_boolean_and_exit_code(self):
        cases = [(False, 0, True, True), (True, 1, False, True), (True, 0, False, False),
                 (True, 1, True, False), (False, 1, True, False), (False, 0, False, False),
                 (True, 1, 0, False), (False, 0, 1, False)]
        for index, (failure, code, passed, accepted) in enumerate(cases):
            with self.subTest(failure=failure, code=code, passed=passed, accepted=accepted):
                harness = self.harness()
                root = self.directory / f"service-{index}"
                (root / "results").mkdir(parents=True)
                outcome = {"action": "deploy", "revision": REVISION, "passed": passed}

                def invoke(args, **kwargs):
                    self.assertEqual(args[-3:], [str(HARNESS.FILES / "release-public.py"), "deploy", REVISION])
                    self.assertEqual(kwargs, {"timeout": 180, "success": False})
                    attempt = root / "results" / "release.owned"
                    attempt.mkdir()
                    (attempt / "outcome.json").write_text(json.dumps(outcome))
                    return TextResult(code=code)

                harness.commands = Mock()
                harness.commands.run.side_effect = invoke
                with patch.object(HARNESS, "ROOT", root):
                    if accepted:
                        _, actual = harness.invoke_release("deploy", {"revision": REVISION}, failure=failure)
                        self.assertEqual(actual, outcome)
                    else:
                        with self.assertRaisesRegex(HARNESS.CheckError, "exit status and original outcome disagree"):
                            harness.invoke_release("deploy", {"revision": REVISION}, failure=failure)

    def test_failure_and_cleanup_failure_cannot_be_converted_into_a_pass(self):
        for execution_passes, cleanup_passes in ((False, True), (True, False), (False, False), (True, True)):
            with self.subTest(execution=execution_passes, cleanup=cleanup_passes):
                output = self.directory / f"main-{execution_passes}-{cleanup_passes}"
                harness = Mock()
                harness.report = {"schemaVersion": 1, "passed": False, "cleanupPassed": False, "phase": "preflight"}
                retained_command = None if execution_passes else {
                    "number": 7, "operation": "release-public.py deploy", "exitStatus": 1,
                }
                harness.commands.last_failure = retained_command
                if not execution_passes:
                    harness.exercise.side_effect = HARNESS.CheckError("Original execution failure")

                def cleanup():
                    harness.report["cleanupPassed"] = cleanup_passes

                harness.cleanup.side_effect = cleanup
                with patch.object(HARNESS, "Harness", return_value=harness), \
                        patch.object(HARNESS.os, "umask"), \
                        patch.object(HARNESS.signal, "signal"), \
                        patch.object(HARNESS.signal, "alarm") as alarm, redirect_stdout(io.StringIO()):
                    status = HARNESS.main(["--disposable-host", "--image", "simplestchat-ci:production", "--output", str(output)])
                self.assertEqual(status, 0 if execution_passes and cleanup_passes else 1)
                report = json.loads((output / "report.json").read_text())
                self.assertIs(report["passed"], execution_passes and cleanup_passes)
                self.assertEqual(report["lastNonzeroCommand"], retained_command)
                if not execution_passes:
                    self.assertEqual(report["failure"], "Original execution failure")
                harness.cleanup.assert_called_once()
                self.assertEqual([call.args for call in alarm.call_args_list], [(480,), (100,), (0,)])
                self.assertEqual(stat.S_IMODE((output / "report.json").stat().st_mode), 0o644)
                self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o755)
                self.assertEqual(stat.S_IMODE((output / "private").stat().st_mode), 0o700)


class HarnessValidatorDiagnosticTests(HarnessTestCase):
    def fixture(self):
        harness = self.harness()
        harness.commands = Mock()
        harness.commands.last_failure = {"number": 56, "operation": "release-public.py stage", "exitStatus": 1}
        attempt = harness.private / "release-attempt"
        attempt.mkdir(mode=0o700)
        name = "scpub-release-validate-" + TOKEN
        (attempt / "validation-name.txt").write_text(name)
        value = {"id": CONTAINER, "name": "/" + name, "image": IMAGE, "user": "10001:10001",
                 "entrypoint": ["/usr/bin/timeout"], "network": "none", "readOnly": True,
                 "state": {"ExitCode": 128, "Status": "created", "OOMKilled": False,
                           "Error": "failed to create task: exec /usr/bin/timeout: no such file or directory"}}
        harness.commands.docker.return_value = TextResult(json.dumps(value))
        return harness, attempt, value

    def test_verified_validator_startup_error_is_projected_without_general_logs_or_environment(self):
        harness, attempt, value = self.fixture()
        previous_failure = deepcopy(harness.commands.last_failure)
        harness.validator_startup_diagnostic(attempt, {"image": IMAGE})
        self.assertEqual(harness.report["validatorStartup"], {
            "diagnosticUnavailable": False, "exitCode": 128, "status": "created", "oomKilled": False,
            "error": value["state"]["Error"], "errorTruncated": False,
        })
        self.assertIs(harness.report["passed"], False)
        self.assertEqual(harness.commands.last_failure, previous_failure)
        harness.commands.docker.assert_called_once()
        call = harness.commands.docker.call_args
        self.assertEqual(call.args[:2], ("inspect", "--format"))
        self.assertEqual(call.args[-1], "scpub-release-validate-" + TOKEN)
        self.assertEqual(call.kwargs, {"timeout": 10, "success": False})
        self.assertNotIn(".Config.Env", call.args[2])
        self.assertNotIn("logs", call.args)
        self.assertNotIn(IMAGE, json.dumps(harness.report["validatorStartup"]))

    def test_missing_symlink_nonregular_oversized_or_invalid_name_never_contacts_docker(self):
        for kind in ("missing", "symlink", "directory", "oversized", "invalid", "multiline"):
            with self.subTest(kind=kind):
                harness, attempt, _ = self.fixture()
                path = attempt / "validation-name.txt"
                path.unlink()
                if kind == "symlink":
                    target = attempt / "other"
                    target.write_text("scpub-release-validate-" + TOKEN)
                    path.symlink_to(target)
                elif kind == "directory":
                    path.mkdir()
                elif kind == "oversized":
                    path.write_text("x" * 129)
                elif kind == "invalid":
                    path.write_text("unrelated-container")
                elif kind == "multiline":
                    path.write_text("scpub-release-validate-" + TOKEN + "\nother")
                harness.validator_startup_diagnostic(attempt, {"image": IMAGE})
                self.assertEqual(harness.report["validatorStartup"], {
                    "diagnosticUnavailable": True, "reason": "ValidatorNameUnavailable",
                })
                harness.commands.docker.assert_not_called()

    def test_every_identity_constraint_is_required_before_startup_error_is_published(self):
        for key, replacement in (("id", "a" * 12), ("name", "/other"), ("image", FAILED_IMAGE),
                                 ("user", "0:0"), ("entrypoint", ["/bin/sh"]), ("network", "bridge"),
                                 ("readOnly", False)):
            with self.subTest(key=key):
                harness, attempt, value = self.fixture()
                value[key] = replacement
                value["state"]["Error"] = "PRIVATE_ERROR_FROM_UNVERIFIED_CONTAINER"
                harness.commands.docker.return_value = TextResult(json.dumps(value))
                harness.validator_startup_diagnostic(attempt, {"image": IMAGE})
                self.assertEqual(harness.report["validatorStartup"], {
                    "diagnosticUnavailable": True, "reason": "ValidatorOwnershipMismatch",
                })
                self.assertNotIn("PRIVATE_ERROR", json.dumps(harness.report))
                self.assertIs(harness.report["passed"], False)

    def test_state_projection_is_typed_and_error_length_is_bounded(self):
        harness, attempt, value = self.fixture()
        value["state"]["Error"] = "x" * 4096
        value["state"]["Other"] = "PRIVATE_UNSELECTED_STATE"
        value["environment"] = ["SECRET=PRIVATE_UNSELECTED_ENVIRONMENT"]
        harness.commands.docker.return_value = TextResult(json.dumps(value))
        harness.validator_startup_diagnostic(attempt, {"image": IMAGE})
        diagnostic = harness.report["validatorStartup"]
        self.assertEqual(diagnostic["error"], "x" * 2048)
        self.assertIs(diagnostic["errorTruncated"], True)
        self.assertNotIn("PRIVATE_UNSELECTED", json.dumps(diagnostic))
        for key, replacement in (("ExitCode", True), ("ExitCode", -1), ("OOMKilled", 0),
                                 ("Status", "PRIVATE_UNEXPECTED_STATUS"), ("Error", {"private": "data"})):
            with self.subTest(key=key, replacement=replacement):
                harness, attempt, value = self.fixture()
                value["state"][key] = replacement
                harness.commands.docker.return_value = TextResult(json.dumps(value))
                harness.validator_startup_diagnostic(attempt, {"image": IMAGE})
                self.assertEqual(harness.report["validatorStartup"], {
                    "diagnosticUnavailable": True, "reason": "ValidatorStateUnavailable",
                })

    def test_failed_or_malformed_inspection_is_unavailable_without_masking_original_command(self):
        for result in (TextResult("PRIVATE_DAEMON_ERROR", code=1), TextResult("malformed-json")):
            with self.subTest(code=result.code):
                harness, attempt, _ = self.fixture()
                previous_failure = deepcopy(harness.commands.last_failure)

                def inspect(*_args, **_kwargs):
                    harness.commands.last_failure = {"number": 57, "operation": "docker inspect", "exitStatus": 1}
                    return result

                harness.commands.docker.side_effect = inspect
                harness.validator_startup_diagnostic(attempt, {"image": IMAGE})
                self.assertEqual(harness.report["validatorStartup"], {
                    "diagnosticUnavailable": True, "reason": "ValidatorInspectionUnavailable",
                })
                self.assertEqual(harness.commands.last_failure, previous_failure)
                self.assertNotIn("PRIVATE_DAEMON_ERROR", json.dumps(harness.report))

    def test_failed_release_retains_validator_evidence_before_the_existing_failure_assertion(self):
        harness, original_attempt, value = self.fixture()
        root = self.directory / "service"
        (root / "results").mkdir(parents=True)
        outcome = {"action": "stage", "revision": REVISION, "phase": "stage", "passed": False,
                   "failure": "Command 004 failed; inspect private output"}

        def invoke(_args, **_kwargs):
            attempt = root / "results" / "release.owned"
            attempt.mkdir()
            (attempt / "outcome.json").write_text(json.dumps(outcome))
            (attempt / "validation-name.txt").write_bytes((original_attempt / "validation-name.txt").read_bytes())
            return TextResult(code=1)

        harness.commands.run.side_effect = invoke
        with patch.object(HARNESS, "ROOT", root), \
                self.assertRaisesRegex(HARNESS.CheckError, "Release exit status and original outcome disagree"):
            harness.invoke_release("stage", {"revision": REVISION, "image": IMAGE})
        self.assertEqual(harness.report["lastRelease"], outcome)
        self.assertEqual(harness.report["validatorStartup"]["error"], value["state"]["Error"])
        self.assertIs(harness.report["passed"], False)
        self.assertEqual(json.loads((root / "results/release.owned/outcome.json").read_text()), outcome)


class HarnessCommandTests(HarnessTestCase):
    def test_nonzero_command_summary_excludes_private_arguments_and_environment(self):
        private_value = "PRIVATE_DATABASE_PASSWORD_AND_BACKUP_CONTENT"
        cases = [
            (["/usr/bin/curl", "--header", "Authorization: Bearer " + private_value,
              "https://localhost/ready"], "curl"),
            ([*HARNESS.RELEASE.DOCKER, "exec", "--interactive", CONTAINER, "psql",
              "--command", "SELECT '" + private_value + "'"], "docker exec"),
            ([*HARNESS.RELEASE.DOCKER, "rm", private_value], "docker rm"),
            ([*HARNESS.RELEASE.DOCKER, "pull", private_value], "docker pull"),
            ([*HARNESS.RELEASE.DOCKER, "image", "inspect", "--format", private_value,
              "simplestchat-ci:production"], "docker image inspect"),
            ([sys.executable, "-B", str(HARNESS.FILES / "release-public.py"), "deploy", REVISION],
             "release-public.py deploy"),
        ]
        for index, (arguments, operation) in enumerate(cases):
            with self.subTest(operation=operation):
                directory = self.directory / f"redacted-command-{index}"
                directory.mkdir(mode=0o700)
                commands = HARNESS.Commands(directory)
                child = Mock()
                child.returncode = 42
                child.poll.return_value = 42
                environment = dict(HARNESS.RELEASE.ENV, PRIVATE_FIXTURE_SECRET=private_value)
                with patch.object(HARNESS.subprocess, "Popen", return_value=child), \
                        patch.object(HARNESS.RELEASE, "ENV", environment):
                    result = commands.run(arguments, success=False)
                self.assertEqual(result.code, 42)
                self.assertEqual(commands.last_failure, {"number": 1, "operation": operation, "exitStatus": 42})
                self.assertNotIn(private_value, json.dumps(commands.last_failure))
                self.assertNotIn("PRIVATE_FIXTURE_SECRET", json.dumps(commands.last_failure))
                self.assertNotIn(REVISION, json.dumps(commands.last_failure))
                self.assertNotIn(CONTAINER, json.dumps(commands.last_failure))
                private_evidence = json.loads((directory / "command-001.json").read_text())
                self.assertEqual(private_evidence["command"], arguments)

    def test_cancelled_owned_process_marks_uncertainty_before_stopping_child(self):
        for interruption in (subprocess.TimeoutExpired(["owned-command"], 3),
                             HARNESS.CheckError("Owned command cancelled")):
            with self.subTest(interruption=type(interruption).__name__):
                directory = self.directory / type(interruption).__name__
                directory.mkdir(mode=0o700)
                commands = HARNESS.Commands(directory)
                child = Mock()
                child.wait.side_effect = interruption
                child.poll.return_value = None
                child.returncode = None

                def settle(process):
                    self.assertIs(process, child)
                    self.assertIs(commands.settlement_unconfirmed, True)
                    process.returncode = -15

                with patch.object(HARNESS.subprocess, "Popen", return_value=child) as launch, \
                        patch.object(HARNESS.RELEASE.Runner, "stop", side_effect=settle) as stop:
                    with self.assertRaises(type(interruption)):
                        commands.run(["owned-command"], timeout=3)
                stop.assert_called_once_with(child)
                self.assertIs(commands.settlement_unconfirmed, True)
                self.assertIs(launch.call_args.kwargs["start_new_session"], True)
                self.assertEqual(launch.call_args.kwargs["env"], HARNESS.RELEASE.ENV)
                report = json.loads((directory / "command-001.json").read_text())
                self.assertEqual(report["exitStatus"], -15)
                self.assertEqual(report["timeoutSeconds"], 3)
                self.assertTrue((directory / "command-001.stdout").exists())
                self.assertTrue((directory / "command-001.stderr").exists())

    def test_completed_command_failure_is_not_confused_with_unsettled_execution(self):
        commands = HARNESS.Commands(self.directory)
        child = Mock()
        child.returncode = 42
        child.poll.return_value = 42
        with patch.object(HARNESS.subprocess, "Popen", return_value=child), \
                patch.object(HARNESS.RELEASE.Runner, "stop") as stop:
            with self.assertRaisesRegex(HARNESS.CheckError, "Command 001 failed"):
                commands.run(["owned-command"])
        stop.assert_not_called()
        self.assertIs(commands.settlement_unconfirmed, False)
        self.assertEqual(json.loads((self.directory / "command-001.json").read_text())["exitStatus"], 42)


class HarnessDerivativeTests(HarnessTestCase):
    def fixture(self, *, before=IMAGE, after=IMAGE):
        harness = self.harness()
        harness.base_image = IMAGE
        harness.base_tag = f"simplestchat-release-test/base:{TOKEN}"
        harness.commands = Mock()
        replies = iter([before, after])

        def docker(*args, **kwargs):
            if args[:2] == ("image", "inspect") and args[-1] == harness.base_tag:
                return TextResult(next(replies))
            if args[:2] == ("buildx", "build"):
                return TextResult()
            if args[:2] == ("image", "inspect") and args[-1].startswith("simplestchat-release/production:"):
                return TextResult(FAILED_IMAGE)
            raise AssertionError(f"Unexpected fixture image operation: {args}, {kwargs}")

        harness.commands.docker.side_effect = docker
        return harness

    def test_derivative_uses_checked_local_tag_default_builder_and_no_production_rebuild(self):
        harness = self.fixture()
        candidate = harness.derivative("candidate")
        directory = harness.private / "candidate"
        dockerfile = (directory / "Dockerfile").read_text()
        self.assertTrue(dockerfile.startswith(f"FROM {harness.base_tag}\n"))
        self.assertNotIn("FROM sha256:", dockerfile)
        self.assertNotIn("RUN ", dockerfile)
        self.assertEqual(candidate["image"], FAILED_IMAGE)
        calls = harness.commands.docker.call_args_list
        self.assertEqual(calls[0].args, ("image", "inspect", "--format", "{{.Id}}", harness.base_tag))
        self.assertEqual(calls[1].args, (
            "buildx", "build", "--builder", "default", "--load", "--platform", "linux/amd64",
            "--provenance=false", "--sbom=false", "--network", "none", "--pull=false",
            "--tag", candidate["tag"], str(directory),
        ))
        self.assertEqual(calls[1].kwargs, {"timeout": 90})
        self.assertEqual(calls[2].args, calls[0].args)
        self.assertEqual(calls[3].args, ("image", "inspect", "--format", "{{.Id}}", candidate["tag"]))
        self.assertEqual(len(calls), 4)

    def test_changed_base_tag_is_refused_before_build_or_candidate_acceptance(self):
        for before, after, expected_builds, message in (
                (FAILED_IMAGE, IMAGE, 0, "before build"), (IMAGE, FAILED_IMAGE, 1, "during build")):
            with self.subTest(message=message):
                harness = self.fixture(before=before, after=after)
                with self.assertRaisesRegex(HARNESS.CheckError, message):
                    harness.derivative("candidate")
                calls = harness.commands.docker.call_args_list
                self.assertEqual(sum(call.args[:2] == ("buildx", "build") for call in calls), expected_builds)
                self.assertNotIn("candidate", harness.fixture_images)
                self.assertFalse(any(call.args[-1].startswith("simplestchat-release/production:") for call in calls))

    def test_failure_fixture_is_a_deterministic_startup_exit_not_an_unbounded_workload(self):
        harness = self.fixture()
        harness.derivative("failed")
        directory = harness.private / "failed"
        self.assertEqual((directory / "failed-server").read_text(),
                         f"#!/bin/sh\necho '{HARNESS.FAILURE_MARKER}' >&2\nexit 42\n")
        self.assertIn("COPY --chown=10001:10001 --chmod=0755 failed-server /app/simplestChat\n",
                      (directory / "Dockerfile").read_text())


if __name__ == "__main__":
    unittest.main()
