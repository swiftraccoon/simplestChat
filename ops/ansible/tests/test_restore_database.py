"""Verify real archive restoration only in the explicitly owned PostgreSQL test cluster."""

import hashlib
import os
import shutil
import subprocess
import tempfile
import unittest
import uuid
from pathlib import Path
from unittest.mock import patch
from urllib.parse import urlsplit, urlunsplit

from test_support import ROOT, string, yaml_value

# isort: split
import release_public as release
import restore_verify as restore
from release_json import decode_json, object_value

POSTGRES_IMAGE = (
    "docker.io/library/postgres:18.6-bookworm@sha256:"
    "1c59e2c3c818eaa0f0628f695b36e7c9e362d6b219b36a54a32df645cbd7e1af"
)


def diagnostic_tail(evidence: Path) -> str:
    """Expose bounded fixture-only errors; never include an archive or command stdout."""
    result: list[str] = []
    for path in sorted(evidence.glob("*.stderr"))[-8:]:
        with path.open("rb") as source:
            _ = source.seek(0, os.SEEK_END)
            _ = source.seek(max(0, source.tell() - 1024))
            tail = source.read(1024).decode("utf-8", errors="replace")
        # Escape terminal controls while preserving readable error text and line breaks.
        clean = "".join(char if char.isprintable() or char == "\n" else "?" for char in tail)
        if clean.strip():
            result.append(path.name + ":\n" + clean)
    return "\n".join(result) or "No command stderr was emitted; inspect the private evidence."


class RestoreDiagnosticTests(unittest.TestCase):
    """Keep container-fixture failures actionable without publishing archive or stdout data."""

    def test_failure_diagnostics_are_bounded_stderr_only(self) -> None:
        """Old output and terminal control bytes cannot escape the selected short stderr tails."""
        with tempfile.TemporaryDirectory() as directory:
            evidence = Path(directory)
            for number in range(10):
                _ = (evidence / f"{number:03d}.stderr").write_bytes(
                    b"x" * 2000 + b"\x1b[31merror\n"
                )
            _ = (evidence / "010.stdout").write_text("excluded fixture stdout")
            _ = (evidence / "owned.dump").write_text("excluded fixture archive")
            actual = diagnostic_tail(evidence)
            self.assertNotIn("000.stderr", actual)
            self.assertNotIn("001.stderr", actual)
            self.assertIn("009.stderr", actual)
            self.assertNotIn("\x1b", actual)
            self.assertNotIn("excluded fixture", actual)
            self.assertLess(len(actual), 8 * 1100)


@unittest.skipUnless(os.environ.get("DISPOSABLE_TEST_DATABASE") == "1", "requires owned PostgreSQL")
class RestoreDatabaseTests(unittest.TestCase):
    """Restore actual schema, data, ACLs and incident history without using production data."""

    def command(self, program: str, *arguments: str, data: bytes | None = None) -> bytes:
        """Execute installed PostgreSQL tools only against this test-owned cluster."""
        executable = shutil.which(program)
        if executable is None:
            self.fail(f"{program} is required")
        return subprocess.run(  # noqa: S603 - fixed tools and explicitly disposable database.
            [executable, *arguments],
            input=data,
            capture_output=True,
            check=True,
            timeout=30,
        ).stdout

    def sql(self, database: str, source: str) -> bytes:
        """Send reviewed SQL with errors fatal and no client configuration."""
        return self.command(
            "psql",
            "-X",
            "-q",
            "-t",
            "-A",
            "--set",
            "ON_ERROR_STOP=1",
            database,
            data=source.encode(),
        )

    def verify_container(self, archive: Path, migrations: dict[str, str]) -> None:
        """Exercise the exact production verifier with an already pulled immutable PG18 image."""
        configured = yaml_value((ROOT / "ops/ansible/group_vars/benchmark_hosts.yml").read_text())
        self.assertEqual(POSTGRES_IMAGE, string(configured, "scpub_postgres_image"))
        evidence = Path(
            tempfile.mkdtemp(
                prefix="simplestchat-restore-container.",
                dir=os.environ.get("RUNNER_TEMP"),
            )
        )
        runner = release.Runner(evidence)
        try:
            image = (
                runner.docker("image", "inspect", "--format", "{{.Id}}", POSTGRES_IMAGE)
                .decode()
                .strip()
            )
            self.assertRegex(image, r"^sha256:[a-f0-9]{64}$")
            with patch.object(restore, "VERIFY_SQL", ROOT / "ops/ansible/files/restore-verify.sql"):
                report = restore.verify(runner, image, archive, migrations)
            self.assertTrue(report["verified"])
            self.assertTrue(report["cleanupPassed"])
            self.assertEqual(
                object_value(report["counts"]),
                {
                    "users": 1,
                    "rooms": 1,
                    "sessions": 0,
                    "credentials": 0,
                    "activeIncidents": 1,
                    "resolvedIncidents": 1,
                },
            )
            self.assertEqual(
                runner.docker(
                    "ps", "--all", "--quiet", "--filter", f"label={restore.LABEL}"
                ).strip(),
                b"",
            )
        except (release.ReleaseError, OSError, ValueError, subprocess.SubprocessError) as error:
            self.fail(
                f"PostgreSQL18 restore fixture failed ({type(error).__name__}); "
                + f"private evidence: {evidence}\n{diagnostic_tail(evidence)}"
            )

    def verify_external_schema(self, target: str, verification: str) -> None:
        """Require complete new operational evidence with private runtime grants."""
        # A complete older archive is valid; any present external schema must be complete.
        _ = self.sql(
            target,
            "SET ROLE postgres;\n"
            + (ROOT / "ops/ansible/files/monitoring-external-schema.sql").read_text(),
        )
        _ = self.sql(target, verification)
        _ = self.sql(target, "ALTER TABLE operations.external_status RENAME TO fixture_missing;")
        with self.assertRaises(subprocess.CalledProcessError):
            _ = self.sql(target, verification)
        _ = self.sql(target, "ALTER TABLE operations.fixture_missing RENAME TO external_status;")
        _ = self.sql(target, "GRANT SELECT ON operations.external_runs TO simplestchat_app;")
        with self.assertRaises(subprocess.CalledProcessError):
            _ = self.sql(target, verification)
        _ = self.sql(target, "REVOKE SELECT ON operations.external_runs FROM simplestchat_app;")
        _ = self.sql(target, verification)

    def test_restore_preserves_incidents_privacy_and_rejects_truncated_archives(self) -> None:
        """Full restoration retains active/resolved rows and rejects privacy/schema drift."""
        original = urlsplit(os.environ["TEST_DATABASE_URL"])
        base = os.environ["TEST_DATABASE_URL"]
        names = ["restore_" + uuid.uuid4().hex for _ in range(3)]
        urls = [urlunsplit(original._replace(path="/" + name)) for name in names]
        _ = self.sql(
            base,
            """DO $$ BEGIN
            IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='postgres') THEN
                CREATE ROLE postgres NOLOGIN;
            END IF;
            IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='simplestchat_app') THEN
                CREATE ROLE simplestchat_app NOLOGIN;
                CREATE ROLE simplestchat_migrate NOLOGIN;
            END IF;
        END $$;""",
        )
        try:
            for name in names:
                _ = self.sql(base, f"CREATE DATABASE {name} OWNER postgres;")
            source, target, corrupt = urls
            _ = self.sql(
                source,
                """SET ROLE postgres;
                REVOKE ALL ON SCHEMA public FROM PUBLIC;
                GRANT USAGE,CREATE ON SCHEMA public TO simplestchat_migrate;
                GRANT USAGE ON SCHEMA public TO simplestchat_app;
                CREATE EXTENSION pg_trgm;
                SET ROLE simplestchat_migrate;
                CREATE TABLE public._sqlx_migrations(version BIGINT PRIMARY KEY,
                  success BOOLEAN NOT NULL, checksum BYTEA NOT NULL);""",
            )
            migrations: dict[str, str] = {}
            for migration in sorted((ROOT / "migrations").glob("*.sql")):
                checksum = hashlib.sha384(migration.read_bytes()).hexdigest()
                version = int(migration.name.split("_")[0])
                migrations[str(version)] = checksum
                _ = self.sql(
                    source,
                    "SET ROLE simplestchat_migrate;\n"
                    + migration.read_text()
                    + f"\nINSERT INTO public._sqlx_migrations VALUES ({version},true,"
                    + f"decode('{checksum}','hex'));",
                )
            _ = self.sql(
                source, (ROOT / "ops/ansible/templates/public-runtime-grants.sql.j2").read_text()
            )
            _ = self.sql(
                source,
                "SET ROLE postgres;\n"
                + (ROOT / "ops/ansible/files/monitoring-schema.sql").read_text(),
            )
            _ = self.sql(
                source,
                """INSERT INTO public.users(id,email,display_name)
                VALUES ('00000000-0000-0000-0000-000000000001','fixture@example.test','Fixture');
                INSERT INTO public.rooms(id,owner_id,display_name)
                VALUES ('fixture','00000000-0000-0000-0000-000000000001','Fixture');
                INSERT INTO operations.alerts(incident_key,rule,severity,resource,
                    first_seen,last_seen,resolved_at)
                VALUES (repeat('a',64),'DatabaseUnavailable','critical','{}',now(),now(),NULL),
                       (repeat('b',64),'CollectorStale','warning','{}',now(),now(),now());
                INSERT INTO operations.alert_cursor VALUES (true,now());""",
            )
            with tempfile.TemporaryDirectory() as directory:
                archive = Path(directory) / "owned.dump"
                _ = self.command("pg_dump", "--format=custom", "--file", str(archive), source)
                if os.environ.get("RESTORE_CONTAINER_E2E") == "1":
                    self.verify_container(archive, migrations)
                _ = self.command(
                    "pg_restore",
                    "--exit-on-error",
                    "--single-transaction",
                    "--dbname",
                    target,
                    str(archive),
                )
                verification = (ROOT / "ops/ansible/files/restore-verify.sql").read_text()
                counts = object_value(decode_json(self.sql(target, verification)))
                self.assertEqual(counts["users"], 1)
                self.assertEqual(counts["activeIncidents"], 1)
                self.assertEqual(counts["resolvedIncidents"], 1)
                self.verify_external_schema(target, verification)
                _ = self.command(
                    "pg_amcheck",
                    "--install-missing",
                    "--heapallindexed",
                    "--parent-check",
                    "--maintenance-db",
                    target,
                    target,
                )
                _ = self.sql(target, "GRANT USAGE ON SCHEMA operations TO simplestchat_app;")
                with self.assertRaises(subprocess.CalledProcessError):
                    _ = self.sql(target, verification)
                _ = self.sql(
                    target,
                    """REVOKE USAGE ON SCHEMA operations FROM simplestchat_app;
                    ALTER TABLE public.users ADD CONSTRAINT restore_fixture
                    CHECK (length(email)>0) NOT VALID;""",
                )
                with self.assertRaises(subprocess.CalledProcessError):
                    _ = self.sql(target, verification)
                _ = archive.write_bytes(archive.read_bytes()[:100])
                with self.assertRaises(subprocess.CalledProcessError):
                    _ = self.command(
                        "pg_restore",
                        "--exit-on-error",
                        "--single-transaction",
                        "--dbname",
                        corrupt,
                        str(archive),
                    )
                self.assertEqual(
                    self.sql(corrupt, "SELECT to_regclass('public.users') IS NULL;").strip(), b"t"
                )
        finally:
            for name in names:
                _ = self.sql(base, f"DROP DATABASE IF EXISTS {name} WITH (FORCE);")


if __name__ == "__main__":
    _ = unittest.main()
