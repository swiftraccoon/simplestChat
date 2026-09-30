"""Require the pinned OpenSSL series to match its vendor's current stable patch.

Vulnerability databases can lag a vendor advisory even after downloading their
latest snapshot. This independent freshness gate consults the official current
release list. It never upgrades dependencies or treats freshness as proof that
the native dependency graph is free of vulnerabilities.
"""

from __future__ import annotations

import hashlib
import json
import re
import time
from datetime import UTC, datetime
from html.parser import HTMLParser
from typing import TYPE_CHECKING, override

from security_context import executable
from security_tools import bounded_file, require, write_private

# isort: split
import bounded_process
from release_json import decode_json, object_value, string_value

if TYPE_CHECKING:
    from pathlib import Path

    from security_context import Context

SOURCE_URL = "https://openssl-library.org/source/"
MAX_PAGE = 1024 * 1024
MAX_LINKS = 1000
VERSION = r"(?:0|[1-9][0-9]{0,3})\.(?:0|[1-9][0-9]{0,3})\.(?:0|[1-9][0-9]{0,3})"
ARCHIVE = re.compile(
    r"https://github\.com/openssl/openssl/releases/download/openssl-("
    + VERSION
    + r")/openssl-\1\.tar\.gz"
)


class Releases(HTMLParser):
    """Collect only exact stable archive links; prereleases are separate products."""

    def __init__(self) -> None:
        """Bound link bookkeeping independently of the response byte ceiling."""
        super().__init__(convert_charrefs=True)
        self.versions: list[str] = []
        self.links: int = 0

    @override
    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        """Ignore prose/scripts and accept only one href on each real anchor."""
        if tag != "a":
            return
        self.links += 1
        require(self.links <= MAX_LINKS, "openssl_release_link_count")
        links = [value for name, value in attrs if name == "href"]
        require(len(links) <= 1, "openssl_release_duplicate_href")
        if links and links[0] is not None:
            match = ARCHIVE.fullmatch(links[0])
            if match is not None:
                self.versions.append(match[1])


def latest_version(page: bytes, pinned: str) -> str:
    """Require one current stable archive for the pinned major/minor series.

    An absent or ambiguous series fails instead of silently selecting an old
    link, a prerelease, another release series or an unrecognized new layout.
    """
    require(re.fullmatch(VERSION, pinned) is not None, "openssl_pinned_version")
    require(0 < len(page) <= MAX_PAGE, "openssl_release_page_size")
    parser = Releases()
    parser.feed(page.decode("utf-8", errors="strict"))
    parser.close()
    series = pinned.rsplit(".", 1)[0]
    matches = [value for value in parser.versions if value.rsplit(".", 1)[0] == series]
    require(len(matches) == 1, "openssl_release_series_missing_or_ambiguous")
    return matches[0]


def fetch_page(context: Context) -> bytes:
    """Fetch one fixed HTTPS origin without redirects, ambient credentials or retries."""
    status, page, _ = bounded_process.run(
        [
            executable("curl"),
            "--disable",
            "--fail",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--tlsv1.2",
            "--connect-timeout",
            "10",
            "--max-time",
            "30",
            "--max-filesize",
            str(MAX_PAGE),
            "--header",
            "Cache-Control: no-cache",
            "--header",
            "Accept: text/html",
            "--write-out",
            "\n%{http_code}",
            SOURCE_URL,
        ],
        cwd=context.root,
        env=context.env,
        limits=bounded_process.Limits(timeout=35, stdout=MAX_PAGE + 4, stderr=16384),
    )
    require(status == 0, "openssl_release_fetch_failed")
    require(page.endswith(b"\n200"), "openssl_release_http_status")
    return page[:-4]


def check(context: Context, snapshot: Path) -> None:
    """Bind a successful freshness observation to the exact snapshotted native pin."""
    started = time.monotonic()
    result: dict[str, object] = {"name": "openssl-freshness", "exitStatus": 1}
    context.checks.append(result)
    try:
        manifest = bounded_file(snapshot / "vendor/native-components.json", MAX_PAGE)
        openssl = object_value(object_value(decode_json(manifest))["openssl"])
        pinned = string_value(openssl["version"])
        source = object_value(openssl["source"])
        match = ARCHIVE.fullmatch(string_value(source["url"]))
        require(match is not None and match[1] == pinned, "openssl_source_version_mismatch")
        page = fetch_page(context)
        current = latest_version(page, pinned)
        receipt: dict[str, object] = {
            "schemaVersion": 1,
            "source": SOURCE_URL,
            "observedAt": datetime.now(UTC).isoformat(),
            "pageSha256": hashlib.sha256(page).hexdigest(),
            "nativeManifestSha256": hashlib.sha256(manifest).hexdigest(),
            "pinnedVersion": pinned,
            "latestVersion": current,
            "passed": current == pinned,
        }
        write_private(
            context.output / "openssl-freshness.json",
            (json.dumps(receipt, indent=2, sort_keys=True) + "\n").encode(),
            0o600,
        )
        require(current == pinned, "openssl_patch_update_required")
        result.update(exitStatus=0, pinnedVersion=pinned, latestVersion=current)
    finally:
        result["elapsedSeconds"] = round(time.monotonic() - started, 3)
