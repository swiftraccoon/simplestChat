"""Build the pinned act runner with one shared concurrency limit for actual jobs."""

from __future__ import annotations

import gzip
import hashlib
import io
import json
import os
import posixpath
import re
import sys
import tarfile
import tempfile
import urllib.request
from pathlib import Path, PurePosixPath
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
MAX_SOURCE_BYTES = 256 * 1024**2
MAX_SOURCE_MEMBER = 16 * 1024**2
MAX_SOURCE_MEMBERS = 10000
MAX_SOURCE_PATH = 4096
FIRST_PRINTABLE = 32


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


def source_members(source: tarfile.TarFile, root: str) -> dict[str, tarfile.TarInfo]:
    """Validate the entire bounded source tree before writing any member."""
    members: dict[str, tarfile.TarInfo] = {}
    for member in source:
        name = member.name.rstrip("/") if member.isdir() else member.name
        path = PurePosixPath(name)
        require(
            len(members) < MAX_SOURCE_MEMBERS
            and 0 < len(name) <= MAX_SOURCE_PATH
            and not path.is_absolute()
            and str(path) == name
            and bool(path.parts)
            and path.parts[0] == root
            and ".." not in path.parts
            and "\\" not in name
            and all(ord(character) >= FIRST_PRINTABLE for character in name),
            "local_act_source_path",
        )
        require(name not in members, "local_act_source_duplicate")
        require(
            (member.isfile() or member.isdir() or member.issym()) and member.sparse is None,
            "local_act_source_type",
        )
        require(
            0 <= member.size <= MAX_SOURCE_MEMBER and (member.isfile() or member.size == 0),
            "local_act_source_size",
        )
        members[name] = member
    require(root in members and members[root].isdir(), "local_act_source_root")
    for name, member in members.items():
        for parent in PurePosixPath(name).parents:
            if parent != PurePosixPath("."):
                require(
                    str(parent) in members and members[str(parent)].isdir(),
                    "local_act_source_parent",
                )
        if member.issym():
            target = posixpath.normpath(str(PurePosixPath(name).parent / member.linkname))
            require(
                0 < len(member.linkname) <= MAX_SOURCE_PATH
                and not PurePosixPath(member.linkname).is_absolute()
                and "\\" not in member.linkname
                and target in members
                and members[target].isfile(),
                "local_act_source_link",
            )
    return members


def extract_source(archive: Path, workspace: Path, revision: str) -> Path:
    """Write only validated files/directories and internal regular-file symlinks."""
    require(re.fullmatch(r"[a-f0-9]{40}", revision), "local_act_source_revision")
    with gzip.open(archive, "rb") as compressed:
        expanded = compressed.read(MAX_SOURCE_BYTES + 1)
    require(len(expanded) <= MAX_SOURCE_BYTES, "local_act_source_expansion")
    root = "act-" + revision
    with tarfile.open(fileobj=io.BytesIO(expanded), mode="r:") as source:
        members = source_members(source, root)
        for name, member in sorted(
            members.items(), key=lambda item: len(PurePosixPath(item[0]).parts)
        ):
            if member.isdir():
                (workspace / name).mkdir(mode=0o755)
        for name, member in members.items():
            if not member.isfile():
                continue
            stream = source.extractfile(member)
            require(stream is not None, "local_act_source_missing")
            if stream is None:
                raise ToolError("local_act_source_missing")  # noqa: EM101 -- Fixed diagnostic.
            with stream:
                data = stream.read(MAX_SOURCE_MEMBER + 1)
            require(len(data) == member.size, "local_act_source_truncated")
            target = workspace / name
            with target.open("xb") as output:
                _ = output.write(data)
            mode = member.mode & 0o755
            if not mode & 0o100:
                mode &= ~0o111
            target.chmod(mode | 0o600)
        for name, member in members.items():
            if member.issym():
                (workspace / name).symlink_to(member.linkname)
    return workspace / root


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
        source_root = extract_source(archive, workspace, revision)
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
    except (
        ToolError,
        OSError,
        ValueError,
        KeyError,
        EOFError,
        tarfile.TarError,
        bounded_process.ProcessError,
    ) as error:
        _ = sys.stderr.write(f"Local act preparation failed: {error}\n")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
