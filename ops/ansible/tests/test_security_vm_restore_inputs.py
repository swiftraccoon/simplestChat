"""Real private tar/FD fixture checks; no database, engine, network or VM."""

from __future__ import annotations

import copy
import hashlib
import io
import json
import os
import tarfile
import tempfile
import unittest
from pathlib import Path
from typing import TYPE_CHECKING, cast
from unittest.mock import patch

from test_support import ROOT

# isort: split
import security_vm_restore_inputs as inputs

if TYPE_CHECKING:
    from collections.abc import Callable


def check() -> None:
    """Provide a healthy explicit owner callback for pure bounded fixture operations."""


def members() -> dict[str, bytes]:
    """Every fixed source role is present, without private production data."""
    return {name: ("inert source " + name + "\n").encode() for name in inputs.MEMBERS}


def stored(root: Path, data: bytes) -> tuple[Path, Path]:
    """Create private initial files with fixed0600 before any selected final fixture mode."""
    archive = root / "input.tar"
    descriptor = os.open(archive, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "wb") as stream:
        size = stream.write(data)
        if size != len(data):
            message = "short_fixture_write"
            raise OSError(message)
        stream.flush()
    destination = root / "input"
    destination.mkdir(mode=0o700)
    (destination / "migrations").mkdir(mode=0o700)
    return archive, destination


def rewritten(data: bytes, change: Callable[[list[tuple[tarfile.TarInfo, bytes]]], None]) -> bytes:
    """Make a finite malformed owned copy; no extraction path is ever executed here."""
    rows: list[tuple[tarfile.TarInfo, bytes]] = []
    with io.BytesIO(data) as source, tarfile.open(fileobj=source, mode="r:") as archive:
        for row in archive.getmembers():
            stream = archive.extractfile(row)
            if stream is None:
                message = "fixture_member_missing"
                raise ValueError(message)
            with stream:
                rows.append((copy.copy(row), stream.read()))
    change(rows)
    with io.BytesIO() as output:
        with tarfile.open(fileobj=output, mode="w", format=tarfile.USTAR_FORMAT) as archive:
            for row, content in rows:
                row.size = len(content)
                with io.BytesIO(content) as source:
                    archive.addfile(row, source)
        return output.getvalue()


class FixtureArchiveTests(unittest.TestCase):
    """Validate bytes and every rejection before a destination file can be published."""

    def test_real_source_closure_roundtrips_exact_bytes_modes_and_names(self) -> None:
        """Match all real source bytes and retain the fixed private output profile."""
        values = members()
        for name in inputs.MIGRATIONS:
            values[name] = (ROOT / name).read_bytes()
        values["monitoring-schema.sql"] = (
            ROOT / "ops/ansible/files/monitoring-schema.sql"
        ).read_bytes()
        values["public-runtime-grants.sql.j2"] = (
            ROOT / "ops/ansible/templates/public-runtime-grants.sql.j2"
        ).read_bytes()
        data = inputs.pack(values, check)
        self.assertEqual(inputs.decode(data, check), values)
        with tempfile.TemporaryDirectory() as temporary:
            archive, destination = stored(Path(temporary), data)
            inputs.unpack(archive, hashlib.sha256(data).hexdigest(), destination, check)
            for name, expected in values.items():
                path = destination / name
                self.assertEqual(path.read_bytes(), expected)
                self.assertEqual(path.stat().st_mode & 0o7777, 0o600)
                self.assertEqual(path.stat().st_nlink, 1)
            self.assertFalse((destination / inputs.MANIFEST).exists())
            with self.assertRaises(inputs.InputError):
                inputs.unpack(archive, hashlib.sha256(data).hexdigest(), destination, check)

    def rejection_has_no_writes(self, data: bytes, *, digest: str | None = None) -> None:
        """All malformed-header/hash cases retain the original empty namespace."""
        with tempfile.TemporaryDirectory() as temporary:
            archive, destination = stored(Path(temporary), data)
            with self.assertRaises((ValueError, tarfile.TarError)):
                inputs.unpack(
                    archive, digest or hashlib.sha256(data).hexdigest(), destination, check
                )
            self.assertEqual({p.name for p in destination.iterdir()}, {"migrations"})
            self.assertEqual(list((destination / "migrations").iterdir()), [])

    def test_bad_hash_missing_duplicate_extra_and_traversal_never_write(self) -> None:
        """Reject each invalid closure before creating any target leaf."""
        data = inputs.pack(members(), check)
        self.rejection_has_no_writes(data, digest="0" * 64)

        def missing(rows: list[tuple[tarfile.TarInfo, bytes]]) -> None:
            _ = rows.pop()

        changes: list[Callable[[list[tuple[tarfile.TarInfo, bytes]]], None]] = [
            missing,
            lambda rows: rows.append(rows[-1]),
        ]
        for name in ("../outside", "/absolute", "migrations/../../outside", "unknown.sql"):

            def rename(rows: list[tuple[tarfile.TarInfo, bytes]], name: str = name) -> None:
                rows[-1][0].name = name

            changes.append(rename)
        for change in changes:
            self.rejection_has_no_writes(rewritten(data, change))

    def test_symlink_hardlink_and_directory_headers_never_write(self) -> None:
        """Reject nonregular data headers without following their link names."""
        data = inputs.pack(members(), check)
        for kind in (tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.DIRTYPE):

            def nonregular(rows: list[tuple[tarfile.TarInfo, bytes]], kind: bytes = kind) -> None:
                rows[-1][0].type = kind
                rows[-1][0].linkname = "../../outside"

            self.rejection_has_no_writes(rewritten(data, nonregular))

    def test_manifest_duplicate_nonfinite_pin_and_type_errors_never_write(self) -> None:
        """Require the complete strict manifest and exact content pins."""
        data = inputs.pack(members(), check)
        for value in (
            b'{"schemaVersion":1,"schemaVersion":1,"members":{}}',
            b'{"schemaVersion":NaN,"members":{}}',
            b'{"schemaVersion":true,"members":{}}',
        ):

            def manifest(rows: list[tuple[tarfile.TarInfo, bytes]], value: bytes = value) -> None:
                rows[0] = rows[0][0], value

            self.rejection_has_no_writes(rewritten(data, manifest))

        def pin(rows: list[tuple[tarfile.TarInfo, bytes]]) -> None:
            metadata = cast("dict[str, object]", json.loads(rows[0][1]))
            pins = cast("dict[str, object]", metadata["members"])
            pin = cast("dict[str, object]", pins[inputs.MEMBERS[-1]])
            pin["sha256"] = "0" * 64
            rows[0] = rows[0][0], json.dumps(metadata).encode()

        self.rejection_has_no_writes(rewritten(data, pin))

    def test_source_alias_mode_and_nonempty_namespace_are_refused(self) -> None:
        """Refuse an unsafe source original instead of broadening its custody profile."""
        data = inputs.pack(members(), check)
        with tempfile.TemporaryDirectory() as temporary:
            archive, destination = stored(Path(temporary), data)
            archive.chmod(0o644)
            with self.assertRaises(inputs.InputError):
                inputs.unpack(archive, hashlib.sha256(data).hexdigest(), destination, check)
            archive.chmod(0o600)
            alias = archive.with_suffix(".link")
            alias.symlink_to(archive)
            with self.assertRaises(OSError):
                inputs.unpack(alias, hashlib.sha256(data).hexdigest(), destination, check)
            os.link(archive, archive.with_suffix(".hard"))
            with self.assertRaises(inputs.InputError):
                inputs.unpack(archive, hashlib.sha256(data).hexdigest(), destination, check)

    def test_actual_fifo_refuses_without_waiting_for_a_writer_and_closes(self) -> None:
        """The pre-type archive open cannot block on a substituted nonregular original."""
        opening = os.open
        descriptors: list[int] = []

        def opened(path: Path, flags: int) -> int:
            self.assertTrue(flags & os.O_NONBLOCK)
            fd = opening(path, flags)
            descriptors.append(fd)
            return fd

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fifo = root / "fifo"
            os.mkfifo(fifo, 0o600)
            with patch.object(os, "open", side_effect=opened), self.assertRaises(inputs.InputError):
                inputs.unpack(fifo, "0" * 64, root / "never-admitted", check)
            self.assertEqual(len(descriptors), 1)
            with self.assertRaises(OSError):
                _ = os.fstat(descriptors[0])

    def test_replaced_parent_never_writes_the_foreign_migrations_child(self) -> None:
        """Child admission stays anchored to the original parent and refuses a replaced path."""
        data = inputs.pack(members(), check)
        real_directory = inputs.directory
        with tempfile.TemporaryDirectory() as temporary:
            archive, destination = stored(Path(temporary), data)
            held_path = destination.with_name("held-original")

            def directory(
                path: Path,
                live: Callable[[], None],
                owners: list[int],
                *,
                dir_fd: int | None = None,
            ) -> tuple[int, tuple[int, ...]]:
                result = real_directory(path, live, owners, dir_fd=dir_fd)
                if path == destination:
                    _ = destination.rename(held_path)
                    destination.mkdir(mode=0o700)
                    (destination / "migrations").mkdir(mode=0o700)
                return result

            with (
                patch.object(inputs, "directory", side_effect=directory),
                self.assertRaises(inputs.InputError),
            ):
                inputs.unpack(archive, hashlib.sha256(data).hexdigest(), destination, check)
            for path in (destination, held_path):
                self.assertEqual({child.name for child in path.iterdir()}, {"migrations"})
                self.assertEqual(list((path / "migrations").iterdir()), [])

    def test_partial_and_interrupted_writes_retry_and_preserve_exact_contents(self) -> None:
        """Retry actual short and interrupted writes under the same live check."""
        data = inputs.pack(members(), check)
        write = os.write
        calls = 0

        def short(fd: int, value: bytes) -> int:
            nonlocal calls
            calls += 1
            if calls == 1:
                raise InterruptedError
            return write(fd, value[:3])

        with tempfile.TemporaryDirectory() as temporary:
            archive, destination = stored(Path(temporary), data)
            with patch.object(os, "write", side_effect=short):
                inputs.unpack(archive, hashlib.sha256(data).hexdigest(), destination, check)
            self.assertGreater(calls, len(inputs.MEMBERS))
            self.assertEqual(
                {name: (destination / name).read_bytes() for name in inputs.MEMBERS}, members()
            )

    def closed_failure(self, *, late: bool) -> tuple[BaseException | None, bool]:
        """Actual admitted FDs close before assertions; a later close cannot replace body."""
        data = inputs.pack(members(), check)
        opening, closing, metadata = os.open, os.close, os.fstat
        owned: list[int] = []
        primary = (
            OSError("fixed_late_admission") if late else inputs.InputError("fixed_hash_refusal")
        )
        failed: BaseException | None = None

        def opened(
            path: str | Path, flags: int, mode: int = 0o600, *, dir_fd: int | None = None
        ) -> int:
            fd = opening(path, flags, mode, dir_fd=dir_fd)
            owned.append(fd)
            return fd

        def live() -> None:
            if late and owned:
                raise primary

        def close(fd: int) -> None:
            closing(fd)
            message = "secondary_checked_close"
            raise OSError(message)

        with tempfile.TemporaryDirectory() as temporary:
            archive, destination = stored(Path(temporary), data)
            try:
                with (
                    patch.object(os, "open", side_effect=opened),
                    patch.object(os, "close", side_effect=close),
                ):
                    inputs.unpack(
                        archive,
                        hashlib.sha256(data).hexdigest() if late else "0" * 64,
                        destination,
                        live,
                    )
            except (OSError, ValueError) as error:
                failed = error
            closed = True
            for fd in owned:
                try:
                    _ = metadata(fd)
                    closed = False
                except OSError:
                    pass
            self.assertEqual({p.name for p in destination.iterdir()}, {"migrations"})
        return failed, closed

    def test_late_original_admission_and_hash_refusal_preserve_first_and_close(self) -> None:
        """Close every admitted original and preserve the earlier refusal."""
        for late in (False, True):
            failure, closed = self.closed_failure(late=late)
            self.assertTrue(closed)
            self.assertIsInstance(failure, OSError if late else inputs.InputError)
            self.assertNotIn("secondary_checked_close", str(failure))

    def test_fsync_failure_before_peer_close_keeps_real_error_and_all_fds_closed(self) -> None:
        """Keep a real sync failure when every checked close also fails."""
        data = inputs.pack(members(), check)
        opened, closed, metadata = os.open, os.close, os.fstat
        descriptors: list[int] = []
        primary = OSError("actual_sync_refusal")

        def opening(
            path: str | Path, flags: int, mode: int = 0o600, *, dir_fd: int | None = None
        ) -> int:
            fd = opened(path, flags, mode, dir_fd=dir_fd)
            descriptors.append(fd)
            return fd

        def syncing(_fd: int) -> None:
            raise primary

        def closing(fd: int) -> None:
            closed(fd)
            message = "secondary_close"
            raise OSError(message)

        with tempfile.TemporaryDirectory() as temporary:
            archive, destination = stored(Path(temporary), data)
            with (
                patch.object(os, "open", side_effect=opening),
                patch.object(os, "fsync", side_effect=syncing),
                patch.object(os, "close", side_effect=closing),
                self.assertRaises(OSError) as got,
            ):
                inputs.unpack(archive, hashlib.sha256(data).hexdigest(), destination, check)
            flags: list[bool] = []
            for fd in descriptors:
                try:
                    _ = metadata(fd)
                    flags.append(False)
                except OSError:
                    flags.append(True)
            self.assertIs(got.exception, primary)
            self.assertTrue(flags and all(flags))

    def mutation_case(self, *, after: bool) -> None:
        """Change a real equal-sized original at one fixed completion boundary."""
        data = inputs.pack(members(), check)
        real_seal = inputs.seal_output
        descriptors: list[int] = []

        def seal(
            fd: int,
            value: bytes,
            identity: tuple[int, ...],
            live: Callable[[], None],
        ) -> tuple[int, ...]:
            descriptors.append(fd)
            if not after:
                _ = os.pwrite(fd, b"X" * len(value), 0)
            completed = real_seal(fd, value, identity, live)
            if after:
                _ = os.pwrite(fd, b"X" * len(value), 0)
            return completed

        with tempfile.TemporaryDirectory() as temporary:
            archive, destination = stored(Path(temporary), data)
            with (
                patch.object(inputs, "seal_output", side_effect=seal),
                self.assertRaises(inputs.InputError),
            ):
                inputs.unpack(archive, hashlib.sha256(data).hexdigest(), destination, check)
            closed: list[bool] = []
            for fd in descriptors:
                try:
                    _ = os.fstat(fd)
                    closed.append(False)
                except OSError:
                    closed.append(True)
            self.assertTrue(closed and all(closed))

    def test_equal_size_original_mutation_before_or_after_seal_is_refused(self) -> None:
        """Neither readback nor final full-stamp validation adopts changed output bytes."""
        self.mutation_case(after=False)
        self.mutation_case(after=True)

    def test_actual_last_close_crossing_the_original_deadline_cannot_return_success(self) -> None:
        """All originals close first, then the same final live fence refuses a late result."""
        data = inputs.pack(members(), check)
        real_close = os.close
        expired = False
        descriptors: list[int] = []
        primary = TimeoutError("original_fixture_deadline")

        def live() -> None:
            if expired:
                raise primary

        def closing(fd: int) -> None:
            nonlocal expired
            real_close(fd)
            descriptors.append(fd)
            expired = True

        with tempfile.TemporaryDirectory() as temporary:
            archive, destination = stored(Path(temporary), data)
            with (
                patch.object(os, "close", side_effect=closing),
                self.assertRaises(TimeoutError) as got,
            ):
                inputs.unpack(archive, hashlib.sha256(data).hexdigest(), destination, live)
            for fd in descriptors:
                with self.assertRaises(OSError):
                    _ = os.fstat(fd)
            self.assertEqual(len(descriptors), 42)
            self.assertIs(got.exception, primary)

    def test_member_read_error_beats_tar_close_and_closes_memory_owners(self) -> None:
        """Preserve the member failure while attempting the parser close."""
        data = inputs.pack(members(), check)
        primary = OSError("member_read_failure")
        close = tarfile.TarFile.close

        def closing(archive: tarfile.TarFile) -> None:
            close(archive)
            message = "secondary_tar_close"
            raise OSError(message)

        with (
            patch.object(inputs, "read_member", side_effect=primary),
            patch.object(tarfile.TarFile, "close", side_effect=closing),
            self.assertRaises(OSError) as got,
        ):
            _ = inputs.decode(data, check)
        self.assertIs(got.exception, primary)


if __name__ == "__main__":
    _ = unittest.main()
