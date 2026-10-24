"""Own one generated host-key entry in a disposable CI user's real SSH home.

OpenSSH may obtain this directory from the password database rather than HOME.
The fixture refuses existing configuration or known-host entries, retains the
original directories and file, and removes only its own unchanged entry. This
is a VM fixture boundary, not a replacement for the product's OpenSSH policy.
"""

from __future__ import annotations

import os
import re
import stat
from typing import TYPE_CHECKING, final

from security_tools import ToolError, require

if TYPE_CHECKING:
    from collections.abc import Callable
    from pathlib import Path

DIRECTORY = os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW
MIN_PORT = 1024
MAX_PORT = 65535
DIRECTORY_MODE = 0o700
PRIVATE_MODE = 0o600


def identity(value: os.stat_result) -> tuple[int, int]:
    """Compare a retained directory with its current namespace entry."""
    return value.st_dev, value.st_ino


def file_stamp(value: os.stat_result) -> tuple[int, ...]:
    """Bind the admitted known-host file without reopening a different inode."""
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


@final
class KnownHost:
    """Exclusively publish one pinned entry, with explicit checked retirement."""

    def __init__(self, home: Path, port: int, host_key: str) -> None:
        """Admit only the generated loopback key beneath a trusted UID-owned home."""
        require(home.is_absolute() and home.resolve() == home, "fetch_vm_ssh_home")
        require(MIN_PORT <= port <= MAX_PORT, "fetch_vm_ssh_port")
        require(
            re.fullmatch(r"ssh-ed25519 [A-Za-z0-9+/=]{68} vm-fixture", host_key),
            "fetch_vm_host_key",
        )
        self.home = home
        self.home_fd: int | None = None
        self.ssh_fd: int | None = None
        self.file_fd: int | None = None
        self.created_directory = False
        self.directory_identity: tuple[int, int] | None = None
        self.published = False
        self.stamp: tuple[int, ...] | None = None
        self.sealed_bytes = b""
        self.data = f"[127.0.0.1]:{port} {host_key}\n".encode("ascii")
        try:
            self.home_fd = os.open(home, DIRECTORY)
            home_stat = os.fstat(self.home_fd)
            require(
                home_stat.st_uid == os.getuid() and not home_stat.st_mode & 0o022,
                "fetch_vm_ssh_home",
            )
            try:
                os.mkdir(".ssh", DIRECTORY_MODE, dir_fd=self.home_fd)
                self.created_directory = True
            except FileExistsError:
                pass
            self.ssh_fd = os.open(".ssh", DIRECTORY, dir_fd=self.home_fd)
            ssh_stat = os.fstat(self.ssh_fd)
            require(
                ssh_stat.st_uid == os.getuid() and stat.S_IMODE(ssh_stat.st_mode) == DIRECTORY_MODE,
                "fetch_vm_ssh_directory",
            )
            self.directory_identity = identity(ssh_stat)
            self._absent("config")
            self._absent("known_hosts")
            self.file_fd = os.open(
                "known_hosts",
                os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW,
                PRIVATE_MODE,
                dir_fd=self.ssh_fd,
            )
            self.published = True
            self.stamp = file_stamp(os.fstat(self.file_fd))
            remaining = memoryview(self.data)
            while remaining:
                written = os.write(self.file_fd, remaining)
                require(written > 0, "fetch_vm_known_hosts_write")
                remaining = remaining[written:]
            os.fsync(self.file_fd)
            sealed = file_stamp(os.fstat(self.file_fd))
            require(
                os.pread(self.file_fd, len(self.data) + 1, 0) == self.data
                and file_stamp(os.fstat(self.file_fd)) == sealed,
                "fetch_vm_known_hosts_changed",
            )
            self.stamp, self.sealed_bytes = sealed, self.data
            os.fsync(self.ssh_fd)
            os.fsync(self.home_fd)
            self.check()
        except BaseException as original:
            try:
                self.finish()
            except BaseException as cleanup:
                raise original from cleanup
            raise

    def _absent(self, name: str) -> None:
        try:
            _ = os.stat(name, dir_fd=self.ssh_fd, follow_symlinks=False)
        except FileNotFoundError:
            return
        message = "fetch_vm_ssh_collision"
        raise ToolError(message)

    def check(self) -> None:
        """Refuse substitutions before and after an actual native SSH invocation."""
        home, ssh, descriptor = self.home_fd, self.ssh_fd, self.file_fd
        if home is None or ssh is None or descriptor is None:
            message = "fetch_vm_ssh_owner"
            raise ToolError(message)
        home_stat, ssh_stat = os.fstat(home), os.fstat(ssh)
        require(
            identity(os.lstat(self.home)) == identity(home_stat)
            and home_stat.st_uid == os.getuid()
            and not home_stat.st_mode & 0o022,
            "fetch_vm_ssh_home_changed",
        )
        require(
            identity(os.stat(".ssh", dir_fd=home, follow_symlinks=False)) == self.directory_identity
            and ssh_stat.st_uid == os.getuid()
            and stat.S_IMODE(ssh_stat.st_mode) == DIRECTORY_MODE,
            "fetch_vm_ssh_directory_changed",
        )
        self._absent("config")
        original = os.fstat(descriptor)
        named = os.stat("known_hosts", dir_fd=ssh, follow_symlinks=False)
        require(
            file_stamp(original) == self.stamp
            and file_stamp(named) == self.stamp
            and stat.S_ISREG(original.st_mode)
            and stat.S_IMODE(original.st_mode) == PRIVATE_MODE
            and original.st_uid == os.getuid()
            and original.st_nlink == 1
            and os.pread(descriptor, len(self.data) + 1, 0) == self.data,
            "fetch_vm_known_hosts_changed",
        )

    def _unlink(self) -> None:
        """Refuse to remove any substituted or altered known-host entry."""
        descriptor, ssh = self.file_fd, self.ssh_fd
        if descriptor is None or ssh is None:
            message = "fetch_vm_ssh_owner"
            raise ToolError(message)
        original = os.fstat(descriptor)
        named = os.stat("known_hosts", dir_fd=ssh, follow_symlinks=False)
        require(
            identity(named) == identity(original)
            and file_stamp(original) == self.stamp
            and file_stamp(named) == self.stamp
            and os.pread(descriptor, len(self.sealed_bytes) + 1, 0) == self.sealed_bytes,
            "fetch_vm_known_hosts_changed",
        )
        os.unlink("known_hosts", dir_fd=ssh)
        self.published = False
        os.fsync(ssh)

    def _remove_directory(self) -> None:
        """Retire only the original empty directory created by this fixture."""
        home = self.home_fd
        if home is None:
            message = "fetch_vm_ssh_owner"
            raise ToolError(message)
        require(
            identity(os.stat(".ssh", dir_fd=home, follow_symlinks=False))
            == self.directory_identity,
            "fetch_vm_ssh_directory_changed",
        )
        os.rmdir(".ssh", dir_fd=home)
        self.created_directory = False
        os.fsync(home)

    def finish(self) -> None:
        """Attempt every owned retirement while preserving any first failure."""
        failures: list[BaseException] = []

        def attempt(action: Callable[[], None]) -> None:
            try:
                action()
            except BaseException as error:  # noqa: BLE001 -- Cancellation must retire all peers.
                failures.append(error)

        home, ssh, descriptor = self.home_fd, self.ssh_fd, self.file_fd
        if self.published:
            attempt(self._unlink)
        if self.created_directory:
            attempt(self._remove_directory)
        self.file_fd, self.ssh_fd, self.home_fd = None, None, None
        for peer in (descriptor, ssh, home):
            if peer is not None:
                attempt(lambda peer=peer: os.close(peer))
        if failures:
            raise failures[0]
