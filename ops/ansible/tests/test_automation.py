"""Execute the shared release-state guard against filesystem doubles; nothing is contacted."""

import io
import json
import re
import stat
import unittest
from contextlib import redirect_stdout
from dataclasses import dataclass
from pathlib import Path
from unittest.mock import patch

from test_support import string

# isort: split
from release_json import JsonObject, boolean_value, integer_value

# This test executes the checked-in guard only with isolated filesystem
# doubles, never shell input or remote code.
# ruff: noqa: S102

ROOT = Path(__file__).resolve().parents[1]


def embedded_guard(script: str) -> str:
    """Require and extract the exact inline release-journal guard under test."""
    match = re.search(r"python3 - <<'PY'\n(.*?)\nPY", script, re.DOTALL)
    if match is None:
        message = "Missing release-journal guard"
        raise AssertionError(message)
    return match[1]


@dataclass(frozen=True, slots=True, kw_only=True)
class Metadata:
    """Filesystem fields used by the journal guard."""

    st_uid: int = 0
    st_mode: int = stat.S_IFREG | 0o600
    st_size: int = 100


@dataclass(frozen=True, slots=True, kw_only=True)
class Parent:
    """Minimal parent-directory metadata double."""

    metadata: Metadata

    def lstat(self) -> Metadata:
        """Return configured metadata without contacting the filesystem."""
        return self.metadata


@dataclass(frozen=True, slots=True, kw_only=True)
class JournalPath:
    """Typed journal fixture; no methods consult an actual journal path."""

    metadata: Metadata
    parent: Parent
    present: bool
    symlink: bool
    text: str

    def exists(self) -> bool:
        """Report the modeled file presence."""
        return self.present

    def is_symlink(self) -> bool:
        """Report whether this fixture models an unsafe symlink."""
        return self.symlink

    def lstat(self) -> Metadata:
        """Return modeled owner, mode and length."""
        return self.metadata

    def read_text(self) -> str:
        """Return the fixture journal, without reading production files."""
        return self.text


class AutomationTests(unittest.TestCase):
    """Verify the guard's decisions offline."""

    def test_persistent_release_guard_rejects_unfinished_or_unprotected_records(self) -> None:
        """Verify persistent release guard rejects unfinished or unprotected records."""
        guard = embedded_guard((ROOT / "files/deploy-public.sh").read_text())
        completed = {"schemaVersion": 1, "finalized": True}
        cases: list[tuple[JsonObject, bool]] = [
            ({}, True),
            ({"exists": False}, True),
            ({"value": dict(completed, finalized=False)}, False),
            ({"value": dict(completed, finalized=1)}, False),
            ({"value": dict(completed, finalized="true")}, False),
            ({"value": dict(completed, schemaVersion=True)}, False),
            ({"value": dict(completed, schemaVersion=2)}, False),
            ({"value": []}, False),
            ({"value": {}}, False),
            ({"text": "invalid JSON"}, False),
            ({"uid": 501}, False),
            ({"mode": stat.S_IFREG | 0o644}, False),
            ({"mode": stat.S_IFDIR | 0o600}, False),
            ({"mode": stat.S_IFIFO | 0o600}, False),
            ({"mode": stat.S_IFLNK | 0o600}, False),
            ({"exists": False, "symlink": True, "mode": stat.S_IFLNK | 0o600}, False),
            ({"size": 0}, False),
            ({"size": 16385}, False),
            ({"parent_uid": 501}, False),
            ({"parent_mode": stat.S_IFDIR | 0o755}, False),
            ({"parent_mode": stat.S_IFLNK | 0o700}, False),
        ]
        for changes, permitted in cases:
            with self.subTest(changes=changes):
                metadata = Metadata(
                    st_uid=integer_value(changes.get("uid", 0)),
                    st_mode=integer_value(changes.get("mode", stat.S_IFREG | 0o600)),
                    st_size=integer_value(changes.get("size", 100)),
                )
                parent = Parent(
                    metadata=Metadata(
                        st_uid=integer_value(changes.get("parent_uid", 0)),
                        st_mode=integer_value(changes.get("parent_mode", stat.S_IFDIR | 0o700)),
                    )
                )
                record = JournalPath(
                    present=boolean_value(changes.get("exists", True)),
                    symlink=boolean_value(changes.get("symlink", False)),
                    metadata=metadata,
                    parent=parent,
                    text=string(changes.get("text", json.dumps(changes.get("value", completed)))),
                )
                with (
                    patch("pathlib.Path", return_value=record) as factory,
                    redirect_stdout(io.StringIO()),
                ):
                    if permitted:
                        exec(compile(guard, "<release-guard>", "exec", optimize=2), {})
                    else:
                        with self.assertRaises((SystemExit, ValueError)):
                            exec(compile(guard, "<release-guard>", "exec", optimize=2), {})
                    factory.assert_called_once_with("/srv/simplestchat-public/release-state.json")


if __name__ == "__main__":
    _ = unittest.main()
