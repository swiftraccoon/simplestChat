"""Exercise scanner completeness, golden coverage and private result boundaries."""

from __future__ import annotations

import contextlib
import io
import json
import os
import shutil
import tempfile
import unittest
from pathlib import Path
from typing import cast, override
from unittest.mock import patch

from test_support import ROOT

# isort: split
import security_semgrep as scanner
from release_json import JsonObject, JsonValue, array_value, decode_json, json_value, object_value
from security_context import Context
from security_tools import ToolError, tool_path

RULE = "simplestchat.example"
VERSION = "1.179.0"


def put(root: Path, name: str, content: str) -> Path:
    """Create inert local fixture bytes; none of this source is evaluated."""
    path = root / name
    path.parent.mkdir(parents=True, exist_ok=True)
    _ = path.write_text(content)
    return path


def result(path: str = "src/example.py", rule: str = RULE) -> JsonObject:
    """Model a real core result with private content that must never reach summaries."""
    return {
        "check_id": rule,
        "path": path,
        "start": {"line": 4},
        "extra": {
            "is_ignored": False,
            "metavars": {"$VALUE": "private-source-sentinel"},
            "message": "private-source-sentinel",
        },
    }


def report() -> JsonObject:
    """Provide the complete required engine envelope, including explicit coverage."""
    return {
        "version": VERSION,
        "results": [result()],
        "errors": [],
        "skipped_rules": [],
        "paths": {"scanned": ["src/example.py"]},
        "time": {"fixpoint_timeouts": []},
        "rules_by_engine": [[RULE, "OSS"]],
    }


def parse(value: JsonObject) -> set[scanner.Finding]:
    """Use the exact same validation path as an engine invocation."""
    return scanner.findings(json.dumps(value).encode(), ["src/example.py"], {RULE}, VERSION)


class FindingTests(unittest.TestCase):
    """A zero exit status does not excuse scanner errors or incomplete coverage."""

    def test_coordinates_exclude_private_match_data(self) -> None:
        """Only reviewed identifiers, an inventoried path and its line are public."""
        actual = parse(report())
        self.assertEqual(actual, {scanner.Finding(RULE, "src/example.py", 4)})
        public = json.dumps([item.json() for item in actual])
        self.assertNotIn("private-source-sentinel", public)

    def test_errors_skipped_rules_and_fixpoint_timeouts_fail(self) -> None:
        """Partial parsing or taint evaluation is a failed scan, never a clean result."""
        for key, code in (("errors", "engine_errors"), ("skipped_rules", "skipped_rules")):
            with self.subTest(key=key):
                value = report()
                value[key] = ["private-source-sentinel"]
                with self.assertRaisesRegex(ToolError, code):
                    _ = parse(value)
        value = report()
        value["time"] = {"fixpoint_timeouts": ["src/example.py"]}
        with self.assertRaisesRegex(ToolError, "fixpoint_timeout"):
            _ = parse(value)

    def test_wrong_version_or_omitted_rules_fail(self) -> None:
        """A different binary response or dropped rule cannot satisfy the locked scan."""
        for key, content in (
            ("version", "0.0.0"),
            ("rules_by_engine", []),
            ("rules_by_engine", [[RULE, "PRO"]]),
            ("rules_by_engine", [[RULE, "OSS"], [RULE, "OSS"]]),
        ):
            with self.subTest(key=key, content=content):
                value = report()
                value[key] = json_value(content)
                with self.assertRaises(ToolError):
                    _ = parse(value)

    def test_missing_duplicate_or_foreign_target_fails(self) -> None:
        """Explicit target equality rejects silently skipped and unrelated files."""
        for paths in ([], ["elsewhere.py"], ["src/example.py", "src/example.py"]):
            with self.subTest(paths=paths):
                value = report()
                value["paths"] = {"scanned": json_value(paths)}
                with self.assertRaisesRegex(ToolError, "incomplete_scan"):
                    _ = parse(value)

    def test_unknown_match_coordinates_or_inline_ignore_fail(self) -> None:
        """Untrusted engine strings cannot escape through the compact public report."""
        for entry in (result("../outside.py"), result(rule="private-source-sentinel")):
            value = report()
            value["results"] = [entry]
            with self.assertRaisesRegex(ToolError, "unknown_finding"):
                _ = parse(value)
        value = report()
        entry = result()
        entry["extra"] = {"is_ignored": True}
        value["results"] = [entry]
        with self.assertRaisesRegex(ToolError, "inline_ignore"):
            _ = parse(value)

    def test_invalid_line_and_ambiguous_json_fail(self) -> None:
        """Line numbers are positive bounded integers; duplicate JSON keys are invalid."""
        for line in (True, 0, -1, 4.5, "4", scanner.MAX_LINE + 1):
            with self.subTest(line=line):
                value = report()
                entry = result()
                entry["start"] = {"line": line}
                value["results"] = [entry]
                with self.assertRaisesRegex(ToolError, "finding_line"):
                    _ = parse(value)
        with self.assertRaises(ValueError):
            _ = scanner.findings(b'{"errors":[],"errors":[]}', [], set(), VERSION)

    def test_required_envelope_fields_cannot_disappear(self) -> None:
        """Missing completeness metadata is not interpreted as an empty error list."""
        for field in report():
            with self.subTest(field=field):
                value = report()
                del value[field]
                with self.assertRaises(KeyError):
                    _ = parse(value)


class FixtureTests(unittest.TestCase):
    """Golden annotations must cover every maintained rule and identify exact lines."""

    root: Path = Path()

    @override
    def setUp(self) -> None:
        """Give each test an isolated private source directory."""
        temporary = tempfile.TemporaryDirectory()
        self.root = Path(temporary.name)
        self.addCleanup(temporary.cleanup)

    def fixture(self, text: str) -> Path:
        """Write one Python example so coverage validation sees only reviewed bytes."""
        return put(self.root, "sample.py", text)

    def test_exact_annotation_lines(self) -> None:
        """Annotations match the following statement and distinguish safe examples."""
        _ = self.fixture(f"# ruleid: {RULE}\nunsafe()\n# ok: {RULE}\nsafe()\n")
        self.assertEqual(
            scanner.fixture_expectations(self.root, {RULE}),
            ({scanner.Finding(RULE, "sample.py", 2)}, {scanner.Finding(RULE, "sample.py", 4)}),
        )

    def test_every_rule_requires_positive_and_negative_coverage(self) -> None:
        """Adding a rule or deleting its counterexample cannot leave the pack untested."""
        for text in (f"# ruleid: {RULE}\nunsafe()\n", f"# ok: {RULE}\nsafe()\n", "safe()\n"):
            _ = self.fixture(text)
            with self.assertRaisesRegex(ToolError, "coverage"):
                _ = scanner.fixture_expectations(self.root, {RULE})

    def test_unknown_malformed_and_unattached_annotations_fail(self) -> None:
        """Typos, trailing directives and blank target lines cannot hide a regression."""
        for text in (
            "# ruleid: simplestchat.unknown\nunsafe()\n",
            f"# ruleid: {RULE},other\nunsafe()\n",
            f"# ruleid: {RULE}\n",
            f"# ruleid: {RULE}\n\nunsafe()\n",
            f"# ruleid: {RULE}\n# ok: {RULE}\nsafe()\n",
        ):
            with self.subTest(text=text):
                _ = self.fixture(text)
                with self.assertRaises(ToolError):
                    _ = scanner.fixture_expectations(self.root, {RULE})

    def test_rule_pack_identity_and_unsafe_features(self) -> None:
        """Only unique bounded IDs, supported languages and non-mutating rules are allowed."""
        base: JsonObject = {
            "id": RULE,
            "languages": ["python"],
            "severity": "ERROR",
            "pattern": "unsafe()",
        }
        path = self.root / "rules.json"
        scanner.dump(path, {"rules": [base]})
        self.assertEqual(scanner.rules(path), {RULE})
        cases: list[JsonValue] = [
            [],
            [base, base],
            [{**base, "id": "unknown.id"}],
            [{**base, "languages": ["generic"]}],
            [{**base, "pattern-where-python": "unsafe()"}],
            [{**base, "fix": "replacement"}],
        ]
        for entries in cases:
            _ = path.write_text(json.dumps({"rules": entries}))
            with self.assertRaises(ToolError):
                _ = scanner.rules(path)

    def test_targets_use_explicit_languages_and_root_paths(self) -> None:
        """Core sees the same exact path identity in discovery, matching and output."""
        value = scanner.targets(["src/db.rs", "web/src/main.ts"])
        entries = array_value(array_value(value)[1])
        target = object_value(array_value(entries[0])[1])
        self.assertEqual(target["path"], {"fpath": "src/db.rs", "ppath": "/src/db.rs"})
        self.assertEqual(target["analyzer"], "rust")
        for names in ([], ["../outside.py"], ["file.bin"], ["src/a.py", "src/a.py"]):
            with self.subTest(names=names), self.assertRaises(ToolError):
                _ = scanner.targets(names)

    def test_source_scope_is_explicit(self) -> None:
        """Vendor, tests, generated files and unknown languages have separate gates."""
        for name in (
            "src/main.rs",
            "build/check.py",
            "web/src/main.ts",
            "web/src/util.js",
            "ops/ansible/files/run.py",
            "vendor/untrusted.py",
            "security/fixture.py",
            "ops/ansible/tests/test_run.py",
            "web/dist/main.js",
            "src/native.cpp",
        ):
            _ = put(self.root, name, "fixture\n")
        self.assertEqual(
            scanner.source_paths(self.root),
            [
                "build/check.py",
                "ops/ansible/files/run.py",
                "src/main.rs",
                "web/src/main.ts",
                "web/src/util.js",
            ],
        )

    def test_existing_output_and_source_output_are_rejected(self) -> None:
        """A failed invocation cannot overwrite evidence or include its own output as source."""
        with self.assertRaisesRegex(ToolError, "output_exists"):
            _ = scanner.check(self.root, self.root)
        with self.assertRaisesRegex(ToolError, "output_in_source"):
            _ = scanner.check(self.root, self.root / "report")
        self.assertFalse((self.root / "report").exists())

    def test_ignored_results_and_target_outputs_are_accepted(self) -> None:
        """The fast dispatcher can write its fresh report under either artifact root."""
        for name in ("results", "target"):
            parent = self.root / name
            parent.mkdir()
            output = scanner.new_output(self.root.resolve(), parent / "security.semgrep")
            self.assertEqual(output, parent.resolve() / "security.semgrep")
            self.assertEqual(output.stat().st_mode & 0o777, 0o700)
        for name in ("results-source", "targets", "src"):
            parent = self.root / name
            parent.mkdir()
            with self.assertRaisesRegex(ToolError, "output_in_source"):
                _ = scanner.new_output(self.root.resolve(), parent / "report")
            self.assertFalse((parent / "report").exists())

    def test_engine_invocation_has_resource_limits_and_private_targets(self) -> None:
        """The wrapper uses explicit targets and bounded process and engine resources."""
        rule_path = self.root / "rules.json"
        scanner.dump(
            rule_path,
            {
                "rules": [
                    {
                        "id": RULE,
                        "languages": ["python"],
                        "severity": "ERROR",
                        "pattern": "unsafe()",
                    }
                ]
            },
        )
        context = Context(self.root, self.root)
        with patch.object(Context, "run", return_value=(0, json.dumps(report()).encode())) as run:
            actual = scanner.run_engine(
                context,
                Path("/reviewed/core"),
                rule_path,
                self.root,
                ["src/example.py"],
                name="fixture-engine",
            )
        self.assertEqual(actual, {scanner.Finding(RULE, "src/example.py", 4)})
        self.assertEqual(run.call_args.kwargs["timeout"], 180)
        argv = cast("list[str]", run.call_args.args[1])
        for flag, value in (("-j", "2"), ("-timeout", "5"), ("-max_memory", "2048")):
            self.assertEqual(argv[argv.index(flag) + 1], value)
        self.assertIn("-strict", argv)
        self.assertEqual((self.root / "fixture-engine-targets.json").stat().st_mode & 0o777, 0o600)

    def test_public_cli_redacts_os_and_parse_diagnostics(self) -> None:
        """Exception text may contain source, credentials or absolute local paths."""
        output = io.StringIO()
        with (
            patch.object(scanner, "check", side_effect=OSError("private-source-sentinel")),
            contextlib.redirect_stdout(output),
        ):
            status = scanner.main(["check", "--output", str(self.root / "output")])
        self.assertEqual(status, 1)
        self.assertNotIn("private-source-sentinel", output.getvalue())
        self.assertEqual(
            json.loads(output.getvalue()), {"passed": False, "error": "semgrep_check_failed"}
        )


@unittest.skipUnless(
    os.environ.get("SIMPLESTCHAT_SEMGREP_ENGINE_TESTS") == "1",
    "Set SIMPLESTCHAT_SEMGREP_ENGINE_TESTS=1 after installing the pinned standalone engine.",
)
class EngineTests(unittest.TestCase):
    """Opt-in installed-tool regressions; the check CLI always validates the golden pack."""

    source: Path = Path()
    engine: Path = Path()

    root: Path = Path()

    @override
    def setUp(self) -> None:
        """Copy only the maintained pack; no fixture code is imported or executed."""
        temporary = tempfile.TemporaryDirectory()
        self.root = Path(temporary.name)
        self.addCleanup(temporary.cleanup)
        self.source = self.root / "source"
        _ = shutil.copytree(ROOT / "security/semgrep", self.source / "security/semgrep")
        self.engine = tool_path("semgrep-core")

    def context(self) -> Context:
        """Give one engine invocation exclusive bounded private evidence paths."""
        output = self.root / "output"
        output.mkdir(mode=0o700)
        return Context(self.source, output)

    def test_real_engine_matches_every_positive_and_negative(self) -> None:
        """The actual pinned native engine must match the complete annotated fixture set."""
        assertions = scanner.golden(self.context(), self.engine, self.source)
        self.assertGreaterEqual(assertions, 40)

    def test_real_engine_rejects_rule_that_loses_positives(self) -> None:
        """A valid but ineffective replacement rule fails exact golden equality."""
        path = self.source / scanner.RULES
        value = object_value(decode_json(path.read_bytes()))
        rule = object_value(array_value(value["rules"])[0])
        del rule["patterns"]
        rule["pattern"] = "never_matches_fixture()"
        _ = path.write_text(json.dumps(value))
        with self.assertRaisesRegex(ToolError, "golden_mismatch"):
            _ = scanner.golden(self.context(), self.engine, self.source)

    def test_real_engine_rejects_false_positive(self) -> None:
        """A fixture changed to unsafe code cannot keep an ok annotation and pass."""
        path = self.source / scanner.FIXTURES / "security_examples.ts.fixture"
        _ = path.write_text(
            path.read_text().replace(
                'console.info("authentication rejected")', "console.info(password)"
            )
        )
        with self.assertRaisesRegex(ToolError, "golden_false_positive"):
            _ = scanner.golden(self.context(), self.engine, self.source)

    def test_real_engine_parse_failure_cannot_look_clean(self) -> None:
        """Malformed source produces an engine error even if no security match exists."""
        path = self.source / scanner.FIXTURES / "invalid.py.fixture"
        _ = path.write_text("def malformed(:\n    requests.get(\n")
        with self.assertRaises(ToolError):
            _ = scanner.golden(self.context(), self.engine, self.source)


if __name__ == "__main__":
    _ = unittest.main()
