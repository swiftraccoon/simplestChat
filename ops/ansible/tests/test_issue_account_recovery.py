"""Verify private recovery delivery offline and atomic guards in owned PostgreSQL."""

from __future__ import annotations

import base64
import hashlib
import io
import os
import resource
import shutil
import stat
import subprocess
import tempfile
import unittest
import uuid
from contextlib import redirect_stderr, redirect_stdout
from dataclasses import replace
from pathlib import Path
from typing import TYPE_CHECKING, override
from unittest.mock import patch
from urllib.parse import urlsplit, urlunsplit

from test_support import objects, string

# isort: split
import bounded_process
import issue_account_recovery as recovery
from release_json import decode_json

if TYPE_CHECKING:
    from collections.abc import Sequence


def options_at(directory: str) -> recovery.Options:
    """Prepare only owned temporary files and synthetic account identities."""
    root = Path(directory)
    root.chmod(0o700)
    identity = root / "identity"
    _ = identity.write_text("inert fixture identity\n")
    identity.chmod(0o600)
    return recovery.Options(
        host="recovery.example.test",
        identity=identity,
        container="a" * 64,
        emails=("alpha@example.test", "beta@example.test"),
        output=root / "recovery.json",
    )


def arguments(options: recovery.Options) -> list[str]:
    """Build the actual CLI contract using only synthetic fixture values."""
    return [
        "--host",
        options.host,
        "--identity",
        str(options.identity),
        "--container",
        options.container,
        "--output",
        str(options.output),
        *[argument for email in options.emails for argument in ("--email", email)],
    ]


class AccountRecoveryTests(unittest.TestCase):
    """No unit test starts a network client or reads an existing private identity."""

    def test_generated_keys_match_application_encoding_and_digest(self) -> None:
        """Every key has 256 bits of unpadded URL-safe data and its exact SHA-256 digest."""
        pairs = [recovery.recovery_pair() for _ in range(4)]
        self.assertEqual(len({key for key, _ in pairs}), len(pairs))
        for key, digest in pairs:
            self.assertRegex(key, r"^sc-recovery-[A-Za-z0-9_-]{43}$")
            self.assertEqual(
                len(base64.urlsafe_b64decode(key.removeprefix("sc-recovery-") + "=")), 32
            )
            self.assertEqual(hashlib.sha256(key.encode("ascii")).hexdigest(), digest)

    def test_invalid_targets_stop_before_generation_or_transport(self) -> None:
        """Ambiguous identities and command-shaped targets cannot reach key generation."""
        with tempfile.TemporaryDirectory() as directory:
            options = options_at(directory)
            invalid = (
                replace(options, host="-invalid"),
                replace(options, host="host name"),
                replace(options, host="host;command"),
                replace(options, host="host..example.test"),
                replace(options, container="postgres"),
                replace(options, container="a" * 63),
                replace(options, emails=()),
                replace(options, emails=("alpha@example.test",) * 2),
                replace(options, emails=("Alpha@example.test",)),
                replace(options, emails=(" alpha@example.test",)),
                replace(options, emails=("alpha@example.test\n",)),
                replace(options, identity=Path("relative-identity")),
                replace(options, output=Path("relative-output")),
            )
            with (
                patch.object(resource, "setrlimit"),
                patch.object(recovery, "recovery_pair") as generate,
                patch.object(bounded_process, "run") as launch,
            ):
                for candidate in invalid:
                    with (
                        self.subTest(candidate=candidate),
                        self.assertRaises(recovery.RecoveryError),
                    ):
                        recovery.issue(candidate)
                generate.assert_not_called()
                launch.assert_not_called()
            self.assertFalse(options.output.exists())

    def test_private_inputs_and_output_cannot_follow_links_or_overwrite(self) -> None:
        """Reject accessible identities, accessible parents, symlinks and existing files."""
        with tempfile.TemporaryDirectory() as directory:
            options = options_at(directory)
            recovery.validate(options)
            options.identity.chmod(0o644)
            with self.assertRaises(recovery.RecoveryError):
                recovery.validate(options)
            options.identity.chmod(0o600)
            linked_identity = options.identity.with_name("linked-identity")
            linked_identity.symlink_to(options.identity)
            with self.assertRaises(recovery.RecoveryError):
                recovery.validate(replace(options, identity=linked_identity))
            options.output.parent.chmod(0o755)
            with self.assertRaises(recovery.RecoveryError):
                recovery.validate(options)
            options.output.parent.chmod(0o700)
            linked_parent = options.output.parent / "linked-parent"
            linked_parent.symlink_to(options.output.parent, target_is_directory=True)
            with self.assertRaises(recovery.RecoveryError):
                recovery.validate(replace(options, output=linked_parent / "recovery.json"))
            options.output.symlink_to(options.identity)
            with self.assertRaises(recovery.RecoveryError):
                recovery.validate(options)
            options.output.unlink()
            _ = options.output.write_text("preserve existing data")
            with (
                patch.object(resource, "setrlimit"),
                patch.object(bounded_process, "run") as launch,
                self.assertRaises(recovery.RecoveryError),
            ):
                recovery.issue(options)
            launch.assert_not_called()
            self.assertEqual(options.output.read_text(), "preserve existing data")

    def test_private_keys_are_durable_before_launch_and_never_sent(self) -> None:
        """Only digests reach stdin; neither process arguments nor public output contain keys."""
        with tempfile.TemporaryDirectory() as directory:
            options = options_at(directory)
            synced: list[int] = []
            public_output, public_error = io.StringIO(), io.StringIO()
            original_fsync = os.fsync

            def sync(descriptor: int) -> None:
                synced.append(stat.S_IFMT(os.fstat(descriptor).st_mode))
                original_fsync(descriptor)

            def launch(
                argv: Sequence[str],
                *,
                input_data: bytes,
                limits: bounded_process.Limits,
                **_options: object,
            ) -> tuple[int, bytes, bytes]:
                self.assertEqual(synced, [stat.S_IFREG, stat.S_IFDIR])
                self.assertEqual(stat.S_IMODE(options.output.stat().st_mode), 0o600)
                accounts = objects(decode_json(options.output.read_bytes()), "accounts")
                self.assertEqual(
                    [string(account, "email") for account in accounts], list(options.emails)
                )
                for account in accounts:
                    key = string(account, "recoveryKey")
                    self.assertNotIn(key, " ".join(argv))
                    self.assertNotIn(key.encode(), input_data)
                self.assertIn("StrictHostKeyChecking=yes", argv)
                self.assertIn("IdentitiesOnly=yes", argv)
                self.assertLessEqual(limits.timeout, 60)
                return 0, recovery.SUCCESS, b""

            with (
                patch.object(resource, "setrlimit"),
                patch.object(os, "fsync", side_effect=sync),
                patch.object(bounded_process, "run", side_effect=launch) as process,
                redirect_stdout(public_output),
                redirect_stderr(public_error),
            ):
                self.assertEqual(recovery.main(arguments(options)), 0)
            process.assert_called_once()
            self.assertEqual(public_output.getvalue().encode(), recovery.SUCCESS)
            self.assertEqual(public_error.getvalue(), "")

    def test_failed_durability_prevents_remote_launch(self) -> None:
        """A file or directory sync failure cannot issue credentials remotely."""
        for failed_sync in (1, 2):
            with tempfile.TemporaryDirectory() as directory:
                options = options_at(directory)
                failures = [None] * (failed_sync - 1) + [OSError("fixture sync failure")]
                with (
                    patch.object(resource, "setrlimit"),
                    patch.object(os, "fsync", side_effect=failures),
                    patch.object(bounded_process, "run") as launch,
                    self.assertRaises(OSError),
                ):
                    recovery.issue(options)
                launch.assert_not_called()

    def test_uncertain_remote_results_retain_private_keys_and_never_retry(self) -> None:
        """Failure, unexpected replies and timeouts retain the sole durable recovery copy."""
        failures: tuple[tuple[int, bytes, bytes] | Exception, ...] = (
            (255, b"", b"private remote diagnostic"),
            (0, b"unexpected private response", b""),
            bounded_process.ProcessError("command_timed_out"),
            OSError("private transport diagnostic"),
        )
        for failure in failures:
            with tempfile.TemporaryDirectory() as directory:
                options = options_at(directory)
                public_output, public_error = io.StringIO(), io.StringIO()
                with (
                    patch.object(resource, "setrlimit"),
                    patch.object(bounded_process, "run", side_effect=[failure]) as launch,
                    redirect_stdout(public_output),
                    redirect_stderr(public_error),
                ):
                    self.assertEqual(recovery.main(arguments(options)), 1)
                launch.assert_called_once()
                accounts = objects(decode_json(options.output.read_bytes()), "accounts")
                self.assertEqual(len(accounts), len(options.emails))
                self.assertEqual(stat.S_IMODE(options.output.stat().st_mode), 0o600)
                self.assertEqual(public_output.getvalue(), "")
                self.assertEqual(
                    public_error.getvalue(),
                    "account_recovery_failed_keep_private_file_no_automatic_retry\n",
                )


@unittest.skipUnless(os.environ.get("DISPOSABLE_TEST_DATABASE") == "1", "requires owned PostgreSQL")
class AccountRecoveryDatabaseTests(unittest.TestCase):
    """Use only a unique database inside the explicitly enabled local test cluster."""

    base: str = ""
    database: str = ""

    def sql(self, target: str, source: bytes) -> bytes:
        """Run psql with fatal errors, no user configuration and no inherited PG selectors."""
        executable = shutil.which("psql")
        if executable is None:
            self.fail("psql is required for the owned PostgreSQL fixture")
        return subprocess.run(  # noqa: S603 -- Fixed psql and explicitly owned fixture database.
            [executable, "-X", "-qAt", "--set=ON_ERROR_STOP=1", "--dbname", target],
            input=source,
            capture_output=True,
            check=True,
            timeout=30,
            env={"PATH": os.defpath, "LC_ALL": "C"},
        ).stdout

    @override
    def setUp(self) -> None:
        """Create a synthetic database only after enforcing the disposable URL contract."""
        self.base = os.environ["TEST_DATABASE_URL"]
        original = urlsplit(self.base)
        self.assertEqual(original.hostname, "127.0.0.1")
        self.assertEqual(original.username, "test_owner")
        self.assertEqual(original.path, "/simplestchat_test")
        self.assertEqual(original.query, "sslmode=disable")
        name = "recovery_" + uuid.uuid4().hex
        self.database = urlunsplit(original._replace(path="/" + name))
        _ = self.sql(self.base, f"CREATE DATABASE {name};".encode())
        self.addCleanup(self.sql, self.base, f"DROP DATABASE {name};".encode())
        _ = self.sql(
            self.database,
            b"""CREATE TABLE public.users (
                id uuid PRIMARY KEY DEFAULT gen_random_uuid(), email text UNIQUE NOT NULL,
                password_hash text, recovery_key_hash text, auth_version bigint NOT NULL DEFAULT 7,
                display_name text NOT NULL DEFAULT 'Fixture',
                updated_at timestamptz NOT NULL DEFAULT '2000-01-01');
            INSERT INTO public.users (email,password_hash,recovery_key_hash) VALUES
                ('alpha@example.test','existing-alpha-password',NULL),
                ('beta@example.test','existing-beta-password',NULL),
                ('protected@example.test','existing-protected-password',repeat('c',64));""",
        )

    def snapshot(self) -> bytes:
        """Read every fixture field so rollback assertions detect collateral mutations."""
        return self.sql(self.database, b"SELECT json_agg(u ORDER BY email) FROM public.users u;")

    def test_multiple_accounts_commit_only_requested_recovery_hashes(self) -> None:
        """Successful initialization preserves passwords, versions and unrelated recovery keys."""
        result = self.sql(
            self.database,
            recovery.transaction(
                [("alpha@example.test", "a" * 64), ("beta@example.test", "b" * 64)]
            ),
        )
        self.assertEqual(result, recovery.SUCCESS)
        self.assertEqual(
            self.sql(
                self.database,
                b"""SELECT email, password_hash, auth_version, recovery_key_hash,
                    updated_at > '2000-01-01'::timestamptz
                    FROM public.users ORDER BY email;""",
            ).decode(),
            "alpha@example.test|existing-alpha-password|7|"
            + "a" * 64
            + "|t\n"
            + "beta@example.test|existing-beta-password|7|"
            + "b" * 64
            + "|t\n"
            + "protected@example.test|existing-protected-password|7|"
            + "c" * 64
            + "|f\n",
        )

    def test_missing_or_already_enabled_account_rolls_back_every_change(self) -> None:
        """A single failed eligibility guard keeps all account fields byte-for-byte identical."""
        before = self.snapshot()
        for other in ("missing@example.test", "protected@example.test"):
            with self.subTest(other=other), self.assertRaises(subprocess.CalledProcessError):
                _ = self.sql(
                    self.database,
                    recovery.transaction([("alpha@example.test", "a" * 64), (other, "b" * 64)]),
                )
            self.assertEqual(self.snapshot(), before)

    def test_suppressed_update_rolls_back_the_entire_transaction(self) -> None:
        """A trigger suppressing one update exercises the post-update row-count guard."""
        _ = self.sql(
            self.database,
            b"""CREATE FUNCTION fixture_suppress_update() RETURNS trigger LANGUAGE plpgsql AS $$
                BEGIN IF NEW.email='beta@example.test' THEN RETURN NULL; END IF; RETURN NEW; END $$;
            CREATE TRIGGER fixture_skip BEFORE UPDATE ON public.users
                FOR EACH ROW EXECUTE FUNCTION fixture_suppress_update();""",
        )
        before = self.snapshot()
        with self.assertRaises(subprocess.CalledProcessError):
            _ = self.sql(
                self.database,
                recovery.transaction(
                    [("alpha@example.test", "a" * 64), ("beta@example.test", "b" * 64)]
                ),
            )
        self.assertEqual(self.snapshot(), before)


if __name__ == "__main__":
    _ = unittest.main()
