"""Prove Rust include expansion cannot outlive the exact reviewed source bytes."""

from __future__ import annotations

import copy
import hashlib
import tempfile
import unittest
from pathlib import Path
from typing import TYPE_CHECKING
from unittest.mock import patch

from test_support import ROOT

# isort: split
import bounded_process
import security_codeql_rust_sources as rust
import security_codeql_triage as triage
from release_json import array_value, decode_json, object_value
from security_tools import ToolError
from test_security_codeql_triage import (
    REFERENCE,
    REVISION,
    alert,
    physical_record,
    review,
    sarif,
)

if TYPE_CHECKING:
    from collections.abc import Sequence


def write_sources(root: Path, sources: dict[str, bytes]) -> None:
    """Create only inert source text in a private test fixture."""
    for name, data in sources.items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        _ = path.write_bytes(data)


class RustSourcesTests(unittest.TestCase):
    """A review must bind every transitive literal include, not its parent alone."""

    def __init__(self, methodName: str = "runTest") -> None:  # noqa: N803 -- unittest API.
        """Represent committed bytes without creating a repository or running Rust."""
        super().__init__(methodName)
        self.sources: dict[str, bytes] = {
            "bin/main.rs": b'mod account { include!("../clients/account.rs"); }',
            "clients/account.rs": b'include!("guard.rs"); fn refresh() {}',
            "clients/guard.rs": b"fn loopback_only() {}",
        }

    def git_show(self, argv: Sequence[str], **_kwargs: object) -> tuple[int, bytes, bytes]:
        """Emulate git show using an independently held committed source snapshot."""
        self.assertEqual(list(argv[:2]), ["git", "show"])
        revision, name = argv[2].split(":", 1)
        self.assertEqual(revision, REVISION)
        return (0, self.sources[name], b"") if name in self.sources else (1, b"", b"")

    def test_transitive_closure_is_sorted_and_revision_checked(self) -> None:
        """The nested guard is part of the identity even though CodeQL points at main."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            write_sources(root, self.sources)
            with patch.object(bounded_process, "run", side_effect=self.git_show):
                actual = rust.included_sources(root, REVISION, "bin/main.rs")
        self.assertEqual(
            actual,
            {
                name: hashlib.sha256(self.sources[name]).hexdigest()
                for name in ("clients/account.rs", "clients/guard.rs")
            },
        )

    def test_module_only_committed_change_invalidates_review_and_api_matches(self) -> None:
        """Both normal gate paths lose approval when only an included guard changes."""
        api = alert()
        object_value(object_value(api["most_recent_instance"])["location"])["path"] = "bin/main.rs"
        raw = sarif()
        object_value(physical_record(raw)["artifactLocation"])["uri"] = "bin/main.rs"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            write_sources(root, self.sources)
            with patch.object(bounded_process, "run", side_effect=self.git_show):
                original = triage.sarif_findings(raw, root, REVISION)[0]
                api_original = triage.api_finding(api, root, REVISION, REFERENCE)
                self.assertEqual(api_original["fingerprint"], original["fingerprint"])
                self.assertTrue(triage.verdict([original], [review(original)])["passed"])
                self.sources["clients/guard.rs"] = b"fn external_origins_allowed() {}"
                write_sources(root, self.sources)
                changed = triage.sarif_findings(raw, root, REVISION)[0]
                api_changed = triage.api_finding(api, root, REVISION, REFERENCE)
            self.assertEqual(original["sourceSha256"], changed["sourceSha256"])
            self.assertNotEqual(original["fingerprint"], changed["fingerprint"])
            self.assertEqual(api_changed["fingerprint"], changed["fingerprint"])
            self.assertFalse(triage.verdict([changed], [review(original)])["passed"])

    def test_uncommitted_module_edit_and_symlink_are_rejected(self) -> None:
        """An uncommitted edit or linked include cannot supply review identity bytes."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            write_sources(root, self.sources)
            guard = root / "clients/guard.rs"
            with patch.object(bounded_process, "run", side_effect=self.git_show):
                _ = guard.write_bytes(b"edited")
                with self.assertRaisesRegex(ToolError, "codeql_source_revision_mismatch"):
                    _ = rust.included_sources(root, REVISION, "bin/main.rs")
                guard.unlink()
                guard.symlink_to(root / "clients/account.rs")
                with self.assertRaisesRegex(ToolError, "codeql_source_symlink"):
                    _ = rust.included_sources(root, REVISION, "bin/main.rs")

    def test_comments_strings_and_characters_do_not_create_includes(self) -> None:
        """Rust literal/comment contents cannot hide or invent a macro invocation."""
        text = """
            /* outer /* include!("bad0"); */ include!("bad1"); */
            // include!("bad2");
            let s = "include!(\\"bad3\\"); /*";
            let raw = r###"include!("bad4"); /*"###;
            let bytes = br#"include!("bad5"); /*"#;
            let ch = '"'; let escaped = '\\''; let byte = b'"';
            include /* gap */ ! ( /* gap */ "../clients/account.rs", /* gap */ );
            r#include!("other.rs");
        """
        self.assertEqual(rust.literal_includes(text), ["../clients/account.rs", "other.rs"])

    def test_normalization_cannot_hide_a_symlink_before_parent_component(self) -> None:
        """An include through link/.. must not bind a different normalized local file."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            write_sources(root, self.sources)
            (root / "clients/link").symlink_to(root / "bin", target_is_directory=True)
            with self.assertRaisesRegex(ToolError, "codeql_source_symlink"):
                _ = rust.include_path(root, "clients/account.rs", "link/../guard.rs")

    def test_unsupported_macro_forms_fail_closed(self) -> None:
        """Dynamic, escaped and non-parenthesized expansion is never silently omitted."""
        for text in (
            'include!(concat!(env!("OUT_DIR"), "/generated.rs"));',
            'include!(r#"guard.rs"#);',
            'include!("guard\\x2ers");',
            'include!["guard.rs"];',
            'include!("/tmp/guard.rs");',
        ):
            with self.subTest(text=text), self.assertRaises(ToolError):
                _ = rust.literal_includes(text)

    def test_escape_cycle_and_resource_bounds_fail_closed(self) -> None:
        """The closure has no path escape, cycle or unbounded traversal fallback."""
        scenarios: list[tuple[str, dict[str, bytes], str | None, int | None]] = [
            (
                "codeql_rust_include_cycle",
                {"clients/guard.rs": b'include!("account.rs");'},
                None,
                None,
            ),
            (
                "codeql_rust_include_escape",
                {"clients/guard.rs": b'include!("../../escape.rs");'},
                None,
                None,
            ),
            ("codeql_rust_include_limit", {}, "MAX_FILES", 2),
            ("codeql_rust_include_depth", {}, "MAX_DEPTH", 1),
            ("codeql_rust_include_bytes", {}, "MAX_BYTES", 1),
            ("codeql_rust_include_edges", {}, "MAX_EDGES", 1),
        ]
        for reason, changes, limit, value in scenarios:
            with self.subTest(reason=reason), tempfile.TemporaryDirectory() as temporary:
                original = copy.deepcopy(self.sources)
                self.sources.update(changes)
                root = Path(temporary).resolve()
                write_sources(root, self.sources)
                with (
                    patch.object(bounded_process, "run", side_effect=self.git_show),
                    patch.object(rust, limit or "MAX_FILES", value if limit else rust.MAX_FILES),
                    self.assertRaisesRegex(ToolError, reason),
                ):
                    _ = rust.included_sources(root, REVISION, "bin/main.rs")
                self.sources = original

    def test_existing_reviewed_rust_files_have_no_include_identity_churn(self) -> None:
        """The previous exact Rust reviews remain valid under the additional binding."""
        paths: set[str] = set()
        for path in triage.REVIEW_PATHS:
            raw = object_value(decode_json(path.read_bytes()))
            paths.update(
                str(object_value(item)["path"])
                for item in array_value(raw["alerts"])
                if str(object_value(item)["path"]).endswith(".rs")
                and not str(object_value(item)["path"]).startswith("vendor/")
                and "includedSources" not in object_value(item)
            )
        self.assertTrue(paths)
        for name in paths:
            with self.subTest(path=name):
                self.assertEqual(rust.literal_includes((ROOT / name).read_text()), [])
