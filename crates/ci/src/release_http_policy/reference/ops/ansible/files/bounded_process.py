"""Stream owned POSIX subprocesses under byte and elapsed-time limits.

Limits apply before writing evidence or buffering a response. Callers provide
separate destinations and budgets for stdout and stderr. Cancellation and every
failure terminate the owned process group and reap its direct child; callers
must still recover any external resources a command created.
"""

from __future__ import annotations

import io
import os
import re
import selectors
import signal
import subprocess
import sys
import time
from contextlib import suppress
from dataclasses import dataclass
from typing import TYPE_CHECKING, cast

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping, Sequence
    from pathlib import Path
    from typing import BinaryIO

MAX_CAPTURE = 2 * 1024**2
CHUNK_SIZE = 65536
POLL_SECONDS = 0.1


class ProcessError(RuntimeError):
    """A fixed failure code; never include captured command output or secrets."""


@dataclass(frozen=True, kw_only=True)
class Limits:
    """Independent output ceilings, timeout and optional ongoing ownership check."""

    timeout: float = 30
    stdout: int = MAX_CAPTURE
    stderr: int = MAX_CAPTURE
    healthy: Callable[[], bool] | None = None


DEFAULT_LIMITS = Limits()


def leader_exited(pid: int) -> bool:
    """Observe exit without reaping, retaining the leader PID and process-group identity."""
    return os.waitid(os.P_PID, pid, os.WEXITED | os.WNOHANG | os.WNOWAIT) is not None


def wait_exited(pid: int, timeout: float) -> bool:
    """Give the leader a bounded grace period without releasing its PID for reuse."""
    deadline = time.monotonic() + timeout
    while not leader_exited(pid):
        if time.monotonic() >= deadline:
            return False
        time.sleep(POLL_SECONDS)
    return True


def darwin_zombie_group(pid: int) -> bool:
    """Confirm an owned, unreaped leader belongs to a group containing only zombies.

    Darwin's group-signaling path excludes zombies and can report EPERM when no
    live member remains. An exited leader alone is insufficient: inaccessible
    live descendants must still make cleanup fail. The fixed system ps command
    produces only numeric identity and state fields, never arguments or secrets.
    """
    if not leader_exited(pid):
        return False
    output, error = io.BytesIO(), io.BytesIO()
    try:
        child = subprocess.Popen(  # noqa: S603 -- Fixed trusted system utility and numeric owned process-group ID.
            ["/bin/ps", "-g", str(pid), "-o", "pid=,pgid=,stat="],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env={"PATH": "/usr/bin:/bin", "LC_ALL": "C"},
            start_new_session=True,
        )
    except OSError:
        return False
    try:
        try:
            status = pump(child, output, error, Limits(timeout=2, stdout=65536, stderr=65536))
        except BaseException:
            # This known no-fork utility must not recursively invoke group
            # cleanup while it is being used to resolve a group cleanup error.
            with suppress(ProcessLookupError):
                child.kill()
            _ = child.wait(timeout=5)
            raise
    except (OSError, ProcessError, subprocess.SubprocessError):
        return False
    finally:
        for stream in (child.stdout, child.stderr):
            if stream is not None:
                stream.close()
    if status != 0 or error.getvalue():
        return False
    return zombie_group_rows(output.getvalue(), pid) and leader_exited(pid)


def zombie_group_rows(output: bytes, pid: int) -> bool:
    """Require complete numeric ps rows, a zombie leader and no live or foreign member."""
    members: set[int] = set()
    for row in output.splitlines():
        match = re.fullmatch(
            rb"[ \t]*([1-9][0-9]{0,9})[ \t]+([1-9][0-9]{0,9})[ \t]+Z[+<>AELNSsVWX]*[ \t]*",
            row,
        )
        if match is None:
            return False
        member, group = int(match[1]), int(match[2])
        if group != pid or member in members:
            return False
        members.add(member)
    return pid in members


def signal_group(pid: int, signum: signal.Signals) -> None:
    """Signal an unreaped owned process group; fail closed on uncertain permission errors."""
    try:
        os.killpg(pid, signum)
    except ProcessLookupError:
        return
    except PermissionError:
        if sys.platform != "darwin" or not darwin_zombie_group(pid):
            raise


def stop(child: subprocess.Popen[bytes]) -> None:
    """Terminate the command's private process group, escalating within five seconds."""
    signal_group(child.pid, signal.SIGTERM)
    _ = wait_exited(child.pid, 5)
    # The unreaped leader keeps this PGID reserved. Descendants may ignore TERM
    # even when the leader has exited; remove them before reaping that leader.
    signal_group(child.pid, signal.SIGKILL)
    _ = child.wait(timeout=5)


def pump(  # noqa: C901, PLR0912 -- One selector loop owns simultaneous input, output, deadline and cancellation.
    child: subprocess.Popen[bytes],
    output: BinaryIO,
    error: BinaryIO,
    limits: Limits,
    *,
    input_data: bytes = b"",
) -> int:
    """Drain both pipes concurrently, enforcing limits before each retained write.

    The child must have PIPE output and a private session. The caller owns process
    cleanup and pipe closure if this function raises. A health predicate is polled
    even when no output arrives, so lease loss cancels a quiet preparation command.
    """
    if child.stdout is None or child.stderr is None:
        message = "missing_output_pipes"
        raise ProcessError(message)
    deadline = time.monotonic() + limits.timeout
    sizes = {"stdout": 0, "stderr": 0}
    pending = memoryview(input_data)
    with selectors.DefaultSelector() as selector:
        for stream, name in ((child.stdout, "stdout"), (child.stderr, "stderr")):
            os.set_blocking(stream.fileno(), False)
            _ = selector.register(stream, selectors.EVENT_READ, name)
        if child.stdin is not None:
            if pending:
                os.set_blocking(child.stdin.fileno(), False)
                _ = selector.register(child.stdin, selectors.EVENT_WRITE, "stdin")
            else:
                child.stdin.close()
        while selector.get_map() or not leader_exited(child.pid):
            if limits.healthy is not None and not limits.healthy():
                message = "command_lease_lost"
                raise ProcessError(message)
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                message = "command_timed_out"
                raise ProcessError(message)
            for key, _ in selector.select(min(remaining, POLL_SECONDS)):
                event = cast("object", key.data)
                if event == "stdin":
                    sent = os.write(key.fd, pending[:CHUNK_SIZE])
                    pending = pending[sent:]
                    if not pending and child.stdin is not None:
                        _ = selector.unregister(key.fd)
                        child.stdin.close()
                    continue
                chunk = os.read(key.fd, CHUNK_SIZE)
                if not chunk:
                    _ = selector.unregister(key.fd)
                    continue
                name = "stdout" if event == "stdout" else "stderr"
                limit = limits.stdout if name == "stdout" else limits.stderr
                destination = output if name == "stdout" else error
                available = limit - sizes[name]
                _ = destination.write(chunk[:available])
                sizes[name] += len(chunk)
                if sizes[name] > limit:
                    message = "command_" + name + "_limit_exceeded"
                    raise ProcessError(message)
        if limits.healthy is not None and not limits.healthy():
            message = "command_lease_lost"
            raise ProcessError(message)
        return child.wait(timeout=max(0.001, deadline - time.monotonic()))


def run(  # noqa: PLR0913 -- Stream budgets and destinations are independent caller policies.
    argv: Sequence[str],
    *,
    limits: Limits = DEFAULT_LIMITS,
    cwd: Path | None = None,
    env: Mapping[str, str] | None = None,
    input_data: bytes = b"",
    output: BinaryIO | None = None,
    error: BinaryIO | None = None,
) -> tuple[int, bytes, bytes]:
    """Run explicit argv and return bounded captures for streams without destinations."""
    stdout = output if output is not None else io.BytesIO()
    stderr = error if error is not None else io.BytesIO()
    with subprocess.Popen(  # noqa: S603 -- reviewed argv; no shell interpolation.
        argv,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        cwd=cwd,
        env=env,
        start_new_session=True,
    ) as child:
        try:
            status = pump(child, stdout, stderr, limits, input_data=input_data)
        except BaseException:
            stop(child)
            raise
    return (
        status,
        stdout.getvalue() if isinstance(stdout, io.BytesIO) else b"",
        stderr.getvalue() if isinstance(stderr, io.BytesIO) else b"",
    )
