"""Keep SQL migration lint aligned with SQLx without allowing scanner suppressions."""

from __future__ import annotations

import json
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path
from typing import cast
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "build"))

import security_check
import security_findings
from security_context import Context
from security_tools import ToolError

INDEX = "CREATE INDEX CONCURRENTLY message_lookup ON chat_messages (recipient_account, id);\n"
MARKER = "-- no-transaction\n"
DATABASE_SOURCE = (
    'let maintenance = PgConnectOptions::from_str(&url).context("invalid DATABASE_URL")?'
    + '.options([("statement_timeout", "10min"), ("lock_timeout", "1min")]);\n'
    + "let connection = PgConnection::connect_with(&maintenance).await?;"
)


def maintenance_fixture(root: Path) -> None:
    """Provide the literal connection timeout contract that production lint verifies."""
    (root / "src").mkdir()
    _ = (root / "src/db.rs").write_text(DATABASE_SOURCE)


class MigrationExecutionPolicyTests(unittest.TestCase):
    """Only a real SQLx execution-mode marker changes the scanner's transaction mode."""

    def test_default_and_single_concurrent_index_modes(self) -> None:
        """Normal migrations retain transactional lint; index expressions remain supported."""
        self.assertTrue(
            security_check.migration_uses_transaction("CREATE TABLE example (id BIGINT);")
        )
        self.assertTrue(security_check.migration_uses_transaction(INDEX))
        self.assertFalse(security_check.migration_uses_transaction(MARKER + INDEX))
        self.assertFalse(
            security_check.migration_uses_transaction(
                MARKER
                + "-- This is an ordinary explanatory comment.\n"
                + "CREATE UNIQUE INDEX CONCURRENTLY quoted ON chat_messages "
                + "((body->'replyTo'->>'messageId')) WHERE content <> ';--'; -- trailing\n"
            )
        )

    def test_markers_cannot_disguise_other_operations_or_add_transactions(self) -> None:
        """Malformed markers, multiple SQL commands and procedural syntax fail closed."""
        rejected = (
            "\n" + MARKER + INDEX,
            " " + MARKER + INDEX,
            "-- NO-TRANSACTION\n" + INDEX,
            "-- no-transaction ignored suffix\n" + INDEX,
            MARKER + "CREATE INDEX ordinary ON chat_messages (id);",
            MARKER + "CREATE TABLE unrelated (id BIGINT);",
            MARKER + INDEX + INDEX,
            MARKER + "BEGIN;\n" + INDEX + "COMMIT;",
            MARKER + INDEX + "DROP TABLE chat_messages;",
            MARKER + "SET lock_timeout = '1min';\n" + INDEX,
            MARKER + "/* comment */\n" + INDEX,
            MARKER + "DO $$ BEGIN NULL; END $$;",
            MARKER + INDEX.rstrip("\n;") + "\\gexec",
            MARKER,
        )
        for sql in rejected:
            with self.subTest(sql=sql), self.assertRaises(ToolError):
                _ = security_check.migration_uses_transaction(sql)

    def test_every_migration_is_linted_in_its_own_execution_mode(self) -> None:
        """No migration is skipped and the no-transaction group gets no rule exclusions."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            maintenance_fixture(root)
            (root / "migrations").mkdir()
            output = root / "output"
            output.mkdir()
            names = ["migrations/001_regular.sql", "migrations/002_index.sql"]
            _ = (root / names[0]).write_text("CREATE TABLE example (id BIGINT);\n")
            _ = (root / names[1]).write_text(MARKER + INDEX)
            context = Context(root, output)
            with (
                patch.object(Context, "run", return_value=(0, b"[]")) as run,
                patch.object(security_check, "tool_path", return_value=root / "squawk"),
            ):
                security_check.migration_checks(context, root, names)
            self.assertEqual(run.call_count, 2)
            for call, transactional, name in zip(
                run.call_args_list, (True, False), names, strict=True
            ):
                args = cast("list[str]", call.args[1])
                self.assertEqual("--assume-in-transaction" in args, transactional)
                expected = (
                    root / name if transactional else output / "migration-context" / Path(name).name
                )
                self.assertEqual(args[-1], str(expected))
                if not transactional:
                    self.assertEqual(
                        expected.read_bytes(),
                        b"SET statement_timeout = '10min';\nSET lock_timeout = '1min';\n"
                        + (root / name).read_bytes(),
                    )
                configuration = tomllib.loads(Path(args[args.index("--config") + 1]).read_text())
                self.assertEqual(configuration["assume_in_transaction"], transactional)
                for key in ("excluded_rules", "included_rules", "excluded_paths"):
                    self.assertEqual(configuration[key], [])

    def test_nontransactional_marker_does_not_authorize_inline_suppression(self) -> None:
        """The existing suppression prohibition applies before execution-mode selection."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            maintenance_fixture(root)
            (root / "migrations").mkdir()
            (root / "security").mkdir()
            _ = (root / "security/migration-baseline.json").write_text(
                json.dumps({"schemaVersion": 1, "reviewedRevision": "a" * 40, "files": []})
            )
            for suppression in ("squawk-ignore-file", "squawk-disable-assume-in-transaction"):
                _ = (root / "migrations/001_index.sql").write_text(
                    MARKER + "-- " + suppression + "\n" + INDEX
                )
                with (
                    self.subTest(suppression=suppression),
                    self.assertRaisesRegex(ToolError, "migration_inline_suppression_forbidden"),
                ):
                    _ = security_findings.new_migrations(root)

    def test_scanner_findings_remain_blocking_for_concurrent_indexes(self) -> None:
        """A successful process cannot hide a nonempty scanner report."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            maintenance_fixture(root)
            output = root / "output"
            output.mkdir()
            _ = (root / "index.sql").write_text(MARKER + INDEX)
            with (
                patch.object(Context, "run", return_value=(0, b'[{"finding":"retained"}]')),
                patch.object(security_check, "tool_path", return_value=root / "squawk"),
                self.assertRaisesRegex(ToolError, "migration_policy_findings"),
            ):
                security_check.migration_checks(Context(root, output), root, ["index.sql"])

    def test_timeout_projection_tracks_literal_startup_values_and_refuses_drift(self) -> None:
        """Changing actual startup budgets changes lint context; removing them is a failure."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            maintenance_fixture(root)
            source = root / "src/db.rs"
            _ = source.write_text(DATABASE_SOURCE.replace('"10min"', '"15min"'))
            self.assertIn(
                b"SET statement_timeout = '15min';", security_check.migration_timeout_context(root)
            )
            for invalid in (
                DATABASE_SOURCE.replace('("lock_timeout", "1min")', ""),
                DATABASE_SOURCE.replace('"1min"', "configured_timeout"),
                DATABASE_SOURCE.replace("connect_with(&maintenance)", "connect_with(&runtime)"),
                DATABASE_SOURCE.replace('"1min")', '"1min"), ("lock_timeout", custom)'),
            ):
                _ = source.write_text(invalid)
                with self.subTest(source=invalid), self.assertRaises(ToolError):
                    _ = security_check.migration_timeout_context(root)
