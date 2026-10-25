"""Pack one fixed restore-fixture source closure; unpack only its empty owned namespace."""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import math
import os
import re
import signal
import stat
import sys
import tarfile
import time
from pathlib import Path
from typing import TYPE_CHECKING, NoReturn, cast, override

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping, Sequence

MIGRATIONS = (
    "migrations/001_create_users.sql",
    "migrations/002_create_sessions.sql",
    "migrations/003_create_rooms.sql",
    "migrations/004_create_room_roles.sql",
    "migrations/005_create_room_states.sql",
    "migrations/006_add_session_token_index.sql",
    "migrations/007_add_webauthn_credential_id.sql",
    "migrations/008_canonicalize_user_emails.sql",
    "migrations/009_add_room_query_indexes.sql",
    "migrations/010_add_refresh_token_families.sql",
    "migrations/011_add_room_search_trigram_indexes.sql",
    "migrations/012_canonicalize_guest_ipv6_moderation_ips.sql",
    "migrations/013_community_accounts.sql",
    "migrations/014_create_room_reports.sql",
    "migrations/015_add_user_chat_style.sql",
    "migrations/016_constraints_and_indexes.sql",
    "migrations/017_moderation_history.sql",
    "migrations/018_user_chat_preferences.sql",
    "migrations/019_invites.sql",
    "migrations/020_invite_receipts_and_retention.sql",
    "migrations/021_invitation_secrets.sql",
    "migrations/022_require_current_sessions.sql",
    "migrations/023_independent_appearance.sql",
    "migrations/024_validate_independent_appearance.sql",
    "migrations/025_chat_history.sql",
    "migrations/026_validate_chat_moderation.sql",
    "migrations/027_chat_updates.sql",
    "migrations/028_chat_lookup_indexes.sql",
    "migrations/029_chat_reply_index.sql",
    "migrations/030_push_subscriptions.sql",
    "migrations/031_discovery.sql",
    "migrations/032_notification_preferences.sql",
    "migrations/033_attachments.sql",
    "migrations/034_room_image_version.sql",
    "migrations/035_role_numbering.sql",
    "migrations/036_validate_role_numbering.sql",
)
MEMBERS = (
    "monitoring-schema.sql",
    "public-runtime-grants.sql.j2",
    "postgres-image.txt",
    *MIGRATIONS,
)
MANIFEST = "restore-input-manifest.json"
MAX_MEMBER = 1024**2
MAX_TOTAL = len(MEMBERS) * MAX_MEMBER
MAX_MANIFEST = 131072
MAX_ARCHIVE = MAX_TOTAL + MAX_MANIFEST + (len(MEMBERS) + 1) * 1024 + 10240
CHUNK = 65536
FILE_MODE = 0o600
DIRECTORY_MODE = 0o700
REFUSAL = "restore_fixture_input_refused"
ROOT = Path("/root/simplestchat-vm-fixture")
ARCHIVE = ROOT / "restore-input.tar"
DESTINATION = ROOT / "restore-input"


class InputError(ValueError):
    """Carry a fixed fixture refusal, never archive paths or parser diagnostics."""


def require(value: object) -> None:
    """Refuse without projecting caller-controlled content."""
    if not value:
        raise InputError(REFUSAL)


def settle(
    actions: Sequence[Callable[[], object]], first: BaseException | None
) -> BaseException | None:
    """Attempt each original close once and preserve the earlier body error."""
    for action in actions:
        try:
            _ = action()
        except BaseException as error:  # noqa: BLE001 -- Every known peer must close.
            first = first or error
    return first


def pairs(values: list[tuple[str, object]]) -> dict[str, object]:
    """Reject duplicate manifest keys before they can overwrite a pin."""
    result: dict[str, object] = {}
    for key, value in values:
        require(key not in result)
        result[key] = value
    return result


def constant(_value: str) -> NoReturn:
    """Reject nonfinite JSON instead of admitting a parser extension."""
    raise InputError(REFUSAL)


def add_member(archive: tarfile.TarFile, name: str, data: bytes, check: Callable[[], None]) -> None:
    """Close the borrowed member stream without masking an add/write failure."""
    info = tarfile.TarInfo(name)
    info.size, info.mode, info.uid, info.gid, info.mtime = len(data), FILE_MODE, 0, 0, 0
    source = io.BytesIO(data)
    first: BaseException | None = None
    try:
        check()
        archive.addfile(info, source)
        check()
    except BaseException as error:  # noqa: BLE001 -- Member close follows every body outcome.
        first = error
    first = settle([source.close, check], first)
    if first is not None:
        raise first


def read_member(
    archive: tarfile.TarFile, row: tarfile.TarInfo, maximum: int, check: Callable[[], None]
) -> bytes:
    """Close the exact bounded member after read/validation, preserving the first error."""
    stream = archive.extractfile(row)
    require(stream is not None)
    if stream is None:
        raise InputError(REFUSAL)
    first: BaseException | None = None
    value = b""
    try:
        check()
        value = stream.read(maximum + 1)
        require(len(value) == row.size)
        check()
    except BaseException as error:  # noqa: BLE001 -- Member close follows every body outcome.
        first = error
    first = settle([stream.close, check], first)
    if first is not None:
        raise first
    return value


def pack(members: Mapping[str, bytes], check: Callable[[], None]) -> bytes:
    """Build deterministic data-only USTAR from already-retained bounded source bytes."""
    check()
    require(set(members) == set(MEMBERS))
    require(
        all(type(value) is bytes and 0 < len(value) <= MAX_MEMBER for value in members.values())
    )
    manifest = {
        "schemaVersion": 1,
        "members": {
            name: {"bytes": len(members[name]), "sha256": hashlib.sha256(members[name]).hexdigest()}
            for name in MEMBERS
        },
    }
    meta = (json.dumps(manifest, sort_keys=True, separators=(",", ":")) + "\n").encode()
    require(len(meta) <= MAX_MANIFEST)
    output = io.BytesIO()
    archive: tarfile.TarFile | None = None
    first: BaseException | None = None
    result = b""
    try:
        archive = tarfile.open(  # noqa: SIM115 -- Explicit close preserves first failure.
            fileobj=output, mode="w", format=tarfile.USTAR_FORMAT
        )
        for name, data in ((MANIFEST, meta), *((name, members[name]) for name in MEMBERS)):
            check()
            add_member(archive, name, data, check)
        completed, archive = archive, None
        completed.close()
        result = output.getvalue()
        require(len(result) <= MAX_ARCHIVE)
        check()
    except BaseException as error:  # noqa: BLE001 -- Close both parser/writer owners.
        first = error
    first = settle(([archive.close] if archive is not None else []) + [output.close, check], first)
    if first is not None:
        raise first
    return result


def decode(data: bytes, check: Callable[[], None]) -> dict[str, bytes]:
    """Validate every member and pin before any destination entry is created."""
    source = io.BytesIO(data)
    archive: tarfile.TarFile | None = None
    first: BaseException | None = None
    result: dict[str, bytes] = {}
    try:
        archive = tarfile.open(fileobj=source, mode="r:")  # noqa: SIM115 -- Preserve first failure.
        rows = archive.getmembers()
        require(len(rows) == len(MEMBERS) + 1 and not archive.pax_headers)
        for row in rows:
            check()
            require(row.name in (*MEMBERS, MANIFEST) and row.name not in result)
            require(
                row.isreg()
                and row.mode == FILE_MODE
                and row.uid == row.gid == 0
                and not row.pax_headers
            )
            maximum = MAX_MANIFEST if row.name == MANIFEST else MAX_MEMBER
            require(0 < row.size <= maximum)
            result[row.name] = read_member(archive, row, maximum, check)
        require(set(result) == {*MEMBERS, MANIFEST})
        metadata = cast(
            "object",
            json.loads(result.pop(MANIFEST), object_pairs_hook=pairs, parse_constant=constant),
        )
        require(type(metadata) is dict)
        meta = cast("dict[str, object]", metadata)
        require(
            set(meta) == {"schemaVersion", "members"}
            and type(meta["schemaVersion"]) is int
            and meta["schemaVersion"] == 1
        )
        require(type(meta["members"]) is dict)
        pins = cast("dict[str, object]", meta["members"])
        require(set(pins) == set(MEMBERS))
        for name in MEMBERS:
            require(type(pins[name]) is dict)
            pin = cast("dict[str, object]", pins[name])
            require(set(pin) == {"sha256", "bytes"} and type(pin["bytes"]) is int)
            require(
                pin["bytes"] == len(result[name])
                and pin["sha256"] == hashlib.sha256(result[name]).hexdigest()
            )
        check()
    except BaseException as error:  # noqa: BLE001 -- Reader and original bytes always close.
        first = error
    first = settle(([archive.close] if archive is not None else []) + [source.close, check], first)
    if first is not None:
        raise first
    return result


def stamp(value: os.stat_result) -> tuple[int, ...]:
    """Bind the source original fully, excluding read-induced atime."""
    return (
        value.st_dev,
        value.st_ino,
        value.st_mode,
        value.st_uid,
        value.st_gid,
        value.st_nlink,
        value.st_size,
        value.st_mtime_ns,
        value.st_ctime_ns,
    )


def directory(
    path: Path, check: Callable[[], None], owners: list[int], *, dir_fd: int | None = None
) -> tuple[int, tuple[int, ...]]:
    """Register a nofollow original directory before any post-open refusal."""
    check()
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=dir_fd)
    owners.append(fd)
    check()
    value = os.fstat(fd)
    require(stat.S_ISDIR(value.st_mode) and stat.S_IMODE(value.st_mode) == DIRECTORY_MODE)
    require(value.st_uid == os.geteuid() and value.st_gid == os.getegid())
    return fd, stamp(value)[:5]


def read_fd(fd: int, maximum: int, check: Callable[[], None]) -> bytes:
    """Bound physical reads and fence actual bytes before accepting them."""
    chunks: list[bytes] = []
    total = 0
    while True:
        check()
        try:
            value = os.read(fd, min(CHUNK, maximum + 1 - total))
        except InterruptedError:
            continue
        check()
        if not value:
            break
        total += len(value)
        require(total <= maximum)
        chunks.append(value)
    return b"".join(chunks)


def write_fd(fd: int, data: bytes, check: Callable[[], None]) -> None:
    """Retry short/EINTR writes under the original check and durably finish the exact file."""
    offset = 0
    while offset < len(data):
        check()
        try:
            size = os.write(fd, data[offset : offset + CHUNK])
        except InterruptedError:
            continue
        require(0 < size <= min(CHUNK, len(data) - offset))
        offset += size
        check()
    check()
    os.fsync(fd)
    check()


def read_original(
    path: Path, maximum: int, check: Callable[[], None], owners: list[int]
) -> tuple[int, os.stat_result, bytes]:
    """Register an original before admission and retain it until the caller closes all peers."""
    check()
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK)
    owners.append(fd)
    check()
    before = os.fstat(fd)
    require(stat.S_ISREG(before.st_mode) and stat.S_IMODE(before.st_mode) == FILE_MODE)
    require(
        before.st_uid == os.geteuid() and before.st_gid == os.getegid() and before.st_nlink == 1
    )
    require(0 < before.st_size <= maximum)
    data = read_fd(fd, maximum, check)
    require(
        len(data) == before.st_size and stamp(os.fstat(fd)) == stamp(before) == stamp(path.lstat())
    )
    return fd, before, data


def names(fd: int, check: Callable[[], None]) -> set[str]:
    """Enumerate the retained namespace rather than reopening a path name."""
    check()
    result = set(os.listdir(fd))
    check()
    return result


def seal_output(
    fd: int, data: bytes, identity: tuple[int, ...], check: Callable[[], None]
) -> tuple[int, ...]:
    """Read back the exact completed original, then seal its full immutable witness."""
    check()
    before = os.fstat(fd)
    require(stamp(before)[:6] == identity and before.st_size == len(data))
    check()
    require(os.lseek(fd, 0, os.SEEK_SET) == 0)
    check()
    require(read_fd(fd, MAX_MEMBER, check) == data)
    check()
    require(stamp(os.fstat(fd)) == stamp(before))
    check()
    return stamp(before)


def verify_outputs(
    outputs: list[tuple[int, int, str, tuple[int, ...]]], check: Callable[[], None]
) -> None:
    """Join every sealed private leaf to its retained original before any close."""
    for fd, directory_fd, leaf, completed in outputs:
        check()
        require(stamp(os.fstat(fd)) == completed)
        check()
        require(stamp(os.stat(leaf, dir_fd=directory_fd, follow_symlinks=False)) == completed)
        check()


def admit_destinations(
    destination: Path, check: Callable[[], None], owners: list[int]
) -> tuple[int, tuple[int, ...], int, tuple[int, ...]]:
    """Anchor both original empty directories before any target leaf is admitted."""
    parent, parent_stamp = directory(destination, check, owners)
    child_path = destination / "migrations"
    child, child_stamp = directory(Path("migrations"), check, owners, dir_fd=parent)
    check()
    require(stamp(destination.lstat())[:5] == parent_stamp)
    check()
    require(stamp(os.stat("migrations", dir_fd=parent, follow_symlinks=False))[:5] == child_stamp)
    check()
    require(stamp(child_path.lstat())[:5] == child_stamp)
    require(names(parent, check) == {"migrations"} and not names(child, check))
    return parent, parent_stamp, child, child_stamp


def unpack(archive: Path, sha256: str, destination: Path, check: Callable[[], None]) -> None:
    """Write only the fixed already-validated closure into two empty anchored directories."""
    owners: list[int] = []
    outputs: list[tuple[int, int, str, tuple[int, ...]]] = []
    first: BaseException | None = None
    try:
        check()
        require(re.fullmatch(r"[a-f0-9]{64}", sha256))
        source, before, data = read_original(archive, MAX_ARCHIVE, check, owners)
        require(hashlib.sha256(data).hexdigest() == sha256)
        values = decode(data, check)
        parent, parent_stamp, child, child_stamp = admit_destinations(destination, check, owners)
        child_path = destination / "migrations"
        for name in MEMBERS:
            check()
            directory_fd, leaf = (
                (child, name.split("/", 1)[1]) if name.startswith("migrations/") else (parent, name)
            )
            fd = os.open(
                leaf,
                os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
                0o600,
                dir_fd=directory_fd,
            )
            owners.append(fd)
            check()
            created = os.fstat(fd)
            require(
                stat.S_ISREG(created.st_mode)
                and stat.S_IMODE(created.st_mode) == FILE_MODE
                and created.st_uid == os.geteuid()
                and created.st_gid == os.getegid()
                and created.st_nlink == 1
            )
            write_fd(fd, values[name], check)
            completed = seal_output(fd, values[name], stamp(created)[:6], check)
            outputs.append((fd, directory_fd, leaf, completed))
        for fd in (child, parent):
            check()
            os.fsync(fd)
            check()
        # Own leaf additions can change directory nlink on macOS; bind identity/mode/owner
        # and the exact final namespace instead of adopting an arbitrary foreign link.
        check()
        require(stamp(os.fstat(parent))[:5] == parent_stamp)
        check()
        require(stamp(destination.lstat())[:5] == parent_stamp)
        check()
        require(stamp(os.fstat(child))[:5] == child_stamp)
        check()
        require(stamp(child_path.lstat())[:5] == child_stamp)
        require(names(parent, check) == {"migrations", *MEMBERS[:3]})
        require(names(child, check) == {name.split("/", 1)[1] for name in MIGRATIONS})
        verify_outputs(outputs, check)
        check()
        require(stamp(os.fstat(source)) == stamp(before))
        check()
        require(stamp(archive.lstat()) == stamp(before))
        check()
    except BaseException as error:  # noqa: BLE001 -- Close every admitted original.
        first = error
    first = settle([*[lambda fd=fd: os.close(fd) for fd in reversed(owners)], check], first)
    if first is not None:
        raise first


def check_marker(check: Callable[[], None]) -> None:
    """Read only the fixed marked-guest original with bounded nofollow ownership."""
    owners: list[int] = []
    first: BaseException | None = None
    try:
        _fd, _before, marker = read_original(
            Path("/etc/simplestchat-vm-fixture"), 128, check, owners
        )
        require(re.fullmatch(rb"[a-f0-9]{32}\n", marker))
        check()
    except BaseException as error:  # noqa: BLE001 -- Never replace marker failure with close.
        first = error
    first = settle([*[lambda fd=fd: os.close(fd) for fd in reversed(owners)], check], first)
    if first is not None:
        raise first


class Options(argparse.Namespace):
    """Expose only the fixed checksum input, never a caller-selected path."""

    sha256: str = ""


class Parser(argparse.ArgumentParser):
    """Keep unreviewed option text out of public diagnostics."""

    @override
    def error(self, message: str) -> NoReturn:
        _ = message
        self.exit(2, "Restore fixture input options are invalid.\n")


def main() -> int:
    """Use only fixed fixture paths and a subordinate30s role inside the parent setup cap."""
    start = time.monotonic()
    parser = Parser(allow_abbrev=False)
    _ = parser.add_argument("--sha256", required=True)
    args = parser.parse_args(namespace=Options())
    cancelled = [0]
    handlers: dict[signal.Signals, object] = {}
    mask = os.umask(0o077)
    status = 1

    def interruption(number: int, _frame: object) -> None:
        cancelled[0] = cancelled[0] or (130 if number == signal.SIGINT else 143)

    def check() -> None:
        now = time.monotonic()
        require(math.isfinite(now) and start <= now < start + 30 and not cancelled[0])

    try:
        for number in (signal.SIGINT, signal.SIGTERM):
            handlers[number] = signal.signal(number, interruption)
        require(sys.platform == "linux" and os.geteuid() == os.getegid() == 0)
        check()
        check_marker(check)
        unpack(ARCHIVE, args.sha256, DESTINATION, check)
        data = (
            json.dumps({"schemaVersion": 1, "unpacked": True, "members": len(MEMBERS)}) + "\n"
        ).encode()
        require(sys.stdout.buffer.write(data) == len(data))
        _ = sys.stdout.buffer.flush()
        status = 0
    except (OSError, ValueError, tarfile.TarError, RuntimeError):
        _ = sys.stderr.write("Restore fixture input unpacking is unproven.\n")
    finally:
        actions: list[Callable[[], object]] = []
        for number, handler in handlers.items():
            actions.append(
                lambda number=number, handler=handler: signal.signal(
                    number, cast("int | Callable[[int, object], None] | None", handler)
                )
            )
        actions.append(lambda: os.umask(mask))
        if settle(actions, None) is not None:
            status = 1
    try:
        check()
    except (OSError, ValueError):
        status = 1
    return cancelled[0] or status


if __name__ == "__main__":
    raise SystemExit(main())
