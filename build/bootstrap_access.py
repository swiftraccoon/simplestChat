"""Bounded OpenSSH password enrollment without secret arguments or transcripts."""

# Fixed failure codes deliberately avoid including remote output or credentials.
# ruff: noqa: EM101

from __future__ import annotations

import contextlib
import errno
import os
import pty
import re
import select
import signal
import termios
import time
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, cast

if TYPE_CHECKING:
    from collections.abc import Sequence

ENROLLED = "SIMPLESTCHAT_BOOTSTRAP_KEY_ENROLLED"
MAX_TRANSCRIPT_BYTES = 65536
MAX_PASSWORD_EVENTS = 5


class BootstrapError(Exception):
    """A stable error code that never contains a child transcript or secret."""


def require(condition: object, code: str) -> None:
    """Reject an unmet boundary condition without disclosing its input."""
    if not condition:
        raise BootstrapError(code)


@dataclass
class PasswordDialogue:
    """Recognize only the bounded English OpenSSH/Debian password exchange."""

    current: str = field(repr=False)
    replacement: str = field(repr=False)
    events: set[str] = field(default_factory=set)
    rotated: bool = False
    enrolled: bool = False

    def consume(self, text: str) -> str | None:
        """Consume one recognized prompt; repeated or reordered prompts fail closed."""
        if ENROLLED in text:
            self.enrolled = True
        if "password updated successfully" in text.lower():
            require("confirm" in self.events, "unexpected_password_success")
            self.rotated = True
        patterns = (
            ("confirm", r"(?:retype|repeat) (?:new )?(?:unix )?password:\s*$"),
            ("new", r"(?:enter )?new (?:unix )?password:\s*$"),
            ("current", r"(?:\(current\)|current|old) (?:unix )?password:\s*$"),
            ("login", r"(?:^|[\r\n])[^\r\n]*password:\s*$"),
        )
        for event, pattern in patterns:
            if not re.search(pattern, text, re.IGNORECASE):
                continue
            require(event not in self.events, "repeated_password_prompt")
            require(len(self.events) < MAX_PASSWORD_EVENTS, "password_prompt_limit")
            if event == "new":
                require("login" in self.events or "current" in self.events, "password_prompt_order")
            if event == "confirm":
                require("new" in self.events, "password_prompt_order")
            if event in {"current", "login"}:
                require("new" not in self.events, "password_prompt_order")
            self.events.add(event)
            return self.replacement if event in {"new", "confirm"} else self.current
        return None


@dataclass(frozen=True)
class PasswordOutcome:
    """Non-sensitive observations retained after the PTY has been discarded."""

    enrolled: bool
    rotated: bool
    exit_status: int


def echo_disabled(descriptor: int) -> bool:
    """Check the PTY's documented integer local-flags field before a secret write."""
    flags = cast("int", termios.tcgetattr(descriptor)[3])
    return flags & termios.ECHO == 0


def stop_child(pid: int) -> None:
    """Reap only this PTY child, bounding a stuck OpenSSH process group."""

    def send(signum: signal.Signals) -> None:
        try:
            os.killpg(pid, signum)
        except PermissionError:
            # macOS may deny group signaling while allowing our owned child.
            with contextlib.suppress(ProcessLookupError):
                os.kill(pid, signum)
        except ProcessLookupError:
            pass

    send(signal.SIGTERM)
    deadline = time.monotonic() + 2
    while time.monotonic() < deadline:
        try:
            found, _ = os.waitpid(pid, os.WNOHANG)
        except ChildProcessError:
            return
        if found:
            return
        time.sleep(0.05)
    send(signal.SIGKILL)
    with contextlib.suppress(ChildProcessError):
        _ = os.waitpid(pid, 0)


def password_session(  # noqa: C901, PLR0912 -- One bounded PTY lifetime owns every exit path.
    argv: Sequence[str], current: str, replacement: str, *, timeout: float = 120
) -> PasswordOutcome:
    """Drive one authenticated enrollment attempt; never retain or print its output.

    Both supplied passwords stay in process memory and the PTY only. No child
    environment variable, command argument, exception or log contains either.
    Unsupported prompts and an enabled terminal echo abort before any write.
    """
    require(bool(argv) and timeout > 0, "invalid_password_session")
    dialogue = PasswordDialogue(current, replacement)
    pid, descriptor = pty.fork()
    if pid == 0:
        try:
            environment = dict(os.environ, LC_ALL="C", LANG="C")
            os.execvpe(argv[0], list(argv), environment)  # noqa: S606 -- Explicit reviewed OpenSSH argv, no shell or secret arguments.
        except OSError:
            os._exit(127)
    deadline = time.monotonic() + timeout
    pending = ""
    observed = 0
    status: int | None = None
    ended = False
    try:
        while time.monotonic() < deadline:
            readable, _, _ = select.select(
                [descriptor], [], [], max(0, min(0.2, deadline - time.monotonic()))
            )
            if readable:
                try:
                    chunk = os.read(descriptor, 4096)
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise BootstrapError("password_pty_read_failed") from None
                    chunk = b""
                if chunk:
                    observed += len(chunk)
                    require(observed <= MAX_TRANSCRIPT_BYTES, "password_output_limit")
                    pending += chunk.decode("utf-8", errors="replace")
                    response = dialogue.consume(pending)
                    if response is not None:
                        # SSH switches local terminal modes before reading. Never
                        # assume a banner saying "password:" establishes no echo.
                        require(echo_disabled(descriptor), "password_terminal_echo_enabled")
                        _ = os.write(descriptor, (response + "\n").encode())
                        pending = ""
                else:
                    ended = True
            if status is None:
                found, child_status = os.waitpid(pid, os.WNOHANG)
                if found:
                    status = os.waitstatus_to_exitcode(child_status)
            # Drain all PTY data, including markers written just before exit.
            if ended and status is not None:
                break
        require(status is not None and ended, "password_session_timeout")
        if status is None:
            raise BootstrapError("password_session_timeout")
        return PasswordOutcome(dialogue.enrolled, dialogue.rotated, status)
    finally:
        os.close(descriptor)
        if status is None:
            stop_child(pid)
