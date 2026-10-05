"""Exercise vendor-release freshness independently of advisory database coverage."""

from __future__ import annotations

import hashlib
import json
import sys
import tempfile
import unittest
from pathlib import Path
from typing import cast
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "build"))

import security_openssl as openssl
from security_context import Context
from security_tools import ToolError

# isort: split
import bounded_process


def archive(version: str) -> str:
    """Return an inert official-format link for parser fixtures."""
    return (
        "https://github.com/openssl/openssl/releases/download/openssl-"
        + version
        + "/openssl-"
        + version
        + ".tar.gz"
    )


def page(*versions: str) -> bytes:
    """Construct a small current-release table without network access."""
    return ("<html>" + "".join(f'<a href="{archive(v)}">source</a>' for v in versions)).encode()


class OpenSSLTests(unittest.TestCase):
    """Missing vendor evidence and stale pins cannot become a passing source gate."""

    def test_latest_stable_includes_new_series_and_ignores_prereleases(self) -> None:
        """A stable major/minor release cannot be hidden by retaining an older series."""
        self.assertEqual(
            openssl.latest_version(page("4.1.0-beta1", "4.0.3", "3.6.5", "3.5.9"), "3.5.9"),
            "4.0.3",
        )
        self.assertEqual(openssl.latest_version(page("4.0.9", "4.1.0"), "4.0.9"), "4.1.0")
        self.assertEqual(openssl.latest_version(page("9.9.9", "10.0.0"), "9.9.9"), "10.0.0")

    def test_absent_ambiguous_and_duplicate_series_fail(self) -> None:
        """No fallback to prereleases, old tables or duplicate archives is allowed."""
        for versions in ((), ("4.0.3-beta1",), ("4.0.2", "4.0.3"), ("4.0.3",) * 2):
            with self.subTest(versions=versions), self.assertRaises(ToolError):
                _ = openssl.latest_version(page(*versions), "4.0.3")

    def test_prose_comments_scripts_and_noncanonical_urls_are_not_releases(self) -> None:
        """Only actual anchors to the exact official stable archive count."""
        valid = archive("4.0.3")
        samples = (
            valid,
            f'<!-- <a href="{valid}">source</a> -->',
            f"<script>const text = '<a href=\"{valid}\">source</a>';</script>",
            f'<a href="{valid}.sha256">hash</a>',
            f'<a href="{valid}?ref=latest">query</a>',
            f'<a href="{valid.replace("https://", "http://")}">http</a>',
            f'<a href="{valid.replace("github.com", "github.com.example.org")}">host</a>',
            f'<a href="{valid.replace("/openssl-4.0.3.tar", "/openssl-4.0.2.tar")}">mismatch</a>',
            f'<a href="{valid}" href="{valid}">duplicate attribute</a>',
        )
        for value in samples:
            with self.subTest(value=value), self.assertRaises(ToolError):
                _ = openssl.latest_version(value.encode(), "4.0.3")

    def test_versions_and_parser_resources_are_bounded(self) -> None:
        """Malformed pins, oversized input and excessive link counts fail explicitly."""
        for value in ("", "3.5", "04.0.3", "4.0.3-beta1", "4.0.3\n"):
            with self.subTest(value=value), self.assertRaises(ToolError):
                _ = openssl.latest_version(page("4.0.3"), value)
        for content in (b"", b"x" * (openssl.MAX_PAGE + 1), b"<a>" * (openssl.MAX_LINKS + 1)):
            with self.subTest(size=len(content)), self.assertRaises(ToolError):
                _ = openssl.latest_version(content, "4.0.3")
        with self.assertRaises(UnicodeDecodeError):
            _ = openssl.latest_version(b"\xff", "4.0.3")

    def fixture(self, directory: Path, version: str = "4.0.3") -> Context:
        """Create a source pin and private evidence directory for one invocation."""
        (directory / "vendor").mkdir()
        _ = (directory / "vendor/native-components.json").write_text(
            json.dumps({"openssl": {"version": version, "source": {"url": archive(version)}}})
        )
        output = directory / "output"
        output.mkdir()
        return Context(directory, output)

    def test_success_binds_manifest_and_page_without_retaining_html(self) -> None:
        """A compact receipt identifies exact inputs while successful checks remain visible."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            context = self.fixture(root)
            content = page("4.0.3")
            with patch.object(openssl, "fetch_page", return_value=content):
                openssl.check(context, root)
            receipt = (context.output / "openssl-freshness.json").read_bytes()
            self.assertIn(hashlib.sha256(content).hexdigest().encode(), receipt)
            self.assertIn(
                hashlib.sha256((root / "vendor/native-components.json").read_bytes())
                .hexdigest()
                .encode(),
                receipt,
            )
            self.assertNotIn(b"<html>", receipt)
            self.assertEqual(context.checks[-1]["exitStatus"], 0)

    def test_older_or_unpublished_pin_fails_with_receipt(self) -> None:
        """Neither a stale dependency nor an unauthenticated future pin is accepted."""
        for pinned in ("4.0.2", "4.0.4"):
            with self.subTest(pinned=pinned), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                context = self.fixture(root, pinned)
                with (
                    patch.object(openssl, "fetch_page", return_value=page("4.0.3")),
                    self.assertRaisesRegex(ToolError, "openssl_stable_update_required"),
                ):
                    openssl.check(context, root)
                self.assertIn(
                    b'"passed": false', (context.output / "openssl-freshness.json").read_bytes()
                )
                self.assertEqual(context.checks[-1]["exitStatus"], 1)

    def test_fetch_failure_and_source_mismatch_do_not_pass(self) -> None:
        """Transport failures propagate and inconsistent pins are rejected before networking."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            context = self.fixture(root)
            with (
                patch.object(openssl, "fetch_page", side_effect=ToolError("fixture_fetch_failure")),
                self.assertRaisesRegex(ToolError, "fixture_fetch_failure"),
            ):
                openssl.check(context, root)
            self.assertEqual(context.checks[-1]["exitStatus"], 1)
            path = root / "vendor/native-components.json"
            _ = path.write_text(path.read_text().replace(archive("4.0.3"), archive("4.0.2")))
            with (
                patch.object(openssl, "fetch_page") as fetch,
                self.assertRaisesRegex(ToolError, "openssl_source_version_mismatch"),
            ):
                openssl.check(context, root)
            fetch.assert_not_called()

    def test_fetch_has_hard_stream_deadline_and_fixed_origin(self) -> None:
        """The shared process boundary enforces limits before retaining HTTP output."""
        with tempfile.TemporaryDirectory() as temporary:
            context = self.fixture(Path(temporary))
            with (
                patch.object(openssl, "executable", return_value="/usr/bin/curl"),
                patch.object(
                    bounded_process, "run", return_value=(0, page("4.0.3") + b"\n200", b"")
                ) as run,
            ):
                self.assertEqual(openssl.fetch_page(context), page("4.0.3"))
            call = run.call_args
            self.assertIsNotNone(call)
            if call is None:
                self.fail("The fetch did not invoke its process boundary")
            arguments = cast("list[str]", call.args[0])
            limits = cast("bounded_process.Limits", call.kwargs["limits"])
            self.assertEqual(arguments[-1], openssl.SOURCE_URL)
            self.assertEqual(arguments[1], "--disable")
            self.assertNotIn("--location", arguments)
            self.assertEqual(limits.stdout, openssl.MAX_PAGE + 4)
            self.assertEqual(limits.stderr, 16384)
            self.assertEqual(limits.timeout, 35)
            self.assertEqual(call.kwargs["env"], context.env)
            with (
                patch.object(bounded_process, "run", return_value=(22, page("4.0.3"), b"")),
                self.assertRaisesRegex(ToolError, "openssl_release_fetch_failed"),
            ):
                _ = openssl.fetch_page(context)
            for suffix in (b"", b"\n301", b"\n403", b"\n204"):
                with (
                    self.subTest(suffix=suffix),
                    patch.object(
                        bounded_process,
                        "run",
                        return_value=(0, page("4.0.3") + suffix, b""),
                    ),
                    self.assertRaisesRegex(ToolError, "openssl_release_http_status"),
                ):
                    _ = openssl.fetch_page(context)


if __name__ == "__main__":
    _ = unittest.main()
