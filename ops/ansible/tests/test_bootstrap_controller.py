"""Offline bootstrap tests, including local PTYs; never contact a remote host."""

from __future__ import annotations

import base64
import io
import json
import os
import stat
import subprocess
import sys
import tempfile
import time
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from typing import TYPE_CHECKING, cast, override
from unittest.mock import patch

from test_support import ROOT

# isort: split
import bootstrap_access as ACCESS  # noqa: N812 -- Explicit module boundary in tests.
import bootstrap_controller as BOOT  # noqa: N812 -- Explicit controller under test.
import release_build as BUILD  # noqa: N812 -- Shared subprocess boundary.
from release_json import JsonObject, decode_json, object_value

if TYPE_CHECKING:
    from collections.abc import Sequence

REVISION = "a" * 40
PREVIOUS = "b" * 40
INITIAL = "fixture-initial-password"
REPLACEMENT = "fixture-replacement-password"
PUBLIC = "ssh-ed25519 " + base64.b64encode(b"fixture-public-key").decode()

PTY_HEADER = """
import sys, termios, time
attributes = termios.tcgetattr(0)
attributes[3] &= ~termios.ECHO
termios.tcsetattr(0, termios.TCSANOW, attributes)
def prompt(text):
    print(text, end='', flush=True)
    return input()
"""


class DialogueTests(unittest.TestCase):
    """Exercise ordering, limits and real terminal echo/EOF behavior."""

    def test_forced_change_prompts_are_ordered_and_not_in_repr(self) -> None:
        """Only the intended secret is returned for each supported prompt."""
        dialogue = ACCESS.PasswordDialogue(INITIAL, REPLACEMENT)
        self.assertEqual(dialogue.consume("debian@host's password: "), INITIAL)
        self.assertEqual(dialogue.consume("Current password: "), INITIAL)
        self.assertEqual(dialogue.consume("New password: "), REPLACEMENT)
        self.assertEqual(dialogue.consume("Retype new password: "), REPLACEMENT)
        self.assertIsNone(dialogue.consume("passwd: password updated successfully\n"))
        self.assertTrue(dialogue.rotated)
        self.assertNotIn(INITIAL, repr(dialogue))
        self.assertNotIn(REPLACEMENT, repr(dialogue))

    def test_repeated_and_reordered_prompts_fail_without_echoing_input(self) -> None:
        """A rejected password never triggers an unbounded retry loop."""
        dialogue = ACCESS.PasswordDialogue(INITIAL, REPLACEMENT)
        _ = dialogue.consume("password: ")
        with self.assertRaisesRegex(ACCESS.BootstrapError, "repeated_password_prompt"):
            _ = dialogue.consume("password: ")
        with self.assertRaisesRegex(ACCESS.BootstrapError, "password_prompt_order"):
            _ = ACCESS.PasswordDialogue(INITIAL, REPLACEMENT).consume("Retype new password: ")

    @unittest.skipUnless(os.name == "posix", "OpenSSH bootstrap requires POSIX PTYs")
    def test_real_pty_forced_change_can_exit_before_enrollment(self) -> None:
        """Model Debian PAM's successful rotation followed by connection closure."""
        program = (
            PTY_HEADER
            + """
for text in ('password: ', 'Current password: ', 'New password: ', 'Retype new password: '):
    prompt(text)
print('passwd: password updated successfully', flush=True)
sys.exit(1)
"""
        )
        outcome = ACCESS.password_session(
            [sys.executable, "-u", "-c", program], INITIAL, REPLACEMENT, timeout=5
        )
        self.assertTrue(outcome.rotated)
        self.assertFalse(outcome.enrolled)
        self.assertEqual(outcome.exit_status, 1)

    def test_real_pty_drains_markers_after_large_final_burst(self) -> None:
        """A reaped child may leave more than one read's worth of PTY data."""
        program = (
            PTY_HEADER
            + f"""
prompt('password: ')
print('x' * 12000)
print({ACCESS.ENROLLED!r}, flush=True)
"""
        )
        outcome = ACCESS.password_session(
            [sys.executable, "-u", "-c", program], INITIAL, REPLACEMENT, timeout=5
        )
        self.assertTrue(outcome.enrolled)
        self.assertEqual(outcome.exit_status, 0)

    def test_enabled_echo_aborts_before_writing_password(self) -> None:
        """A password-looking banner cannot elicit a secret while terminal echo is on."""
        program = "print('password: ', end='', flush=True); input()"
        with self.assertRaisesRegex(ACCESS.BootstrapError, "password_terminal_echo_enabled"):
            _ = ACCESS.password_session(
                [sys.executable, "-u", "-c", program], INITIAL, REPLACEMENT, timeout=3
            )

    def test_timeout_reaps_child_and_does_not_print_transcript(self) -> None:
        """A stalled authenticated session remains bounded and silent."""
        program = PTY_HEADER + "prompt('password: '); print('private-banner'); time.sleep(20)"
        output = io.StringIO()
        started = time.monotonic()
        with (
            redirect_stdout(output),
            redirect_stderr(output),
            self.assertRaisesRegex(ACCESS.BootstrapError, "password_session_timeout"),
        ):
            _ = ACCESS.password_session(
                [sys.executable, "-u", "-c", program], INITIAL, REPLACEMENT, timeout=0.25
            )
        self.assertLess(time.monotonic() - started, 3)
        self.assertEqual(output.getvalue(), "")

    def test_output_limit_discards_untrusted_banner(self) -> None:
        """Oversized remote output cannot become an unbounded secret transcript."""
        with self.assertRaisesRegex(ACCESS.BootstrapError, "password_output_limit"):
            _ = ACCESS.password_session(
                [sys.executable, "-u", "-c", "print('x' * 70000)"],
                INITIAL,
                REPLACEMENT,
                timeout=3,
            )


class BootstrapTests(unittest.TestCase):
    """Verify local filesystem, trust, inventory and orchestration contracts."""

    def __init__(self, methodName: str = "runTest") -> None:  # noqa: N803 -- unittest API.
        """Declare typed fixture state before unittest invokes setUp."""
        super().__init__(methodName)
        self.base: Path = Path()
        self.root: Path = Path()
        self.private: Path = Path()
        self.target: BOOT.BootstrapTarget = BOOT.BootstrapTarget("", "", 22, Path(), Path())
        self.args: BOOT.BootstrapOptions = BOOT.BootstrapOptions()

    @override
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name).resolve()
        self.root = self.base / "checkout"
        (self.root / "ops/ansible").mkdir(parents=True)
        self.private = self.base / "private"
        self.private.mkdir(mode=0o700)
        self.target = BOOT.BootstrapTarget(
            "test.example.invalid",
            "debian",
            22,
            self.private / "key",
            self.private / "known_hosts",
        )
        self.args = BOOT.BootstrapOptions(
            host=self.target.host,
            user=self.target.user,
            name="test_vps",
            revision=REVISION,
            identity=str(self.target.identity),
            known_hosts=str(self.target.known_hosts),
            inventory=str(self.root / "ops/ansible/inventory.local.test.yml"),
            output=str(self.root / "evidence"),
        )
        root_patch = patch.object(BOOT, "ROOT", self.root)
        _ = root_patch.start()
        self.addCleanup(root_patch.stop)

    def secret(self, name: str, value: str) -> Path:
        """Create one synthetic protected fixture outside the checkout."""
        path = self.private / name
        BOOT.write_new(path, value.encode())
        return path

    def test_actual_entrypoint_help_without_test_import_bootstrap(self) -> None:
        """The executable wrapper must resolve sibling host-helper imports itself."""
        completed = subprocess.run(  # noqa: S603 -- Fixed local help-only entry point.
            [sys.executable, str(ROOT / "build/bootstrap.py"), "--help"],
            cwd=self.base,
            capture_output=True,
            text=True,
            check=False,
            timeout=10,
            env={key: value for key, value in os.environ.items() if key != "PYTHONPATH"},
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertIn("--host-fingerprint", completed.stdout)

    def test_private_files_reject_permissions_symlinks_and_checkout(self) -> None:
        """Credential input cannot silently accept shared files or tracked locations."""
        path = self.secret("password", INITIAL)
        self.assertEqual(BOOT.password_value(path, "", self.root), INITIAL)
        path.chmod(0o644)
        with self.assertRaisesRegex(ACCESS.BootstrapError, "invalid_private_file_mode"):
            _ = BOOT.password_value(path, "", self.root)
        link = self.private / "link"
        link.symlink_to(path)
        with self.assertRaisesRegex(ACCESS.BootstrapError, "selected_path_is_symlink"):
            _ = BOOT.local_path(str(link))
        with self.assertRaisesRegex(ACCESS.BootstrapError, "credential_path_inside_checkout"):
            _ = BOOT.password_value(self.root / "password", "", self.root)

    def test_replacement_is_saved_exclusively_and_reused(self) -> None:
        """Recovery credentials exist before SSH and a retry never replaces them."""
        path = self.private / "replacement.json"
        first = BOOT.replacement_password(path, self.root)
        self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
        original = path.read_bytes()
        self.assertEqual(BOOT.replacement_password(path, self.root), first)
        self.assertEqual(path.read_bytes(), original)

    def test_host_scan_requires_independent_matching_pin(self) -> None:
        """Scanned keys are candidates, never an implicit trust decision."""
        encoded = PUBLIC.split()[1]
        fingerprint = BOOT.key_fingerprint(encoded)
        with patch.object(BOOT, "run_command") as run:
            with self.assertRaisesRegex(
                ACCESS.BootstrapError, "verified_host_fingerprint_required"
            ):
                _ = BOOT.host_trust(self.target, None)
            run.assert_not_called()
        with patch.object(BOOT, "run_command", return_value=(0, f"host {PUBLIC}")):
            with self.assertRaisesRegex(ACCESS.BootstrapError, "host_fingerprint_mismatch"):
                _ = BOOT.host_trust(self.target, "SHA256:" + "x" * 43)
            self.assertFalse(self.target.known_hosts.exists())
            self.assertEqual(BOOT.host_trust(self.target, fingerprint), fingerprint)
        self.assertIn(PUBLIC, self.target.known_hosts.read_text())

    def test_existing_known_hosts_can_be_publicly_readable_and_is_preserved(self) -> None:
        """Trusted existing entries need no new scan or fingerprint prompt."""
        _ = self.secret("known_hosts", f"other.invalid {PUBLIC}\n{self.target.host} {PUBLIC}\n")
        self.target.known_hosts.chmod(0o644)
        original = self.target.known_hosts.read_bytes()
        with patch.object(
            BOOT, "run_command", return_value=(0, f"{self.target.host} {PUBLIC}")
        ) as run:
            self.assertEqual(BOOT.host_trust(self.target, None), "existing_known_hosts")
            self.assertEqual(run.call_count, 1)
        self.assertEqual(self.target.known_hosts.read_bytes(), original)

    def test_known_hosts_requires_a_nonreplaceable_parent(self) -> None:
        """Even an owner-only trust file is unsafe in a group-writable directory."""
        _ = self.secret("known_hosts", f"{self.target.host} {PUBLIC}\n")
        self.private.chmod(0o770)
        with self.assertRaisesRegex(ACCESS.BootstrapError, "unsafe_private_parent_mode"):
            _ = BOOT.host_trust(self.target, None)

    def test_key_success_never_reads_password_files_or_rotates(self) -> None:
        """Key-first reruns tolerate SSH banners and bypass all password operations."""
        self.args.password_file = str(self.private / "must-not-read")
        self.args.replacement_password_file = str(self.private / "must-not-create")
        with (
            patch.object(
                BOOT, "run_command", return_value=(0, "Legal banner\nSIMPLESTCHAT_KEY_READY")
            ),
            patch.object(BOOT, "password_value") as password,
            patch.object(BOOT, "replacement_password") as replacement,
            patch.object(BOOT, "password_session") as session,
        ):
            self.assertEqual(BOOT.enroll(self.target, PUBLIC, self.args), "existing_key")
            password.assert_not_called()
            replacement.assert_not_called()
            session.assert_not_called()

    def test_forced_change_reconnects_once_with_replacement(self) -> None:
        """Successful PAM rotation with nonzero SSH exit still proceeds to enrollment."""
        self.args.password_file = str(self.secret("initial", INITIAL))
        self.args.replacement_password_file = str(self.secret("replacement", REPLACEMENT))
        with (
            patch.object(
                BOOT, "run_command", side_effect=[(255, ""), (0, "SIMPLESTCHAT_KEY_READY")]
            ),
            patch.object(
                BOOT,
                "password_session",
                side_effect=[
                    ACCESS.PasswordOutcome(enrolled=False, rotated=True, exit_status=1),
                    ACCESS.PasswordOutcome(enrolled=True, rotated=False, exit_status=0),
                ],
            ) as session,
        ):
            self.assertEqual(
                BOOT.enroll(self.target, PUBLIC, self.args), "password_rotated_and_key_enrolled"
            )
            self.assertEqual(session.call_count, 2)
            self.assertEqual(session.call_args_list[1].args[1:], (REPLACEMENT, REPLACEMENT))
            for call in session.call_args_list:
                self.assertNotIn(INITIAL, str(cast("object", call.args[0])))
                self.assertNotIn(REPLACEMENT, str(cast("object", call.args[0])))
        self.assertEqual(Path(self.args.replacement_password_file).read_text(), REPLACEMENT)

    def test_failed_authentication_does_not_retry_or_provision(self) -> None:
        """A failed password is not retried automatically, limiting account lockouts."""
        self.args.password_file = str(self.secret("initial", INITIAL))
        self.args.replacement_password_file = str(self.secret("replacement", REPLACEMENT))
        with (
            patch.object(BOOT, "run_command", return_value=(255, "")),
            patch.object(
                BOOT,
                "password_session",
                return_value=ACCESS.PasswordOutcome(enrolled=False, rotated=False, exit_status=255),
            ) as session,
        ):
            with self.assertRaisesRegex(ACCESS.BootstrapError, "key_enrollment_failed"):
                _ = BOOT.enroll(self.target, PUBLIC, self.args)
            self.assertEqual(session.call_count, 1)

    def test_enrollment_preserves_existing_authorized_keys_idempotently(self) -> None:
        """Execute only the generated POSIX enrollment script in a disposable HOME."""
        home = self.private / "remote-home"
        ssh = home / ".ssh"
        ssh.mkdir(parents=True, mode=0o700)
        authorized = ssh / "authorized_keys"
        original = "ssh-ed25519 unrelated-key-without-trailing-newline"
        _ = authorized.write_text(original)
        command = BOOT.enrollment_command(PUBLIC)
        for _ in range(2):
            result = subprocess.run(  # noqa: S603 -- Generated reviewed enrollment command, disposable HOME only.
                ["/bin/sh", "-c", command],
                check=False,
                capture_output=True,
                text=True,
                env=dict(os.environ, HOME=str(home)),
                timeout=5,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(authorized.read_text().startswith(original))
        self.assertEqual(authorized.read_text().count(PUBLIC), 1)
        self.assertEqual(stat.S_IMODE(authorized.stat().st_mode), 0o600)

    def test_inventory_creation_then_revision_override_preserves_bytes_and_extras(self) -> None:
        """Source advancement is an extra var, never an inventory rewrite."""
        path = Path(self.args.inventory)
        with patch.object(BOOT, "run_command", return_value=(0, "")):
            _ = BOOT.prepare_inventory(path, self.target, self.args)
            data = object_value(decode_json(path.read_text()))
            host = object_value(
                object_value(object_value(data["benchmark_hosts"])["hosts"])[self.args.name]
            )
            host["scbench_revision"] = PREVIOUS
            host["scpub_port_mbps"] = 2000
            host["scpub_transfer_allowance_tb"] = 0
            _ = path.write_text(json.dumps(data))
            previous = path.read_bytes()
            _ = BOOT.prepare_inventory(path, self.target, self.args)
            self.assertEqual(path.read_bytes(), previous)
        command = BOOT.site_command(self.root, path, self.args.name, REVISION, maintenance=True)
        overrides = object_value(decode_json(command[-1]))
        self.assertEqual(overrides["scbench_revision"], REVISION)
        self.assertTrue(overrides["scbench_upgrade_packages"])
        self.assertTrue(overrides["scbench_reboot"])
        self.assertTrue(host["scbench_upgrade_packages"] is False)
        self.assertTrue(host["scbench_reboot"] is False)

    def test_recovery_file_data_and_directory_entry_are_synced_in_order(self) -> None:
        """Local recovery material is durable before a later remote password change."""
        kinds: list[bool] = []

        def record_sync(descriptor: int) -> None:
            kinds.append(stat.S_ISDIR(os.fstat(descriptor).st_mode))

        with patch.object(os, "fsync", side_effect=record_sync):
            BOOT.write_new(self.private / "recovery", b"fixture")
        self.assertEqual(kinds, [False, True])

    def test_controller_environment_excludes_inherited_code_and_agent_hooks(self) -> None:
        """Automation uses a reviewed PATH and never inherits process/plugin injection hooks."""
        with patch.dict(
            os.environ,
            {
                "PYTHONPATH": "fixture",
                "LD_PRELOAD": "fixture",
                "SSH_AUTH_SOCK": "fixture",
                "ANSIBLE_INVENTORY_ENABLED": "script",
                "BASH_ENV": "fixture",
                "LANG": "fixture-invalid-locale",
                "LC_ALL": "C",
            },
        ):
            environment = BOOT.ansible_environment(self.root)
        for key in (
            "PYTHONPATH",
            "LD_PRELOAD",
            "SSH_AUTH_SOCK",
            "ANSIBLE_INVENTORY_ENABLED",
            "BASH_ENV",
        ):
            self.assertNotIn(key, environment)
        self.assertEqual(BOOT.ssh_command(self.target, "true")[0], "/usr/bin/ssh")
        self.assertEqual(
            set(environment),
            {
                "PATH",
                "HOME",
                "LC_ALL",
                "LANG",
                "ANSIBLE_CONFIG",
                "ANSIBLE_HOST_KEY_CHECKING",
                "ANSIBLE_RETRY_FILES_ENABLED",
            },
        )
        self.assertEqual(environment["LC_ALL"], "C.UTF-8")
        self.assertEqual(environment["LANG"], "C.UTF-8")
        generic = BOOT.controller_environment()
        self.assertEqual(generic["LC_ALL"], "C")
        self.assertEqual(generic["LANG"], "C")

    def test_ansible_cli_imports_with_the_isolated_environment(self) -> None:
        """Real pinned Ansible startup requires UTF-8 on both Linux and macOS."""
        completed = subprocess.run(
            [sys.executable, "-m", "ansible.cli.playbook", "--version"],
            cwd=self.base,
            capture_output=True,
            text=True,
            check=False,
            timeout=15,
            env=BOOT.ansible_environment(ROOT),
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertIn("ansible-playbook [core ", completed.stdout)

    def test_inventory_transport_override_and_wrong_host_rejected(self) -> None:
        """An existing inventory cannot redirect the selected target or SSH executable."""
        path = Path(self.args.inventory)
        for key, value in (
            ("ansible_host", "different.invalid"),
            ("ansible_ssh_executable", "/untrusted/ssh"),
            ("ansible_ssh_extra_args", "-o StrictHostKeyChecking=no"),
        ):
            data = BOOT.inventory_value(self.target, self.args)
            host = object_value(
                object_value(object_value(data["benchmark_hosts"])["hosts"])[self.args.name]
            )
            host[key] = value
            if path.exists():
                path.unlink()
            BOOT.write_record(path, data)
            with (
                patch.object(BOOT, "run_command", return_value=(0, "")),
                self.assertRaises(ACCESS.BootstrapError),
            ):
                _ = BOOT.prepare_inventory(path, self.target, self.args)

    def test_inventory_groups_and_group_variables_are_rejected(self) -> None:
        """Inherited group data cannot bypass the one-host transport boundary."""
        path = Path(self.args.inventory)
        extra_group = BOOT.inventory_value(self.target, self.args)
        extra_group["all"] = {"vars": {"ansible_ssh_extra_args": "untrusted"}}
        group_variables = BOOT.inventory_value(self.target, self.args)
        object_value(group_variables["benchmark_hosts"])["vars"] = {"ansible_user": "other"}
        for data in (extra_group, group_variables):
            if path.exists():
                path.unlink()
            BOOT.write_record(path, data)
            with (
                patch.object(BOOT, "run_command", return_value=(0, "")),
                self.assertRaises(ACCESS.BootstrapError),
            ):
                _ = BOOT.prepare_inventory(path, self.target, self.args)

    def test_preflight_ignores_banner_and_requires_one_metadata_record(self) -> None:
        """Public SSH banners may precede bounded allowlisted metadata."""
        record = 'SIMPLESTCHAT_BOOTSTRAP_FACTS {"version":"13"}'
        with patch.object(BOOT, "run_command", return_value=(0, "Legal banner\n" + record)):
            self.assertEqual(BOOT.remote_python(self.target, "pass"), {"version": "13"})
        with (
            patch.object(BOOT, "run_command", return_value=(0, record + "\n" + record)),
            self.assertRaisesRegex(ACCESS.BootstrapError, "invalid_preflight_envelope"),
        ):
            _ = BOOT.remote_python(self.target, "pass")

    def test_private_transport_is_explicit_and_password_free(self) -> None:
        """OpenSSH does not inherit aliases, forwarding, agents or a password fallback."""
        command = BOOT.ssh_command(self.target, "true")
        for option in (
            "BatchMode=yes",
            "StrictHostKeyChecking=yes",
            "IdentityAgent=none",
            "ForwardAgent=no",
            "PasswordAuthentication=no",
            "ControlMaster=no",
        ):
            self.assertIn(option, command)
        self.assertIn("/dev/null", command)
        self.assertNotIn(INITIAL, " ".join(command))

    def test_busy_capacity_journal_and_unsupported_os_block_provisioning(self) -> None:
        """Read-only host gates cover the capacity runner's shared ownership journal."""
        facts: JsonObject = {
            "distribution": "debian",
            "version": "13",
            "architecture": "x86_64",
            "systemd": True,
            "freeBytes": 40 * BOOT.GIB,
            "passwordlessSudo": True,
        }
        with (
            patch.object(BOOT, "remote_python", side_effect=[facts, {"busy": True}]),
            self.assertRaisesRegex(ACCESS.BootstrapError, "host_workload_active_or_unfinished"),
        ):
            _ = BOOT.preflight(self.target, 20)
        facts["version"] = "12"
        with (
            patch.object(BOOT, "remote_python", return_value=facts),
            self.assertRaisesRegex(ACCESS.BootstrapError, "unsupported_distribution"),
        ):
            _ = BOOT.preflight(self.target, 20)

    def test_provision_retains_evidence_and_sudo_file_is_removed(self) -> None:
        """Only a private filename reaches Ansible, and logs have an evidence directory."""
        self.args.become_password_file = str(self.secret("sudo", INITIAL))
        output = self.root / "result"
        output.mkdir(mode=0o700)
        commands: list[list[str]] = []

        def capture(
            runner: BUILD.Runner, argv: Sequence[str], **_kwargs: object
        ) -> tuple[int, str]:
            self.assertEqual(runner.output, output / "provision")
            commands.append(list(argv))
            secret_path = Path(argv[argv.index("--become-password-file") + 1])
            self.assertEqual(secret_path.read_text(), INITIAL + "\n")
            self.assertEqual(stat.S_IMODE(secret_path.stat().st_mode), 0o600)
            return 0, ""

        with (
            patch.object(BOOT, "run_command", return_value=(0, "")),
            patch.object(BUILD.Runner, "run", autospec=True, side_effect=capture),
        ):
            BOOT.provision(
                self.target,
                self.args,
                Path(self.args.inventory),
                {"passwordlessSudo": False},
                output,
            )
            self.assertEqual(len(commands), 1)
            arguments = commands[0]
            self.assertIn("--become-password-file", arguments)
            self.assertNotIn(INITIAL, str(arguments))
            self.assertIn(
                BOOT.inventory_ssh_common_args(self.target),
                str(decode_json(arguments[arguments.index("--become-password-file") - 1])),
            )
        self.assertEqual(list(self.private.glob(".simplestchat-become-*")), [])


if __name__ == "__main__":
    _ = unittest.main()
