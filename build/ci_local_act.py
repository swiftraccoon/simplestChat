"""Build the pinned act runner with one shared concurrency limit for actual jobs."""

from __future__ import annotations

import hashlib
import json
import os
import sys
import tarfile
import tempfile
import urllib.request
from pathlib import Path
from typing import TYPE_CHECKING, cast

from security_context import executable
from security_tools import ToolError, bounded_file, require

# isort: split
import bounded_process

if TYPE_CHECKING:
    from http.client import HTTPResponse

ROOT = Path(__file__).resolve().parents[1]
PATCH = ROOT / "build/ci-local-act.patch"
PIN = ROOT / "build/ci-local-act.json"


def digest(path: Path) -> str:
    """Hash an existing artifact without loading the executable into memory."""
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def run(argv: list[str], environment: dict[str, str], cwd: Path = ROOT) -> bytes:
    """Bound the owned compiler and all descendants, including cancellation."""
    status, output, error = bounded_process.run(
        argv,
        cwd=cwd,
        env=environment,
        limits=bounded_process.Limits(timeout=300, stdout=16 * 1024**2, stderr=16 * 1024**2),
    )
    _ = sys.stderr.write(error.decode(errors="replace"))
    require(status == 0, "local_act_build_command_failed")
    return output


def publish(staging: Path, target: Path, key: str) -> None:
    """Expose only a complete executable and integrity receipt, using one rename."""
    receipt = staging / "receipt.json"
    _ = receipt.write_text(
        json.dumps({"inputSha256": key, "binarySha256": digest(staging / "act")}) + "\n"
    )
    receipt.chmod(0o600)
    _ = staging.rename(target)


def prepare() -> Path:
    """Authenticate source and patch, test the limiter, and reuse only an intact build."""
    pin_bytes = bounded_file(PIN, 4096)
    pin = cast("dict[str, str | int]", json.loads(pin_bytes))
    require(digest(PATCH) == pin["patchSha256"], "local_act_patch_digest")
    compiler = executable("go")
    environment = dict(
        os.environ,
        GOENV="off",
        GOTOOLCHAIN="local",
        GOFLAGS="",
        CGO_ENABLED="0",
        GOWORK="off",
        GOPRIVATE="",
        GONOSUMDB="",
        GOINSECURE="",
        GOSUMDB="sum.golang.org",
    )
    identity = run([compiler, "env", "GOVERSION", "GOOS", "GOARCH"], environment)
    key = hashlib.sha256(pin_bytes + identity).hexdigest()
    cache = ROOT / ".cache/ci-act"
    target = cache / key
    binary, receipt = target / "act", target / "receipt.json"
    if target.exists():
        recorded = cast("object", json.loads(bounded_file(receipt, 4096)))
        require(
            recorded == {"inputSha256": key, "binarySha256": digest(binary)}
            and os.access(binary, os.X_OK),
            "local_act_cache_integrity",
        )
        return binary

    cache.mkdir(parents=True, exist_ok=True)
    for name in ("go-build", "go-modules", "go-path", "tmp"):
        (cache / name).mkdir(exist_ok=True)
    environment.update(
        GOCACHE=str(cache / "go-build"),
        GOMODCACHE=str(cache / "go-modules"),
        GOPATH=str(cache / "go-path"),
        GOTMPDIR=str(cache / "tmp"),
        TMPDIR=str(cache / "tmp"),
    )
    with tempfile.TemporaryDirectory(prefix="build-", dir=cache) as temporary:
        workspace = Path(temporary)
        archive = workspace / "source.tar.gz"
        revision = str(pin["revision"])
        url = f"https://codeload.github.com/nektos/act/tar.gz/{revision}"
        expected_bytes = int(pin["archiveBytes"])
        with (
            cast("HTTPResponse", urllib.request.urlopen(url, timeout=120)) as response,
            archive.open("wb") as output,
        ):
            data = response.read(expected_bytes + 1)
            require(len(data) == expected_bytes, "local_act_archive_size")
            _ = output.write(data)
        require(digest(archive) == pin["archiveSha256"], "local_act_archive_digest")
        with tarfile.open(archive) as source:
            source.extractall(workspace, filter="data")
        source_root = workspace / f"act-{revision}"
        published = workspace / "published"
        published.mkdir(mode=0o700)
        commands = [
            [executable("patch"), "--batch", "--forward", "-p1", "-i", str(PATCH)],
            [
                compiler,
                "test",
                "-mod=readonly",
                "./pkg/runner",
                "-run",
                "^TestLocalGlobalLeafLimit",
                "-count=1",
            ],
            [
                compiler,
                "build",
                "-mod=readonly",
                "-trimpath",
                "-ldflags",
                f"-X main.version={pin['version']}-local-job-limit",
                "-o",
                str(published / "act"),
                ".",
            ],
        ]
        for command in commands:
            _ = sys.stderr.write(run(command, environment, source_root).decode(errors="replace"))
        publish(published, target, key)
    return binary


def main() -> int:
    """Print the authenticated local executable, without changing global tools."""
    try:
        _ = sys.stdout.write(str(prepare()) + "\n")
    except (ToolError, OSError, ValueError, KeyError, bounded_process.ProcessError) as error:
        _ = sys.stderr.write(f"Local act preparation failed: {error}\n")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
