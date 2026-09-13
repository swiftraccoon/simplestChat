#!/usr/bin/env python3
"""Build one production-only Linux release locally, without a registry push.

Requires an already configured local Docker/Buildx installation. The checked-in
Dockerfile remains authoritative for native provenance and dependency checks.
The output is a fresh, private evidence directory; failures are retained and
never retried automatically. No running application container is changed.
"""

import argparse
from contextlib import contextmanager
from datetime import datetime, timezone
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import signal
import stat
import subprocess
import sys
import tarfile
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "release_artifact", ROOT / "ops/ansible/files/release_artifact.py",
)
ARTIFACT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ARTIFACT)
BENCHMARK_STATE = Path("/run/simplestchat-bench")


class BuildError(RuntimeError):
    """A failed release step; the caller must inspect its retained evidence."""


def timestamp():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def write_json(path, value):
    with path.open("x", encoding="utf-8") as stream:
        json.dump(value, stream, indent=2, sort_keys=True)
        stream.write("\n")


class Runner:
    """Bound child process groups and retain each command's original output."""

    def __init__(self, output=None):
        self.output = output
        self.sequence = 0

    def run(self, argv, *, cwd, timeout=30, env=None, allow_failure=False, capture=True):
        self.sequence += 1
        name = f"{self.sequence:02d}-{Path(argv[0]).name}"
        if self.output:
            log_path = self.output / f"{name}.log"
            log = log_path.open("xb")
            write_json(self.output / f"{name}.command.json", {"argv": argv, "timeoutSeconds": timeout})
        else:
            log = tempfile.TemporaryFile()
        started = time.monotonic()
        process = None
        timed_out = False
        cleanup = None
        cleanup_error = None
        try:
            with log:
                process = subprocess.Popen(
                    argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                    stdout=log, stderr=subprocess.STDOUT, start_new_session=True,
                )
                try:
                    process.wait(timeout=timeout)
                except subprocess.TimeoutExpired:
                    timed_out = True
                    raise BuildError(f"Command timed out: {name}") from None
                if not capture:
                    data = b""
                elif self.output:
                    if log_path.stat().st_size > 2 * 1024 * 1024:
                        raise BuildError(f"Command inspection output too large: {name}")
                    data = log_path.read_bytes()
                else:
                    log.seek(0)
                    data = log.read(2 * 1024 * 1024 + 1)
                if process.returncode and not allow_failure:
                    raise BuildError(f"Command failed ({process.returncode}): {name}")
                if len(data) > 2 * 1024 * 1024:
                    raise BuildError(f"Command inspection output too large: {name}")
                return process.returncode, data.decode("utf-8", errors="strict").strip()
        finally:
            if process is not None and process.poll() is None:
                try:
                    cleanup = self._stop(process)
                except (OSError, subprocess.TimeoutExpired) as error:
                    cleanup_error = f"{type(error).__name__}: {error}"
            if self.output:
                write_json(self.output / f"{name}.outcome.json", {
                    "pid": process.pid if process else None,
                    "exitStatus": process.returncode if process else None,
                    "timedOut": timed_out, "elapsedSeconds": round(time.monotonic() - started, 3),
                    "cleanup": cleanup, "cleanupError": cleanup_error,
                })
            if cleanup_error:
                raise BuildError(f"Owned command cleanup is uncertain: {name}; inspect its outcome")

    @staticmethod
    def _stop(process):
        result = {"groupSignalDenied": False, "forcedKill": False}

        def send(signum):
            try:
                os.killpg(process.pid, signum)
            except ProcessLookupError:
                # The group may have exited between poll and signal. Popen
                # owns this exact child and rechecks its status before signaling.
                process.send_signal(signum)
            except PermissionError:
                result["groupSignalDenied"] = True
                process.send_signal(signum)

        # Do not use killpg(pid, 0) as a liveness probe: macOS can deny that
        # probe even when terminating/reaping our direct child is permitted.
        send(signal.SIGTERM)
        try:
            process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            result["forcedKill"] = True
            send(signal.SIGKILL)
            process.wait(timeout=5)
        return result


def clean_revision(runner, root):
    _, revision = runner.run(["git", "rev-parse", "--verify", "HEAD"], cwd=root)
    if not re.fullmatch(r"[a-f0-9]{40}", revision):
        raise BuildError("HEAD is not a full Git commit ID")
    _, dirty = runner.run(["git", "status", "--porcelain=v1", "--untracked-files=all"], cwd=root)
    if dirty:
        raise BuildError("Commit or remove checkout changes before building a release")
    return revision


def validate_output(value, root, runner):
    path = Path(value).expanduser().absolute()
    if any(ord(char) < 32 for char in str(path)) or "," in str(path):
        raise BuildError("Output path cannot contain control characters or commas")
    if path.exists() or path.is_symlink():
        raise BuildError("Output directory must not already exist")
    if not path.parent.is_dir() or path.parent.resolve() != path.parent:
        raise BuildError("Output parent must exist and contain no symlink components")
    path = path.parent.resolve() / path.name
    if path.is_relative_to(root):
        status, _ = runner.run(["git", "check-ignore", "--quiet", "--", str(path)], cwd=root, allow_failure=True)
        if status != 0:
            raise BuildError("Output inside the checkout must be Git-ignored")
    return path


@contextmanager
def benchmark_guard(state=BENCHMARK_STATE, release=Path('/srv/simplestchat-public/release-state.json')):
    """Honor the host automation lock without creating host state on a Mac."""
    lock = None
    try:
        if state.exists() or state.is_symlink():
            if state.is_symlink() or not state.is_dir() or state.stat().st_uid != 0 or state.stat().st_mode & 0o777 != 0o700:
                raise BuildError("Cannot verify private benchmark state ownership")
            path = state / "workload.lock"
            if path.is_symlink() or not path.is_file() or path.stat().st_uid != 0 or path.stat().st_mode & 0o777 != 0o600:
                raise BuildError("Cannot verify benchmark workload lock")
            lock = path.open("r+")
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError as error:
                raise BuildError("Another simplestChat build or benchmark is active") from error
            record = state / "current.json"
            if record.exists() or record.is_symlink():
                if record.is_symlink() or not record.is_file() or record.stat().st_uid != 0 or record.stat().st_mode & 0o777 != 0o600 or record.stat().st_size > 65536:
                    raise BuildError("Cannot verify benchmark cleanup record")
                values = json.loads(record.read_text())
                if not isinstance(values, dict) or type(values.get("schemaVersion")) is not int or values.get("schemaVersion") != 1 or values.get("finalized") is not True:
                    raise BuildError("Benchmark cleanup is unfinished")
        if release.exists() or release.is_symlink():
            metadata = release.lstat()
            parent = release.parent.lstat()
            if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != 0 or stat.S_IMODE(metadata.st_mode) != 0o600 \
                    or metadata.st_size > 16384 or not stat.S_ISDIR(parent.st_mode) \
                    or parent.st_uid != 0 or stat.S_IMODE(parent.st_mode) != 0o700:
                raise BuildError('Cannot verify private release state ownership')
            record = json.loads(release.read_text())
            if not isinstance(record, dict) or type(record.get('schemaVersion')) is not int \
                    or record.get('schemaVersion') != 1 or record.get('finalized') is not True:
                raise BuildError('Release cleanup is unfinished')
        yield
    finally:
        if lock:
            lock.close()


def docker_preflight(runner, root):
    env = dict(os.environ)
    endpoint = env.get("DOCKER_HOST") if not env.get("DOCKER_CONTEXT") else None
    if not endpoint:
        _, endpoint = runner.run(["docker", "context", "inspect", "--format", "{{.Endpoints.docker.Host}}"], cwd=root)
    if not re.fullmatch(r"unix:///[^\x00-\x20]+", endpoint):
        raise BuildError("Release builds require a local Unix-socket Docker endpoint")
    for name in ("DOCKER_CONTEXT", "DOCKER_HOST", "DOCKER_TLS_VERIFY", "DOCKER_CERT_PATH", "DOCKER_API_VERSION",
                 "BUILDX_BUILDER", "BUILDKIT_HOST"):
        env.pop(name, None)
    docker = ["docker", "--host", endpoint]
    _, save_help = runner.run(docker + ["image", "save", "--help"], cwd=root, env=env)
    if len(re.findall(r"^[ \t]+--platform(?:[ \t]|$)", save_help, re.MULTILINE)) != 1:
        raise BuildError("Docker CLI must support image save --platform before building a release")
    _, server_api = runner.run(docker + ["version", "--format", "{{.Server.APIVersion}}"], cwd=root, env=env)
    version = re.fullmatch(r"(0|[1-9][0-9]{0,2})\.(0|[1-9][0-9]{0,2})", server_api)
    if version is None:
        raise BuildError("Cannot validate the Docker server API version for platform-specific image export")
    if tuple(int(part) for part in version.groups()) < (1, 48):
        raise BuildError("Docker server API 1.48 or newer is required for image save --platform")
    for label in ("com.docker.compose.project=simplestchat-public", "simplestchat.benchmark.run"):
        _, running = runner.run(docker + ["ps", "--quiet", "--filter", f"label={label}"], cwd=root, env=env)
        if running:
            raise BuildError("Public chat or a private benchmark is running on this Docker endpoint")
    _, builder = runner.run(docker + ["buildx", "inspect", "default"], cwd=root, env=env)
    if re.findall(r"^Driver:\s+(\S+)\s*$", builder, re.MULTILINE) != ["docker"]:
        raise BuildError("The default Buildx builder must use the inspected local Docker driver")
    return docker, env


def unpack_source(archive, destination):
    """Restore Git modes independently of the caller's private umask."""
    destination.mkdir(mode=0o755)
    destination.chmod(0o755)
    names = set()
    with tarfile.open(archive, "r:") as contents:
        for member in contents:
            name = PurePosixPath(member.name)
            if name.is_absolute() or ".." in name.parts or str(name) in names or str(name) in ("", "."):
                raise BuildError("Invalid source archive path")
            names.add(str(name))
            target = destination / name
            if member.isdir():
                target.mkdir(mode=0o755)
                target.chmod(0o755)
            elif member.isfile():
                with contents.extractfile(member) as source, target.open("xb") as output:
                    shutil.copyfileobj(source, output)
                target.chmod(0o755 if member.mode & 0o111 else 0o644)
            else:
                raise BuildError("Source archive links and special files are unsupported")


def migration_checksums(context):
    migrations = {}
    for path in sorted((context / "migrations").glob("*.sql")):
        match = re.fullmatch(r"([0-9]+)_[A-Za-z0-9_]+\.sql", path.name)
        if match is None or not path.is_file() or path.is_symlink():
            raise BuildError("Invalid migration filename")
        version = str(int(match[1]))
        if version == "0" or int(version) > 2**63 - 1 or version in migrations:
            raise BuildError("Invalid or duplicate migration version")
        migrations[version] = hashlib.sha384(path.read_bytes()).hexdigest()
    if not 0 < len(migrations) <= 1000:
        raise BuildError("Source archive must contain 1–1000 migrations")
    return migrations


def build_release(output_value, timeout, *, root=ROOT, runner=None):
    if type(timeout) is not int or not 60 <= timeout <= 7200:
        raise BuildError("Build timeout must be between 60 and 7200 seconds")
    runner = runner or Runner()
    output = validate_output(output_value, root, runner)
    revision = clean_revision(runner, root)
    output.mkdir(mode=0o700)
    runner.output = output
    started = timestamp()
    passed = False
    error_text = None
    try:
        with benchmark_guard(), tempfile.TemporaryDirectory(prefix="simplestchat-release.") as temporary:
            docker, env = docker_preflight(runner, root)
            temporary = Path(temporary)
            source_archive = temporary / "source.tar"
            runner.run(["git", "archive", "--format=tar", f"--output={source_archive}", revision], cwd=root, capture=False)
            context = temporary / "source"
            unpack_source(source_archive, context)
            migrations = migration_checksums(context)
            inputs = {name: ARTIFACT.sha256_file(context / name) for name in (
                "Dockerfile", ".dockerignore", "Cargo.lock", "web/package-lock.json", "build/pip-constraints.txt",
            )}
            write_json(output / "source.json", {"revision": revision, "inputsSha256": inputs})
            tag = f"simplestchat-release/production:{revision}"
            deadline = time.monotonic() + timeout
            runner.run(docker + [
                "buildx", "build", "--builder", "default", "--platform", "linux/amd64",
                "--pull", "--progress", "plain", "--target", "production", "--provenance=false", "--sbom=false",
                "--label", f"org.opencontainers.image.revision={revision}", "--tag", tag,
                "--load", str(context),
            ], cwd=context, env=env, timeout=timeout, capture=False)
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise BuildError("Build/export deadline expired before image export")
            runner.run(docker + ["image", "save", "--platform", "linux/amd64", "--output", str(output / "image.tar"), tag],
                       cwd=root, env=env, timeout=min(remaining, 300), capture=False)
            if clean_revision(runner, root) != revision:
                raise BuildError("Checkout revision changed during release build")
            archive = output / "image.tar"
            manifest = {
                "schemaVersion": 1, "revision": revision, "platform": "linux/amd64",
                "archiveSha256": ARTIFACT.sha256_file(archive), "imageTag": tag,
                "migrations": migrations, "createdAt": timestamp(),
            }
            candidate = output / "release.candidate.json"
            write_json(candidate, manifest)
            ARTIFACT.verify_archive(archive, ARTIFACT.validate_manifest(candidate))
            os.link(candidate, output / "release.json")
            passed = True
    except BaseException as error:
        error_text = str(error) or type(error).__name__
        raise
    finally:
        write_json(output / "outcome.json", {
            "schemaVersion": 1, "revision": revision, "startedAt": started, "finishedAt": timestamp(),
            "passed": passed, "error": error_text,
        })
        print(f"Release evidence: {output} (passed={str(passed).lower()})", flush=True)
    return output


class Once(argparse.Action):
    def __call__(self, parser, namespace, values, option_string=None):
        seen = getattr(namespace, "_seen", set())
        if self.dest in seen:
            parser.error(f"duplicate option: {option_string}")
        seen.add(self.dest)
        namespace._seen = seen
        setattr(namespace, self.dest, values)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, action=Once, help="Fresh directory with an existing parent; use an ignored results/ child or a path outside the checkout")
    parser.add_argument("--timeout-seconds", type=int, default=3600, action=Once, help="Build/export deadline, 60–7200 seconds (default: 3600)")
    args = parser.parse_args()
    os.umask(0o077)

    def interrupted(signum, _frame):
        raise BuildError(f"Interrupted by signal {signum}")

    signal.signal(signal.SIGTERM, interrupted)
    try:
        build_release(args.output, args.timeout_seconds)
    except (BuildError, ARTIFACT.ArtifactError, OSError, ValueError, KeyboardInterrupt) as error:
        print(f"Release build failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
