"""Scan one immutable image's canonical archive in bounded socket-free containers.

Online database preparation has no artifact/source/credential mounts. Artifact
scanners use a separately trusted base, no network, a non-root identity, a read-only
root filesystem and explicit resource limits. The selected application is never
executed. Every owned container is removed by verified exact identity.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import re
import shutil
import signal
import stat
import sys
import threading
import time
import uuid
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field, replace
from pathlib import Path
from typing import TYPE_CHECKING

import security_archive
import security_elf
import security_image_cache
import security_image_policy as policy
import security_rpm_notices
import security_runtime
import security_secret_projection as projection
import security_secret_spans
import security_tools
from native_security import engine_prefix
from security_policy import read_exceptions

# isort: split
import release_artifact
import release_build
from release_json import JsonObject, JsonValue, array_value, decode_json, object_value, string_value

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping, Sequence
    from types import FrameType

    from security_policy import ExceptionRecord

ROOT = Path(__file__).resolve().parents[1]
TOOLS = ("syft", "grype", "gitleaks")
MAX_OUTPUT = 768 * 1024 * 1024
MAX_DB = 4 * 1024**3
MAX_FILES = 20000
MAX_SECRET_FILES = security_archive.MAX_MEMBERS + 1
MAX_SECRET_BYTES = 4 * 1024**3
SECRET_DISK_RESERVE = 256 * 1024**2
SCAN_SECONDS = 900
DEFAULT_SCRATCH_BYTES = 256 * 1024**2
SYFT_SCAN_SCRATCH_BYTES = 1024**3
MAX_SCANNERS = 8
MAX_COMMAND_LOG = 4 * 1024**2
MAX_EXIT_STATUS = 255
FINDINGS_EXIT = 10
LABEL = "simplestchat.security.image"
PLATFORMS = {"linux/amd64": "linux-x86_64", "linux/arm64": "linux-aarch64"}


def write(path: Path, value: JsonValue) -> None:
    """Publish exclusive private evidence, never overwrite a prior run."""
    security_archive.new_file(path, (json.dumps(value, indent=2, sort_keys=True) + "\n").encode())


def digest(path: Path) -> str:
    """Hash a regular artifact without unbounded in-memory copies."""
    policy.require(not path.is_symlink() and path.is_file(), "image_evidence_file")
    result = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(65536):
            result.update(chunk)
    return result.hexdigest()


def directory_size(path: Path, limit: int) -> bool:
    """Stop scanner writes when outputs exceed the count or byte budget."""
    total = count = 0
    for directory, directories, files in os.walk(path, followlinks=False):
        for name in [*directories, *files]:
            entry = Path(directory) / name
            metadata = entry.lstat()
            count += 1
            if not (stat.S_ISDIR(metadata.st_mode) or stat.S_ISREG(metadata.st_mode)):
                return False
            total += metadata.st_size if stat.S_ISREG(metadata.st_mode) else 0
            if count > MAX_FILES or total > limit:
                return False
    return True


def readable_tree(path: Path) -> None:
    """Expose only this private staging tree's regular bytes to a non-root scanner."""
    for directory, directories, files in os.walk(path, followlinks=False):
        for name in files:
            entry = Path(directory) / name
            policy.require(not entry.is_symlink(), "image_scanner_input_link")
            entry.chmod(0o444)
        for name in directories:
            entry = Path(directory) / name
            policy.require(not entry.is_symlink(), "image_scanner_input_link")
            entry.chmod(0o555)
    path.chmod(0o555)


def scanner_diagnostic(  # noqa: PLR0913 -- Explicit bounded lifecycle evidence inputs.
    tool: str,
    result: tuple[int, str] | None,
    owned: JsonObject | None,
    *,
    cleanup_verified: bool,
    scratch_bytes: int,
    command_log: Path | None,
) -> JsonObject:
    """Summarize fixed failure classes and private log hashes, never scanner text."""
    log: bytes | None = None
    outcome: JsonObject = {}
    if command_log is not None and command_log.exists():
        descriptor = os.open(command_log, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(descriptor, "rb") as source:
            metadata = os.fstat(source.fileno())
            policy.require(
                stat.S_ISREG(metadata.st_mode) and metadata.st_size <= MAX_COMMAND_LOG,
                "image_scanner_diagnostic_log",
            )
            log = source.read(MAX_COMMAND_LOG + 1)
            policy.require(len(log) == metadata.st_size, "image_scanner_diagnostic_log")
        outcome = object_value(policy.report(command_log.with_suffix(".outcome.json"), limit=65536))
    exit_status = result[0] if result is not None else outcome.get("exitStatus")
    if type(exit_status) is not int:
        exit_status = None
    container_exit = owned.get("exitStatus") if owned else None
    oom = owned.get("oomKilled") if owned else None
    valid_state = (
        type(container_exit) is int
        and 0 <= container_exit <= MAX_EXIT_STATUS
        and type(oom) is bool
        and (
            result is None
            or (
                owned is not None
                and owned.get("state") == "exited"
                and container_exit == exit_status
                and not (oom and exit_status == 0)
            )
        )
    )
    timed_out = outcome.get("timedOut") is True
    output_limited = outcome.get("outputLimited") is True
    storage_exhausted = log is not None and b"no space left on device" in log.lower()
    classification = (
        "timed_out"
        if timed_out
        else "output_limit"
        if output_limited
        else "out_of_memory"
        if oom is True
        else "storage_exhausted"
        if storage_exhausted
        else "controller_failed"
        if result is None
        else "container_state_invalid"
        if not valid_state
        else "cleanup_unverified"
        if not cleanup_verified
        else "completed"
        if exit_status == 0
        else "findings"
        if tool in {"grype", "gitleaks"} and exit_status == FINDINGS_EXIT
        else "scanner_failed"
    )
    return {
        "schemaVersion": 1,
        "tool": tool,
        "classification": classification,
        "exitStatus": exit_status,
        "containerExitStatus": container_exit if type(container_exit) is int else None,
        "oomKilled": oom if type(oom) is bool else None,
        "containerStateValid": valid_state,
        "cleanupVerified": cleanup_verified,
        "timedOut": timed_out,
        "outputLimited": output_limited,
        "storageExhausted": storage_exhausted,
        "logBytes": len(log) if log is not None else None,
        "logSha256": hashlib.sha256(log).hexdigest() if log is not None else None,
        "limits": {
            "temporaryBytes": scratch_bytes,
            "memoryBytes": 2 * 1024**3,
            "swapAdditionalBytes": 0,
            "cpus": 2,
            "pids": 128,
            "seconds": SCAN_SECONDS,
            "logBytes": MAX_COMMAND_LOG,
        },
    }


@dataclass
class Sandbox:
    """One isolated local engine and exact-owned ephemeral scanner transaction."""

    engine: list[str]
    env: Mapping[str, str]
    output: Path
    tools: Path
    base: str
    platform: str
    runner: release_build.Runner = field(default_factory=release_build.Runner)
    sequence: int = 0
    diagnostic_prefix: str = ""
    scanner_limit: int = MAX_SCANNERS
    cancelled: threading.Event = field(default_factory=threading.Event)
    database_cache: Path | None = None

    def command(
        self, argv: Sequence[str], *, timeout: int = 30, allow_failure: bool = False
    ) -> tuple[int, str]:
        """Bound controller output/process lifetime independently of container resources."""
        return self.runner.run(
            [*self.engine, *argv],
            cwd=ROOT,
            env=self.env,
            timeout=timeout,
            allow_failure=allow_failure,
        )

    def inspect_owned(self, name: str, owner: str) -> JsonObject:
        """Only inspect allowlisted lifecycle identities; never emit container environment."""
        id_field = ".ID" if Path(self.engine[0]).name == "podman" else ".Id"
        status, encoded = self.command(
            [
                "container",
                "inspect",
                "--format",
                '{"id":{{json '
                + id_field
                + '}},"image":{{json .Image}},"owner":'
                + '{{json (index .Config.Labels "'
                + LABEL
                + '")}},"state":{{json .State.Status}},'
                + '"exitStatus":{{json .State.ExitCode}},"oomKilled":{{json .State.OOMKilled}}}',
                name,
            ],
            allow_failure=True,
        )
        policy.require(status == 0, "image_container_identity_unavailable")
        item = object_value(decode_json(encoded))
        policy.require(
            item.get("owner") == owner
            and string_value(item["image"]).removeprefix("sha256:")
            == self.base.removeprefix("sha256:")
            and re.fullmatch(r"[a-f0-9]{64}", string_value(item["id"])),
            "image_container_identity_changed",
        )
        return item

    def run(  # noqa: PLR0913 -- Explicit sandbox mounts, output and network boundary.
        self,
        tool: str,
        arguments: Sequence[str],
        *,
        destination: Path,
        mounts: Mapping[str, Path] | None = None,
        online: bool = False,
        extra_env: Mapping[str, str] | None = None,
    ) -> tuple[int, str]:
        """Run one verified binary and remove only its exact labelled container on any exit."""
        policy.require(tool in TOOLS, "image_scanner_unknown")
        policy.require(
            not online or (tool == "grype" and list(arguments) == ["db", "update"] and not mounts),
            "image_online_artifact_mount",
        )
        policy.require(not self.cancelled.is_set(), "image_scan_cancelled")
        policy.require(self.sequence < self.scanner_limit, "image_scanner_count")
        self.sequence += 1
        owner = uuid.uuid4().hex
        name = "simplestchat-security-" + owner
        uid, gid = (os.getuid(), os.getgid()) if os.getuid() else (65532, 65532)
        destination.mkdir(mode=0o700)
        if os.getuid() == 0:
            os.chown(destination, uid, gid)
        if online and self.database_cache is not None:
            _ = security_image_cache.restore(self.database_cache, destination, uid, gid)
        limit = MAX_DB if online else MAX_OUTPUT
        scratch_bytes = (
            SYFT_SCAN_SCRATCH_BYTES
            if tool == "syft" and list(arguments[:2]) == ["scan", "docker-archive:/input/image.tar"]
            else DEFAULT_SCRATCH_BYTES
        )
        args = [
            "run",
            "--name",
            name,
            "--label",
            LABEL + "=" + owner,
            "--platform",
            self.platform,
            "--pull=never",
            "--network",
            "bridge" if online else "none",
            "--read-only",
            "--user",
            f"{uid}:{gid}",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--pids-limit=128",
            "--cpus=2",
            "--memory=2g",
            "--memory-swap=2g",
            "--ulimit",
            "nofile=1024:1024",
            "--tmpfs",
            f"/tmp:rw,noexec,nosuid,nodev,size={scratch_bytes},mode=1777",  # noqa: S108 -- Private container tmpfs.
            "--env",
            "HOME=/tmp",
            "--env",
            "XDG_CONFIG_HOME=/tmp/config",
            "--env",
            "LC_ALL=C",
            "--env",
            "SYFT_CHECK_FOR_APP_UPDATE=false",
            "--env",
            "GRYPE_CHECK_FOR_APP_UPDATE=false",
            "--env",
            "GRYPE_DB_AUTO_UPDATE=" + ("true" if online else "false"),
            "--env",
            "GRYPE_DB_VALIDATE_BY_HASH_ON_START=true",
            "--env",
            "GRYPE_DB_VALIDATE_AGE=true",
            "--env",
            "GRYPE_DB_MAX_ALLOWED_BUILT_AGE=120h",
            "--env",
            "GRYPE_EXTERNAL_SOURCES_ENABLE=false",
            "--env",
            "SYFT_ENRICH_ALL=false",
            "--mount",
            f"type=bind,src={self.tools},dst=/tools,readonly",
            "--mount",
            f"type=bind,src={destination},dst=/output",
        ]
        for key, value in sorted((extra_env or {}).items()):
            args += ["--env", key + "=" + value]
        for target, source in sorted((mounts or {}).items()):
            policy.require(target in {"/input", "/layers", "/db"}, "image_scanner_mount")
            args += ["--mount", f"type=bind,src={source},dst={target},readonly"]
        args += ["--entrypoint", "/tools/" + tool, self.base, *arguments]
        self.runner.healthy = lambda: (
            not self.cancelled.is_set() and directory_size(destination, limit)
        )
        command_log = (
            self.runner.output / f"{self.runner.sequence + 1:02d}-{Path(self.engine[0]).name}.log"
            if self.runner.output is not None
            else None
        )
        result = None
        started = time.monotonic()
        owned = None
        cleanup_verified = False
        try:
            result = self.command(args, timeout=SCAN_SECONDS, allow_failure=True)
            return result
        finally:
            self.runner.healthy = None
            try:
                owned = self.inspect_owned(name, owner)
                _ = self.command(["container", "rm", "--force", string_value(owned["id"])])
                _, remaining = self.command(
                    [
                        "ps",
                        "--all",
                        "--quiet",
                        "--no-trunc",
                        "--filter",
                        "id=" + string_value(owned["id"]),
                    ]
                )
                policy.require(not remaining, "image_container_cleanup_failed")
                cleanup_verified = True
                write(
                    self.output / f"container-{self.diagnostic_prefix}{self.sequence:02d}.json",
                    owned,
                )
            finally:
                diagnostic = scanner_diagnostic(
                    tool,
                    result,
                    owned,
                    cleanup_verified=cleanup_verified,
                    scratch_bytes=scratch_bytes,
                    command_log=command_log,
                )
                diagnostic["elapsedSeconds"] = round(time.monotonic() - started, 3)
                write(
                    self.output
                    / f"scanner-result-{self.diagnostic_prefix}{self.sequence:02d}.json",
                    diagnostic,
                )
            policy.require(diagnostic["containerStateValid"], "image_scanner_container_state")
            policy.require(directory_size(destination, limit), "image_scanner_output_limit")


def tool_bundle(directory: Path, output: Path, platform: str) -> Path:
    """Copy only receipt-verified executable bytes into a read-only scanner mount."""
    bundle = output / "scanner-tools"
    bundle.mkdir(mode=0o700)
    for name in TOOLS:
        source = security_tools.tool_path(name, directory, target_platform=PLATFORMS[platform])
        destination = bundle / name
        _ = shutil.copyfile(source, destination, follow_symlinks=False)
        destination.chmod(0o555)
    security_archive.new_file(bundle / "gitleaks.toml", b"[extend]\nuseDefault = true\n")
    security_archive.new_file(bundle / "empty.ignore", b"")
    (bundle / "gitleaks.toml").chmod(0o444)
    (bundle / "empty.ignore").chmod(0o444)
    bundle.chmod(0o555)
    return bundle


def prepare_sandbox(args: Options, output: Path, image_policy: JsonObject) -> Sandbox:
    """Use an explicit local engine and an empty registry-auth/config directory."""
    engine = engine_prefix(args.engine)
    config = output / "engine-config"
    config.mkdir(mode=0o700)
    write(config / "config.json", {})
    env = {key: os.environ[key] for key in ("PATH", "HOME", "XDG_RUNTIME_DIR") if key in os.environ}
    env.update(
        {
            "LC_ALL": "C",
            "LANG": "C",
            "DOCKER_CONFIG": str(config),
            "REGISTRY_AUTH_FILE": str(config / "config.json"),
        }
    )
    command_logs = output / "scanner-commands"
    command_logs.mkdir(mode=0o700)
    runner = release_build.Runner(output=command_logs)
    base = string_value(image_policy["scannerBase"])
    sandbox = Sandbox(
        engine,
        env,
        output,
        tool_bundle(args.tools_directory, output, args.platform),
        base,
        args.platform,
        runner,
        database_cache=args.database_cache,
    )
    _ = sandbox.command(["pull", "--platform", args.platform, base], timeout=300)
    _, identity = sandbox.command(["image", "inspect", "--format", "{{.Id}}", base])
    identity = "sha256:" + identity.removeprefix("sha256:")
    policy.require(re.fullmatch(r"sha256:[a-f0-9]{64}", identity), "image_scanner_base_identity")
    sandbox.base = identity
    return sandbox


def bind_archive(args: Options) -> JsonObject:
    """Require the same canonical export selected by the trusted producer, without re-saving."""
    policy.require(re.fullmatch(r"sha256:[a-f0-9]{64}", args.image_id), "immutable_image_required")
    outcome = object_value(policy.report(args.artifact_dir / "outcome.json", limit=65536))
    policy.require(
        outcome.get("passed") is True and outcome.get("exportedImageId") == args.image_id,
        "image_export_identity",
    )
    manifest = release_artifact.validate_manifest(args.artifact_dir / "release.json")
    policy.require(
        manifest["revision"] == outcome.get("revision") and manifest["platform"] == args.platform,
        "image_export_revision_platform",
    )
    return object_value(decode_json(json.dumps(manifest)))


def selected_image(sandbox: Sandbox, args: Options, tree: Path) -> JsonObject:
    """Compare actual local image filesystem identity with the authenticated archive."""
    _, encoded = sandbox.command(
        [
            "image",
            "inspect",
            "--format",
            '{"id":{{json .Id}},"layers":{{json .RootFS.Layers}},'
            + '"architecture":{{json .Architecture}}}',
            args.image_id,
        ]
    )
    value = object_value(decode_json(encoded))
    canonical_id = "sha256:" + string_value(value["id"]).removeprefix("sha256:")
    report = object_value(policy.report(tree / "report.json"))
    layers = [object_value(item)["diffId"] for item in array_value(report["layers"])]
    policy.require(
        canonical_id == args.image_id == report["imageId"]
        and "sha256:" + string_value(report["configSha256"]) == args.image_id
        and value["layers"] == layers
        and "linux/" + string_value(value["architecture"]) == args.platform,
        "image_selected_archive_mismatch",
    )
    value["archiveConfigSha256"] = report["configSha256"]
    return value


def prepare_database(sandbox: Sandbox) -> JsonObject:
    """Fetch a fresh database without artifact mounts and verify its status and bytes."""
    database = sandbox.output / "database"
    status, _ = sandbox.run(
        "grype",
        ["db", "update"],
        destination=database,
        online=True,
        extra_env={"GRYPE_DB_CACHE_DIR": "/output/cache"},
    )
    policy.require(status == 0, "image_database_update_failed")
    readable_tree(database)
    db_status_dir = sandbox.output / "database-status"
    status, text = sandbox.run(
        "grype",
        ["db", "status", "-o", "json"],
        destination=db_status_dir,
        mounts={"/db": database},
        extra_env={"GRYPE_DB_CACHE_DIR": "/db/cache"},
    )
    policy.require(status == 0, "image_database_status_failed")
    db_status = object_value(decode_json(text))
    policy.database_status(db_status, policy.load_policy(ROOT / "security/image-policy.json"))
    db_status["databaseSha256"] = digest(database / "cache/6/vulnerability.db")
    db_status["importReceiptSha256"] = digest(database / "cache/6/import.json")
    write(sandbox.output / "database-status.json", db_status)
    if sandbox.database_cache is not None:
        security_image_cache.save(sandbox.database_cache, database)
    return db_status


def scan_artifacts(
    sandbox: Sandbox, args: Options, tree: Path, native: JsonObject, elf: JsonObject
) -> tuple[Path, Path]:
    """Catalog and scan the current archive offline, independently of DB preparation."""
    # This mount contains only the exact archive, not the checkout or unrelated evidence.
    inputs = sandbox.output / "scanner-input"
    inputs.mkdir(mode=0o700)
    _ = shutil.copyfile(
        args.artifact_dir / "image.tar", inputs / "image.tar", follow_symlinks=False
    )
    policy.require(
        digest(inputs / "image.tar") == digest(args.artifact_dir / "image.tar"),
        "image_archive_copy_changed",
    )
    readable_tree(inputs)
    raw_sbom = sandbox.output / "sbom-raw"
    status, _ = sandbox.run(
        "syft",
        [
            "scan",
            "docker-archive:/input/image.tar",
            "-o",
            "syft-json=/output/sbom.syft.json",
        ],
        destination=raw_sbom,
        mounts={"/input": inputs},
    )
    policy.require(status == 0, "image_sbom_failed")
    sbom_dir = sandbox.output / "sbom"
    sbom_dir.mkdir(mode=0o700)
    enriched = policy.enrich_sbom(
        object_value(policy.report(raw_sbom / "sbom.syft.json")),
        native,
        elf,
        policy.load_policy(ROOT / "security/image-policy.json"),
    )
    write(sbom_dir / "sbom.syft.json", enriched)
    readable_tree(sbom_dir)
    status, _ = sandbox.run(
        "syft",
        ["convert", "/input/sbom.syft.json", "-o", "spdx-json=/output/sbom.spdx.json"],
        destination=sandbox.output / "spdx",
        mounts={"/input": sbom_dir},
    )
    policy.require(status == 0, "image_spdx_conversion_failed")
    secret_inputs = secret_bundle(tree / "layers", sandbox.output, cancelled=sandbox.cancelled)
    secret_dir = sandbox.output / "secrets"
    status, _ = sandbox.run(
        "gitleaks",
        secret_arguments(),
        destination=secret_dir,
        mounts={"/layers": secret_inputs},
    )
    policy.require(status in (0, FINDINGS_EXIT), "image_secret_scan_failed")
    return sbom_dir, secret_dir


def scan_phase[T](sandbox: Sandbox, name: str, operation: Callable[[], T]) -> T:
    """Retain safe timings and both independent failures, including cancellation."""
    started = time.monotonic()
    passed = False
    failure: str | None = None
    try:
        result = operation()
    except BaseException as error:
        failure = (
            str(error) if isinstance(error, security_tools.ToolError) else type(error).__name__
        )
        raise
    else:
        passed = True
        return result
    finally:
        write(
            sandbox.output / f"scanner-result-phase-{name}.json",
            {
                "schemaVersion": 1,
                "phase": name,
                "passed": passed,
                "failure": failure,
                "elapsedSeconds": round(time.monotonic() - started, 3),
                "cancelled": sandbox.cancelled.is_set(),
            },
        )


def scan_reports(
    sandbox: Sandbox, args: Options, tree: Path, native: JsonObject, elf: JsonObject
) -> tuple[Path, Path, Path, JsonObject]:
    """Overlap only independent checks and join before current-DB vulnerability matching."""
    # Reserve two invocations for database work, preserving the global ceiling
    # of eight, including the already completed detector self-test.
    sandbox.scanner_limit = MAX_SCANNERS - 2
    policy.require(sandbox.sequence <= sandbox.scanner_limit, "image_scanner_count")
    logs = sandbox.output / "database-commands"
    logs.mkdir(mode=0o700)
    database = replace(
        sandbox,
        runner=release_build.Runner(output=logs),
        sequence=0,
        diagnostic_prefix="database-",
        scanner_limit=2,
    )

    def cancel(_number: int, _frame: FrameType | None) -> None:
        sandbox.cancelled.set()

    previous = {number: signal.signal(number, cancel) for number in (signal.SIGINT, signal.SIGTERM)}
    try:
        with ThreadPoolExecutor(max_workers=2) as executor:
            database_future = executor.submit(
                scan_phase, database, "database", lambda: prepare_database(database)
            )
            artifact_future = executor.submit(
                scan_phase,
                sandbox,
                "artifacts",
                lambda: scan_artifacts(sandbox, args, tree, native, elf),
            )
            failed = False
            try:
                for future in (database_future, artifact_future):
                    try:
                        _ = future.result()
                    except (
                        OSError,
                        ValueError,
                        KeyError,
                        release_build.BuildError,
                        security_tools.ToolError,
                    ):
                        failed = True
            except BaseException:
                sandbox.cancelled.set()
                raise
        policy.require(not sandbox.cancelled.is_set(), "image_scan_cancelled")
        policy.require(not failed, "image_parallel_scan_failed")
    finally:
        for number, handler in previous.items():
            _ = signal.signal(number, handler)
    db_status = database_future.result()
    sbom_dir, secret_dir = artifact_future.result()
    grype_dir = sandbox.output / "vulnerabilities"
    status, _ = sandbox.run(
        "grype",
        ["sbom:/input/sbom.syft.json", "-o", "json", "--file", "/output/grype.json"],
        destination=grype_dir,
        mounts={"/input": sbom_dir, "/db": sandbox.output / "database"},
        extra_env={"GRYPE_DB_CACHE_DIR": "/db/cache"},
    )
    policy.require(status == 0, "image_vulnerability_scan_failed")
    return sbom_dir, grype_dir, secret_dir, db_status


def secret_arguments() -> list[str]:
    """Apply the same complete detector configuration to canary and artifact bytes."""
    return [
        "dir",
        "/layers",
        "--no-banner",
        "--redact=100",
        "--config=/tools/gitleaks.toml",
        "--gitleaks-ignore-path=/tools/empty.ignore",
        "--ignore-gitleaks-allow",
        "--max-decode-depth=3",
        "--report-format=json",
        "--report-path=/output/gitleaks.json",
        "--exit-code=10",
        "--max-target-megabytes=0",
    ]


def secret_selftest(sandbox: Sandbox) -> None:
    """Require the actual pinned detector to find a never-issued inert credential-shaped canary."""
    work = sandbox.output / "secret-canary"
    work.mkdir(mode=0o700)
    originals = work / "originals"
    originals.mkdir(mode=0o700)
    canary = (
        "ghp_"
        + base64.b64encode(hashlib.sha512(b"inert scanner coverage only").digest())
        .decode()
        .replace("+", "x")
        .replace("/", "y")[:36]
    )
    payload = ("TOKEN=" + canary + " # gitleaks:allow\n").encode()
    prefixes = {
        "canary-elf": b"\x7fELF" + bytes(64) + b"\n",
        "canary-iso": bytes(32769) + b"CD001\n",
        "canary-pdf": b"%PDF-1.7\n",
        "canary-text": b"",
    }
    for name, prefix in prefixes.items():
        security_archive.new_file(originals / name, prefix + payload)
    source = secret_bundle(originals, work)
    identity = object_value(policy.report(work / "secret-paths.json"))
    readable_tree(source)
    output = sandbox.output / "secret-canary-output"
    status, _ = sandbox.run(
        "gitleaks", secret_arguments(), destination=output, mounts={"/layers": source}
    )
    report = policy.report(output / "gitleaks.json")
    spans = security_secret_spans.collect(report, identity, source)
    verdict = policy.secret_verdict(report, [], identity, spans)
    findings = array_value(verdict["blocked"])
    policy.require(
        status == FINDINGS_EXIT
        and len(findings) == len(prefixes)
        and all(object_value(item)["rule"] == "github-pat" for item in findings)
        and {string_value(object_value(item)["path"]) for item in findings} == set(prefixes),
        "image_secret_detector_selftest",
    )
    policy.require(
        all(
            object_value(item).get("status") == "resolved"
            and object_value(item).get("spanSha256") == hashlib.sha256(canary.encode()).hexdigest()
            for item in array_value(spans["findings"])
        ),
        "image_secret_coordinate_selftest",
    )
    write(
        sandbox.output / "secret-selftest.json",
        {
            "passed": True,
            "rule": "github-pat",
            "formats": list(prefixes),
            "projectionFormat": projection.FORMAT,
            "toolLockSha256": digest(ROOT / "build/security-tools.lock.json"),
        },
    )


def secret_inventory(
    layers: Path, output: Path, *, cancelled: threading.Event | None = None
) -> list[tuple[Path, int]]:
    """Budget every regular input and fixed prefix before writing any projection."""
    sources: list[tuple[Path, int]] = []
    total = 0
    for source in layers.rglob("*"):
        policy.require(cancelled is None or not cancelled.is_set(), "image_scan_cancelled")
        metadata = source.lstat()
        if stat.S_ISDIR(metadata.st_mode):
            continue
        policy.require(stat.S_ISREG(metadata.st_mode), "image_secret_input_kind")
        policy.require(metadata.st_size <= security_archive.MAX_FILE, "image_secret_input_size")
        sources.append((source, metadata.st_size))
        total += metadata.st_size + len(projection.PREFIX)
        policy.require(len(sources) <= MAX_SECRET_FILES, "image_secret_input_count")
        policy.require(total <= MAX_SECRET_BYTES, "image_secret_projection_budget")
    policy.require(bool(sources), "image_secret_input_empty")
    policy.require(
        shutil.disk_usage(output).free >= total + SECRET_DISK_RESERVE,
        "image_secret_projection_disk",
    )
    return sorted(sources)


def secret_bundle(layers: Path, output: Path, *, cancelled: threading.Event | None = None) -> Path:
    """Scan bounded printable projections under neutral names, retaining original identity."""
    sources = secret_inventory(layers, output, cancelled=cancelled)
    inputs = output / "secret-input"
    inputs.mkdir(mode=0o700)
    paths: JsonObject = {}
    for index, (source, expected_bytes) in enumerate(sources):
        policy.require(cancelled is None or not cancelled.is_set(), "image_scan_cancelled")
        name = f"content-{index:06d}"
        view = projection.project(source, inputs / name, max_bytes=security_archive.MAX_FILE)
        policy.require(view.source_bytes == expected_bytes, "image_secret_input_changed")
        paths[name] = {
            "path": source.relative_to(layers).as_posix(),
            "sha256": view.source_sha256,
            "sourceBytes": view.source_bytes,
            "projectionSha256": view.projection_sha256,
            "projectionBytes": view.projection_bytes,
            "projectionFormat": view.format,
        }
    policy.require(bool(paths), "image_secret_input_empty")
    write(output / "secret-paths.json", paths)
    readable_tree(inputs)
    return inputs


def native_binding(tree: Path, elf: JsonObject) -> JsonObject:
    """Require build-time native provenance to describe these exact executable bytes."""
    path = security_elf.image_path(
        tree / "rootfs", "/usr/share/simplestchat/native-components.json"
    )
    native = object_value(policy.report(path, limit=8 * 1024 * 1024))
    binary = object_value(native["binary"])
    policy.require(
        binary.get("sha256") == elf["binarySha256"] and binary.get("size") == elf["binaryBytes"],
        "image_native_binary_mismatch",
    )
    policy.require(
        native["native_manifest_sha256"] == digest(ROOT / "vendor/native-components.json")
        and native["cargo_lock_sha256"] == digest(ROOT / "Cargo.lock"),
        "image_native_source_mismatch",
    )
    policy.require(
        bool(array_value(native["static_archives"]))
        and bool(array_value(native["wrap_components"]))
        and bool(object_value(native["registry_component"])),
        "image_native_inventory_missing",
    )
    policy.openssl_build_binding(
        native,
        object_value(
            object_value(policy.report(ROOT / "vendor/native-components.json"))["openssl"]
        ),
    )
    return native


def runtime_disposition(  # noqa: PLR0913 -- Explicit artifact and output bindings.
    *,
    tree: Path,
    elf: JsonObject,
    native: JsonObject,
    manifest: JsonObject,
    vulnerabilities: JsonObject,
    output: Path,
    outcome: JsonObject,
) -> JsonObject:
    """Publish the exact conditional disposition before any final passing verdict."""
    outcome["vulnerabilityFindings"] = vulnerabilities
    if security_runtime.affected_scopes(vulnerabilities):
        runtime = security_runtime.proof(
            tree=tree,
            elf=elf,
            native=native,
            manifest=manifest,
            root=ROOT,
            sbom_sha256=digest(output / "spdx/sbom.spdx.json"),
        )
        vex: JsonObject = {}
    else:
        runtime, vex = security_runtime.not_required(
            manifest, string_value(outcome["imageId"]), vulnerabilities
        )
    write(output / "runtime-proof.json", runtime)
    if runtime["required"] is True:
        vulnerabilities, vex = security_runtime.apply(
            vulnerabilities, runtime, output / "runtime-proof.json"
        )
    write(output / "vex.openvex.json", vex)
    outcome["vexRequired"] = runtime["required"]
    outcome["runtimeProofSha256"] = digest(output / "runtime-proof.json")
    outcome["vexSha256"] = digest(output / "vex.openvex.json")
    return vulnerabilities


def secret_checks(
    secret_dir: Path, output: Path, exceptions: Sequence[ExceptionRecord]
) -> JsonObject:
    """Authenticate current complete match regions before applying exact public-data reviews."""
    report = policy.report(secret_dir / "gitleaks.json")
    paths = object_value(policy.report(output / "secret-paths.json"))
    spans = security_secret_spans.collect(report, paths, output / "secret-input")
    verdict = policy.secret_verdict(report, exceptions, paths, spans)
    verdict["projectionSpans"] = spans
    return verdict


def execute(args: Options) -> bool:
    """Persist separate evidence and one honest final verdict, including failed checks."""
    manifest = bind_archive(args)
    image_policy = policy.load_policy(ROOT / "security/image-policy.json")
    exceptions = read_exceptions()
    args.output.mkdir(mode=0o700)
    output = args.output.resolve(strict=True)
    outcome: JsonObject = {
        "schemaVersion": 1,
        "passed": False,
        "imageId": args.image_id,
        "revision": manifest["revision"],
        "archiveSha256": manifest["archiveSha256"],
        "platform": args.platform,
        "toolLockSha256": digest(ROOT / "build/security-tools.lock.json"),
        "policySha256": digest(ROOT / "security/image-policy.json"),
        "exceptionsSha256": digest(ROOT / "security/exceptions.json"),
    }
    try:
        tree = output / "archive"
        runner = release_build.Runner(output=output)
        _ = runner.run(
            [
                sys.executable,
                str(ROOT / "build/security_archive.py"),
                "--archive",
                str(args.artifact_dir / "image.tar"),
                "--manifest",
                str(args.artifact_dir / "release.json"),
                "--output",
                str(tree),
            ],
            cwd=ROOT,
            timeout=SCAN_SECONDS,
            capture=False,
        )
        elf_path = output / "elf.json"
        _ = runner.run(
            [
                sys.executable,
                str(ROOT / "build/security_elf.py"),
                "--rootfs",
                str(tree / "rootfs"),
                "--platform",
                args.platform,
                "--output",
                str(elf_path),
            ],
            cwd=ROOT,
            timeout=120,
            capture=False,
        )
        elf = object_value(policy.report(elf_path))
        native = native_binding(tree, elf)
        write(output / "native.json", native)
        outcome["binarySha256"] = elf["binarySha256"]
        sandbox = prepare_sandbox(args, output, image_policy)
        outcome["selectedImage"] = selected_image(sandbox, args, tree)
        secret_selftest(sandbox)
        outcome["secretDetectorSelfTest"] = True
        outcome["secretCoverage"] = {
            "format": projection.FORMAT,
            "limitations": list(projection.LIMITATIONS),
            "maximumSourceFileBytes": security_archive.MAX_FILE,
            "maximumProjectionBytes": MAX_SECRET_BYTES,
            "maximumRegularFiles": MAX_SECRET_FILES,
            "diskReserveBytes": SECRET_DISK_RESERVE,
            "projectionPrefixBytes": len(projection.PREFIX),
        }
        sbom_dir, grype_dir, secret_dir, db_status = scan_reports(sandbox, args, tree, native, elf)
        policy.database_status(db_status, image_policy)
        packages = policy.inventory(
            object_value(policy.report(sbom_dir / "sbom.syft.json")), image_policy
        )
        notices = security_rpm_notices.collect(
            tree / "rootfs",
            packages,
            image_id=args.image_id,
            archive_sha256=string_value(manifest["archiveSha256"]),
            sbom_sha256=digest(output / "spdx/sbom.spdx.json"),
            revision=string_value(manifest["revision"]),
            platform=args.platform,
        )
        write(output / "runtime-license-evidence.json", notices)
        outcome["runtimeLicenseEvidenceSha256"] = digest(output / "runtime-license-evidence.json")
        policy.require(notices.get("passed") is True, "image_runtime_notice_integrity")
        grype = object_value(policy.report(grype_dir / "grype.json"))
        policy.vulnerability_database_binding(grype, db_status)
        outcome["runtimeRpms"] = policy.runtime_rpm_bindings(packages, elf, tree / "rootfs")
        vulnerabilities = policy.vulnerability_verdict(grype, exceptions, packages)
        vulnerabilities = runtime_disposition(
            tree=tree,
            elf=elf,
            native=native,
            manifest=manifest,
            vulnerabilities=vulnerabilities,
            output=output,
            outcome=outcome,
        )
        checks: JsonObject = {
            "vulnerabilities": vulnerabilities,
            "licenses": policy.license_verdict(packages, image_policy, exceptions),
            "secrets": secret_checks(secret_dir, output, exceptions),
        }
        write(output / "checks.json", checks)
        outcome["checks"] = checks
        outcome["secretPathMapSha256"] = digest(output / "secret-paths.json")
        outcome["databaseEvidenceSha256"] = digest(output / "database-status.json")
        outcome["sbomSha256"] = digest(output / "spdx/sbom.spdx.json")
        outcome["nativeSha256"] = digest(output / "native.json")
        outcome["elfSha256"] = digest(elf_path)
        outcome["nativeVulnerabilityCoverage"] = {
            "inventory": "Authenticated static build inputs included in SBOM",
            "matching": "No inferred CPE applicability for adapted or static components",
            "limit": "Package scanning does not prove native libraries are vulnerability-free",
        }
        outcome["passed"] = all(object_value(value)["passed"] is True for value in checks.values())
    except (
        ValueError,
        OSError,
        KeyError,
        release_build.BuildError,
        security_tools.ToolError,
    ) as error:
        outcome["error"] = (
            str(error) if isinstance(error, security_tools.ToolError) else type(error).__name__
        )
    finally:
        write(output / "outcome.json", outcome)
    return outcome["passed"] is True


@dataclass
class Options(argparse.Namespace):
    """Explicit current artifact selectors; neither tags nor implicit resaves are accepted."""

    image_id: str = ""
    artifact_dir: Path = Path()
    output: Path = Path()
    tools_directory: Path = Path()
    engine: str = "docker"
    platform: str = "linux/amd64"
    database_cache: Path | None = None


def main() -> int:
    """Return failed when preparation, evidence coverage, policy or cleanup fails."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("image_id")
    for name in ("artifact-dir", "output", "tools-directory"):
        _ = parser.add_argument("--" + name, type=Path, required=True)
    _ = parser.add_argument("--engine", choices=("docker", "podman"), default="docker")
    _ = parser.add_argument("--platform", choices=tuple(PLATFORMS), default="linux/amd64")
    _ = parser.add_argument("--database-cache", type=Path)
    args = parser.parse_args(namespace=Options())
    _ = os.umask(0o077)
    args.artifact_dir = args.artifact_dir.resolve(strict=True)
    try:
        passed = execute(args)
    except (ValueError, OSError, KeyError, release_build.BuildError, security_tools.ToolError):
        passed = False
    print(  # noqa: T201 -- Fixed CLI status.
        "Image security " + ("passed" if passed else "failed") + "; evidence: " + str(args.output)
    )
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
