"""Overlap authenticated scanner downloads with the isolated release fixture.

Only the fixture may access Docker here. Image scanning starts in its existing
later CI step, after this helper has reaped both bounded child processes.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import signal
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import TYPE_CHECKING

from security_context import ROOT, executable
from security_tools import ToolError, require, write_private

# isort: split
# security_context makes the shared standalone host helpers importable.
import bounded_process

if TYPE_CHECKING:
    from collections.abc import Sequence
    from types import FrameType


@dataclass(frozen=True)
class Task:
    """A fixed command with independent process and output bounds."""

    name: str
    command: tuple[str, ...]
    timeout: float


@dataclass(frozen=True)
class Result:
    """Safe status metadata; command output remains in separate private files."""

    name: str
    status: int | None
    failure: str | None
    seconds: float


def run_task(task: Task, output: Path, cancelled: threading.Event) -> Result:
    """Reap a single command on exit, cancellation, timeout or output overflow."""
    started = time.monotonic()
    status: int | None = None
    failure: str | None = None
    try:
        streams = [output / f"{task.name}.{suffix}" for suffix in ("stdout", "stderr")]
        for path in streams:
            write_private(path, b"", 0o600)
        with streams[0].open("wb") as stdout, streams[1].open("wb") as stderr:
            status, _, _ = bounded_process.run(
                task.command,
                cwd=ROOT,
                output=stdout,
                error=stderr,
                limits=bounded_process.Limits(
                    timeout=task.timeout, healthy=lambda: not cancelled.is_set()
                ),
            )
    except (OSError, RuntimeError) as error:
        failure = (
            str(error) if isinstance(error, bounded_process.ProcessError) else type(error).__name__
        )
    return Result(task.name, status, failure, round(time.monotonic() - started, 3))


def run_tasks(tasks: tuple[Task, Task], output: Path, cancelled: threading.Event) -> list[Result]:
    """Keep both independent outcomes; one ordinary failure does not hide the other."""
    with ThreadPoolExecutor(max_workers=2) as executor:
        futures = [executor.submit(run_task, task, output, cancelled) for task in tasks]
        try:
            return [future.result() for future in futures]
        except BaseException:
            # Unexpected parent failures also terminate and reap owned children.
            cancelled.set()
            raise


def tasks(controller: Path, image: str, fixture_output: Path, output: Path) -> tuple[Task, Task]:
    """Build the existing fixture command and a download-only pinned installer."""
    return (
        Task(
            "release-fixture",
            (
                executable("sudo"),
                str(controller),
                "-B",
                "build/test-release-container.py",
                "--disposable-host",
                "--image",
                image,
                "--output",
                str(fixture_output),
            ),
            # The fixture bounds execution to 480s and its cleanup to 100s.
            590,
        ),
        Task(
            "scanner-tools",
            (
                sys.executable,
                "-B",
                "build/security_tools.py",
                "install",
                "--tools",
                "syft",
                "grype",
                "gitleaks",
                "--platform",
                "linux-x86_64",
                "--directory",
                str(output / "tools"),
            ),
            300,
        ),
    )


@dataclass
class Options(argparse.Namespace):
    """Explicit paths owned by this CI invocation, never ambient commands."""

    controller: Path = Path()
    image: str = ""
    fixture_output: Path = Path()
    output: Path = Path()


def main(argv: Sequence[str] | None = None) -> int:
    """Run both checks without publishing success until both have finished."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("--controller", type=Path, required=True)
    _ = parser.add_argument("--image", required=True)
    _ = parser.add_argument("--fixture-output", type=Path, required=True)
    _ = parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args(argv, namespace=Options())
    _ = os.umask(0o077)
    cancelled = threading.Event()

    def cancel(_number: int, _frame: FrameType | None) -> None:
        cancelled.set()

    previous = {number: signal.signal(number, cancel) for number in (signal.SIGINT, signal.SIGTERM)}
    try:
        require(re.fullmatch(r"sha256:[a-f0-9]{64}", args.image), "release_image_not_immutable")
        require(args.controller.is_absolute() and args.controller.is_file(), "invalid_controller")
        require(args.fixture_output.is_absolute() and args.output.is_absolute(), "relative_output")
        args.output.mkdir(mode=0o700)
        results = run_tasks(
            tasks(args.controller, args.image, args.fixture_output, args.output),
            args.output,
            cancelled,
        )
        passed = not cancelled.is_set() and all(
            result.status == 0 and result.failure is None for result in results
        )
        write_private(
            args.output / "summary.json",
            (
                json.dumps(
                    {"passed": passed, "checks": [asdict(result) for result in results]}, indent=2
                )
                + "\n"
            ).encode(),
            0o600,
        )
        for result in results:
            _ = sys.stdout.write(
                f"{result.name}: status={result.status}, failure={result.failure}, "
                + f"seconds={result.seconds}\n"
            )
    except (OSError, ToolError) as error:
        _ = sys.stderr.write(
            (str(error) if isinstance(error, ToolError) else type(error).__name__) + "\n"
        )
        return 1
    else:
        return 0 if passed and not cancelled.is_set() else 1
    finally:
        cancelled.set()
        for number, handler in previous.items():
            _ = signal.signal(number, handler)


if __name__ == "__main__":
    raise SystemExit(main())
