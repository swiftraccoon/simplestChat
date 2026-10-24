"""Observe the unchanged whole release builder through finite inert tool fixtures.

Actual source/image archives, extraction, hashes, guards, manifest validation,
publication and failure finalizers remain in the reference path. This adapter
starts no external tool, Docker engine, host workload or network operation.
It does not establish process settlement, authenticity or runtime custody.
"""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import io
import json
import os
import re
import sys
import tarfile
import tempfile
import time
from contextlib import ExitStack, redirect_stdout
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, final
from unittest.mock import patch

import test_release_build as FIXTURE  # noqa: N812 -- Reuse unchanged reference fixtures.

# isort: split
import release_artifact as ARTIFACT  # noqa: N812 -- Preserve the reference alias.
import release_build as BUILD  # noqa: N812 -- Preserve the reference alias.
import release_fetch_receiver as RECEIVER  # noqa: N812 -- Preserve the reference alias.
from release_json import JsonObject, JsonValue, decode_json, object_value

if TYPE_CHECKING:
    from collections.abc import Mapping, Sequence

SCENARIOS = (
    "build-success",
    "export-missing-tag",
    "export-existing-tag",
    "invalid-selector",
    "invalid-output",
    "dirty-or-invalid-head",
    "guard-blocked",
    "remote-endpoint",
    "export-capability",
    "checked-env",
    "active-workload",
    "custom-builder",
    "probe-failure",
    "source-invalid",
    "migrations-invalid",
    "build-fails",
    "export-fails",
    "source-changes",
    "image-identity",
    "layer-metadata",
    "conflicting-tag",
    "tag-changes",
    "archive-wrong-layers",
    "archive-invalid",
    "receiver-boundary",
    "deadline-before-save",
)
MAX_OUTPUT = 1024 * 1024
MAX_FIXTURE_FILE = 2 * 1024 * 1024
MAX_FIXTURE_TOTAL = 64 * 1024 * 1024
STAMP = "2026-10-24T00:00:00Z"
SCRUBBED = (
    "DOCKER_CONTEXT",
    "DOCKER_HOST",
    "DOCKER_TLS_VERIFY",
    "DOCKER_CERT_PATH",
    "DOCKER_API_VERSION",
    "BUILDX_BUILDER",
    "BUILDKIT_HOST",
)


@dataclass(frozen=True)
class Case:
    """One closed scenario case; values are external fixture observations only."""

    name: str
    mutation: str = ""
    value: JsonValue = None
    export: bool = False


def cases(  # noqa: C901, PLR0911 -- Finite oracle case inventory.
    scenario: str,
) -> tuple[Case, ...]:
    """Return the complete fixed case inventory for one CLI-selected family."""
    direct = {
        "build-success": (Case("production"),),
        "export-missing-tag": (Case("missing", export=True),),
        "export-existing-tag": (Case("matching", "existing-tag", export=True),),
        "build-fails": (Case("build", "fail", "build"),),
        "export-fails": (
            Case("build-save", "fail", "export"),
            Case("export-save", "fail", "export", export=True),
            Case("selected-unavailable", "fail", "inspect", export=True),
        ),
        "conflicting-tag": (Case("conflict", "conflicting-tag", export=True),),
        "tag-changes": (
            Case("before-save", "tag-before", export=True),
            Case("after-save", "tag-after", export=True),
        ),
        "archive-wrong-layers": (Case("diff-ids", "fail", "archive-layers", export=True),),
        "receiver-boundary": (
            Case("build-unsigned", "receiver"),
            Case("export-unsigned", "receiver", export=True),
        ),
        "deadline-before-save": (Case("exhausted", "deadline"),),
        "dirty-or-invalid-head": (
            Case("dirty", "dirty"),
            Case("noncanonical-head", "invalid-head"),
        ),
        "remote-endpoint": (
            Case("ssh", "endpoint", "ssh://root@fixture.invalid"),
            Case("tcp", "endpoint", "tcp://fixture.invalid:2375"),
        ),
        "active-workload": (
            Case("public", "active-public"),
            Case("benchmark", "active-benchmark"),
        ),
        "custom-builder": (
            Case("build-refused", "custom-builder"),
            Case("export-accepted", "custom-builder", export=True),
        ),
        "probe-failure": (
            Case("cli", "fail", "save-help"),
            Case("server", "fail", "server-version"),
        ),
        "source-changes": (
            Case("head", "changed-head"),
            Case("dirty", "final-dirty"),
            Case("export-head", "changed-head", export=True),
        ),
    }
    if scenario in direct:
        return direct[scenario]
    if scenario == "invalid-selector":
        return tuple(
            Case(name, "timeout", value)
            for name, value in (
                ("zero", 0),
                ("below-min", 59),
                ("above-max", 7201),
                ("boolean", True),
                ("string", "120"),
            )
        ) + tuple(
            Case(name, "image-id", value)
            for name, value in (
                ("mutable", "simplestchat-ci:production"),
                ("empty", ""),
                ("short", "sha256:" + "b" * 63),
                ("uppercase", "sha256:" + "B" * 64),
                ("image-boolean", True),
                ("image-number", 123),
            )
        )
    if scenario == "invalid-output":
        return tuple(
            Case(name, "output", name)
            for name in (
                "existing",
                "nonignored",
                "comma",
                "control",
                "missing-parent",
                "symlink-parent",
            )
        )
    if scenario == "guard-blocked":
        return tuple(
            Case(name, "guard", name)
            for name in (
                "active-lock",
                "unfinished-benchmark",
                "boolean-schema",
                "bad-mode",
                "symlink-lock",
                "unfinished-release",
                "release-bad-mode",
            )
        )
    if scenario == "export-capability":
        return tuple(
            Case(name, "save-help", text)
            for name, text in (
                ("empty", ""),
                ("missing", "Options:\n  -o, --output string\n"),
                ("prose", "Description mentions --platform only"),
                ("prefix", "  --platform-other string\n"),
                ("duplicate", "  --platform string\n  --platform string\n"),
            )
        ) + tuple(
            Case("api-" + str(index), "api", value)
            for index, value in enumerate(
                (
                    "1.47",
                    "1.9",
                    "0.99",
                    "",
                    "1",
                    "1.48.0",
                    "01.48",
                    "1.048",
                    "1.48-dev",
                    "1.48\n1.49",
                    " 1.48",
                    "1.48 ",
                    "1.٤٨",
                    "1.-48",
                    "1000.0",
                    "1.1000",
                )
            )
        )
    if scenario == "checked-env":
        return tuple(
            Case(value, "checked-env", value) for value in ("1.48", "1.49", "1.100", "2.0")
        )
    if scenario == "source-invalid":
        return tuple(
            Case(name, "source", name)
            for name in (
                "traversal",
                "absolute",
                "duplicate",
                "symlink",
                "special",
            )
        )
    if scenario == "migrations-invalid":
        return tuple(
            Case(name, "migrations", name)
            for name in (
                "missing",
                "name",
                "zero",
                "duplicate",
                "overflow",
                "too-many",
            )
        )
    if scenario == "image-identity":
        image_cases: tuple[tuple[str, JsonValue], ...] = (
            ("id", {"id": "sha256:" + "c" * 64}),
            ("os", {"os": "windows"}),
            ("architecture", {"architecture": "arm64"}),
            ("user", {"user": "root"}),
            ("labels-null", {"labels": None}),
            ("labels-empty", {"labels": {}}),
            ("revision", {"labels": {"org.opencontainers.image.revision": "c" * 40}}),
            ("cmd", {"cmd": ["/app/load_test"]}),
            ("entrypoint", {"entrypoint": ["/bin/sh"]}),
        )
        return tuple(Case(name, "image", value, export=True) for name, value in image_cases)
    if scenario == "layer-metadata":
        many_layers: list[JsonValue] = [FIXTURE.LAYER_ID for _ in range(1001)]
        layer_values: tuple[JsonValue, ...] = (
            None,
            {},
            {"Type": "layers", "Layers": []},
            {"Type": "other", "Layers": [FIXTURE.LAYER_ID]},
            {"Type": "layers", "Layers": ["sha256:wrong"]},
            {"Type": "layers", "Layers": many_layers},
        )
        return tuple(
            Case("layers-" + str(index), "rootfs", value, export=True)
            for index, value in enumerate(layer_values)
        )
    if scenario == "archive-invalid":
        return tuple(
            Case(name, "archive", name)
            for name in (
                "revision",
                "user",
                "architecture",
                "tags",
                "multiple",
                "traversal",
                "duplicate",
                "oci-digest",
                "oci-size",
                "oci-ref",
            )
        )
    raise ValueError("unknown_release_build_scenario")  # noqa: EM101


def action(argv: Sequence[str]) -> str:  # noqa: PLR0911 -- Fixed external role projection.
    """Name only fixed Git and Docker roles accepted by the reference fixture."""
    if argv[0] == "git":
        return {
            "check-ignore": "git-check-ignore",
            "rev-parse": "git-head",
            "status": "git-status",
            "archive": "git-archive",
        }[argv[1]]
    if argv[1] == "context":
        return "docker-context"
    command = list(argv[3:])
    if command == ["image", "save", "--help"]:
        return "docker-save-help"
    if command[0] == "version":
        return "docker-api"
    if command[0] == "ps":
        return "docker-public" if "simplestchat-public" in command[-1] else "docker-benchmark"
    if command[:2] == ["buildx", "inspect"]:
        return "docker-builder"
    if command[:2] == ["buildx", "build"]:
        return "docker-build"
    if command[:2] == ["image", "inspect"]:
        return "docker-inspect-tag" if command[3] == "{{.Id}}" else "docker-inspect-image"
    return {"ls": "docker-tag-list", "tag": "docker-tag", "save": "docker-save"}[command[1]]


@dataclass
class Clock:
    """Supply only the reference's external monotonic observation."""

    now: float = 0

    def monotonic(self) -> float:
        """Return the original finite fixture instant without sleeping."""
        return self.now


@final
class Runner:
    """Delegate real fixture archives and tool observations to the unchanged adapter."""

    def __init__(self, case: Case, clock: Clock) -> None:
        """Select one finite mutation before the actual whole reference starts."""
        self.case = case
        self.clock = clock
        self.status_reads = 0
        self.output: Path | None = None
        self.inner = FIXTURE.FakeRunner(
            dirty=case.mutation == "dirty",
            ignored=not (case.mutation == "output" and case.value == "nonignored"),
            fail=case.value if case.mutation == "fail" and isinstance(case.value, str) else None,
            builder="remote" if case.mutation == "custom-builder" else "docker",
            final_revision="c" * 40 if case.mutation == "changed-head" else FIXTURE.REVISION,
            save_help=case.value
            if case.mutation == "save-help" and isinstance(case.value, str)
            else "Options:\n      --platform string   Export a specific platform\n",
            server_api=case.value
            if case.mutation in ("api", "checked-env") and isinstance(case.value, str)
            else "1.48",
            existing_tag=FIXTURE.IMAGE_ID
            if case.mutation == "existing-tag"
            else "sha256:" + "c" * 64
            if case.mutation == "conflicting-tag"
            else "",
            selected_images=["sha256:" + "c" * 64]
            if case.mutation == "tag-before"
            else [FIXTURE.IMAGE_ID, "sha256:" + "c" * 64]
            if case.mutation == "tag-after"
            else None,
        )
        if case.mutation == "image":
            self.inner.image.update(object_value(case.value))
        if case.mutation == "rootfs":
            self.inner.image["rootfs"] = case.value

    def run(  # noqa: PLR0913 -- Match the actual explicit subprocess boundary.
        self,
        argv: Sequence[str],
        *,
        cwd: Path,
        timeout: float = 30,
        env: Mapping[str, str] | None = None,
        allow_failure: bool = False,
        capture: bool = True,
    ) -> tuple[int, str]:
        """Supply an external observation while actual operation policy stays in path."""
        self.inner.output = self.output
        result = self.inner.run(
            argv, cwd=cwd, timeout=timeout, env=env, allow_failure=allow_failure, capture=capture
        )
        role = action(argv)
        if role == "git-head" and self.case.mutation == "invalid-head":
            return 0, "main"
        if role == "git-status":
            self.status_reads += 1
            if self.case.mutation == "final-dirty" and self.status_reads > 1:
                return 0, " M src/main.rs"
        if (
            role in ("docker-public", "docker-benchmark")
            and self.case.mutation == "active-" + role[7:]
        ):
            return 0, "fixture-active-container"
        if role == "git-archive" and self.case.mutation in ("source", "migrations"):
            archive = Path(
                next(value.split("=", 1)[1] for value in argv if value.startswith("--output="))
            )
            mutate_source(archive, self.case)
        if role == "docker-build" and self.case.mutation == "deadline":
            self.clock.now = 120
        if role == "docker-save" and self.case.mutation == "archive":
            archive = Path(argv[list(argv).index("--output") + 1])
            mutate_archive(archive, self.case.name)
        return result


def mutate_source(path: Path, case: Case) -> None:
    """Construct a real inert source tar with one selected invalid external member."""
    entries: list[tuple[tarfile.TarInfo, bytes]] = []
    with tarfile.open(path, "r:") as archive:
        for member in archive:
            if case.mutation == "migrations" and member.name.startswith("migrations/"):
                continue
            source = archive.extractfile(member) if member.isfile() else None
            if source is None:
                entries.append((member, b""))
            else:
                with source:
                    entries.append((member, source.read()))
    with tarfile.open(path, "w") as archive:
        for member, data in entries:
            archive.addfile(member, io.BytesIO(data) if member.isfile() else None)
        if case.mutation == "source":
            if case.name in ("traversal", "absolute", "duplicate"):
                name = {"traversal": "../escape", "absolute": "/escape", "duplicate": "Dockerfile"}[
                    case.name
                ]
                FIXTURE.add_file(archive, name, b"invalid")
            else:
                member = tarfile.TarInfo("invalid-member")
                member.type = tarfile.SYMTYPE if case.name == "symlink" else tarfile.FIFOTYPE
                member.linkname = "/escape" if case.name == "symlink" else ""
                archive.addfile(member)
        else:
            names = {
                "missing": [],
                "name": ["bad.sql"],
                "zero": ["0_bad.sql"],
                "duplicate": ["1_one.sql", "01_two.sql"],
                "overflow": [str(2**63) + "_bad.sql"],
                "too-many": [str(index) + "_fixture.sql" for index in range(1, 1002)],
            }[case.name]
            for name in names:
                FIXTURE.add_file(archive, "migrations/" + name, b"SELECT 1;\n")


def mutate_archive(path: Path, name: str) -> None:
    """Emit real invalid Docker/OCI bytes, leaving verification to the reference."""
    if name in ("revision", "user", "architecture", "tags"):
        FIXTURE.image_archive(
            path,
            revision="c" * 40 if name == "revision" else FIXTURE.REVISION,
            user="root" if name == "user" else "10001:10001",
            architecture="arm64" if name == "architecture" else "amd64",
            tags=[FIXTURE.TAG, "unrelated:latest"] if name == "tags" else None,
        )
    elif name == "multiple":
        FIXTURE.image_archive(path, image_count=2)
    elif name in ("traversal", "duplicate"):
        FIXTURE.image_archive(
            path,
            extra=lambda archive: FIXTURE.add_file(
                archive, "../escape" if name == "traversal" else "manifest.json", b"[]"
            ),
        )
    else:

        def change(index: JsonObject) -> None:
            manifests = index["manifests"]
            if not isinstance(manifests, list):
                raise TypeError("fixture_index")  # noqa: EM101
            manifest = object_value(manifests[0])
            if name == "oci-digest":
                manifest["digest"] = "sha256:" + "b" * 64
            elif name == "oci-size":
                manifest["size"] = 1
            else:
                object_value(manifest["annotations"])["io.containerd.image.name"] = "other:latest"

        FIXTURE.image_archive(path, modern=True, index_change=change)


def read_record(path: Path) -> JsonObject | None:
    """Read an actual retained reference record if this transition created it."""
    return object_value(decode_json(path.read_text())) if path.is_file() else None


def normalize(value: str, base: Path) -> str:
    """Replace only the private fixture root, preserving all other observed bytes."""
    selected = value.replace(str(base), "$FIXTURE")
    return re.sub(r"(/temporary-[0-9]+/simplestchat-release\.)[^/ '\"]+", r"\1$TEMP", selected)


def command_records(runner: Runner, base: Path) -> list[JsonValue]:
    """Project actual selected argv/options, without captured stdout authorization."""
    result: list[JsonValue] = []
    for argv, options in runner.inner.calls:
        environment = options["env"]
        kept: JsonObject | None = (
            None
            if environment is None
            else {
                name: value
                for name, value in environment.items()
                if name in (*SCRUBBED, "FIXTURE_ENV")
            }
        )
        record: JsonObject = {
            "argv": [normalize(value, base) for value in argv],
            "cwd": normalize(str(options["cwd"]), base),
            "timeoutSeconds": options["timeout"],
            "envOverrides": kept,
            "allowFailure": options["allow_failure"],
            "capture": options["capture"],
        }
        result.append(record)
    return result


def setup_guard(base: Path, name: str, resources: ExitStack) -> tuple[Path, Path]:
    """Create original private guard facts, retaining the real nonblocking flock."""
    runtime = base / "runtime"
    release = base / "persistent" / "release-state.json"
    if name in ("unfinished-release", "release-bad-mode"):
        release.parent.mkdir(mode=0o700)
        finalized = "true" if name == "release-bad-mode" else "false"
        _ = release.write_text('{"schemaVersion":1,"finalized":' + finalized + "}")
        release.chmod(0o644 if name == "release-bad-mode" else 0o600)
    else:
        runtime.mkdir(mode=0o755 if name == "bad-mode" else 0o700)
        lock = runtime / "workload.lock"
        lock.touch(mode=0o600)
        if name == "symlink-lock":
            lock.unlink()
            lock.symlink_to(base / "absent-lock")
        elif name == "active-lock":
            stream = resources.enter_context(lock.open("r+"))
            fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
        else:
            record = runtime / "current.json"
            _ = record.write_text(
                '{"schemaVersion":'
                + ("true" if name == "boolean-schema" else "1")
                + ',"finalized":'
                + ("true" if name in ("boolean-schema", "bad-mode") else "false")
                + "}"
            )
            record.chmod(0o600)
    return runtime, release


def observe(  # noqa: C901, PLR0912, PLR0915 -- Own the complete reference transaction.
    case: Case,
    base: Path,
) -> JsonObject:
    """Call unchanged actual build_release and retain its whole operation observations."""
    root = base / "checkout"
    root.mkdir(mode=0o700)
    output = root / "release"
    clock = Clock()
    runner = Runner(case, clock)
    temporary_paths: list[Path] = []
    original_temporary = tempfile.TemporaryDirectory
    original_guard = BUILD.benchmark_guard
    original_stat, original_lstat = Path.stat, Path.lstat
    runtime, release = base / "runtime", base / "persistent/release-state.json"
    environment: dict[str, str] = {}
    if case.mutation == "endpoint" and isinstance(case.value, str):
        environment["DOCKER_HOST"] = case.value
    if case.mutation == "checked-env":
        environment.update(dict.fromkeys(SCRUBBED, "ignored-override"))
        environment["DOCKER_HOST"] = "unix:///var/run/docker.sock"
        environment["FIXTURE_ENV"] = "preserved"
    if case.mutation == "output":
        if case.name == "existing":
            output.mkdir(mode=0o700)
        elif case.name == "comma":
            output = root / "bad,name"
        elif case.name == "control":
            output = root / "bad\nname"
        elif case.name == "missing-parent":
            output = root / "missing/child"
        elif case.name == "symlink-parent":
            (root / "alias").symlink_to(root, target_is_directory=True)
            output = root / "alias/child"
    existed = output.exists()
    timeout = case.value if case.mutation == "timeout" else 120
    image_id = (
        case.value if case.mutation == "image-id" else FIXTURE.IMAGE_ID if case.export else None
    )

    def temporary(*, prefix: str) -> tempfile.TemporaryDirectory[str]:
        path = base / ("temporary-" + str(len(temporary_paths)))
        path.mkdir(mode=0o700)
        temporary_paths.append(path)
        return original_temporary(prefix=prefix, dir=path)

    def owned_stat(path: Path, *, follow_symlinks: bool = True) -> os.stat_result:
        values = list(original_stat(path, follow_symlinks=follow_symlinks))
        if path.is_relative_to(runtime) or path.is_relative_to(release.parent):
            values[4] = 0
        return os.stat_result(values)

    def owned_lstat(path: Path) -> os.stat_result:
        values = list(original_lstat(path))
        if path.is_relative_to(runtime) or path.is_relative_to(release.parent):
            values[4] = 0
        return os.stat_result(values)

    error_text: str | None = None
    passed = False
    receiver_rejected: bool | None = None
    with (
        ExitStack() as resources,
        patch.object(BUILD, "benchmark_guard", lambda: original_guard(runtime, release)),
        patch.object(tempfile, "TemporaryDirectory", temporary),
        patch.dict(os.environ, environment, clear=True),
    ):
        if case.mutation == "guard":
            runtime, release = setup_guard(base, case.name, resources)
        _ = resources.enter_context(patch.object(BUILD, "timestamp", return_value=STAMP))
        _ = resources.enter_context(patch.object(time, "monotonic", clock.monotonic))
        _ = resources.enter_context(patch.object(Path, "stat", owned_stat))
        _ = resources.enter_context(patch.object(Path, "lstat", owned_lstat))
        _ = resources.enter_context(redirect_stdout(io.StringIO()))
        try:
            _ = BUILD.build_release(output, timeout, root=root, runner=runner, image_id=image_id)
            passed = True
        except (BUILD.BuildError, ARTIFACT.ArtifactError, OSError, ValueError) as error:
            error_text = normalize(str(error), base)
        source = read_record(output / "source.json")
        if passed and case.mutation == "receiver":
            receiver_rejected = False
            try:
                _ = RECEIVER.validate_build_evidence(output, {"revision": FIXTURE.REVISION})
            except KeyError:
                receiver_rejected = True
            if source is None:
                raise TypeError("fixture_source")  # noqa: EM101
            source["exportedImageId"] = FIXTURE.IMAGE_ID
            _ = (output / "source.json").write_text(json.dumps(source))
            _ = source.pop("exportedImageId")
            try:
                _ = RECEIVER.validate_build_evidence(output, {"revision": FIXTURE.REVISION})
            except RECEIVER.FetchError:
                pass
            else:
                receiver_rejected = False
    outcome = read_record(output / "outcome.json")
    if outcome is not None and isinstance(outcome.get("error"), str):
        outcome["error"] = normalize(str(outcome["error"]), base)
    actions: list[JsonValue] = [action(argv) for argv, _ in runner.inner.calls]
    files: list[JsonValue] = []
    if output.is_dir():
        files.extend(sorted(path.name for path in output.iterdir()))
    return {
        "name": case.name,
        "fixture": {"mutation": case.mutation, "value": case.value, "export": case.export},
        "passed": passed,
        "published": (output / "release.json").is_file(),
        "outputCreated": not existed and output.is_dir(),
        "error": error_text,
        "actions": actions,
        "commands": command_records(runner, base),
        "retainedFiles": files,
        "source": source,
        "manifest": read_record(output / "release.json"),
        "outcome": outcome,
        "temporaryClean": all(not any(path.iterdir()) for path in temporary_paths),
        "receiverRejected": receiver_rejected,
    }


def fixture_pin(path: Path, root: Path) -> JsonObject:
    """Pin only independently generated fixed fixture bytes, never candidate output."""
    size = path.stat().st_size
    if size > MAX_FIXTURE_FILE:
        raise ValueError("release_fixture_file_limit")  # noqa: EM101
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        while chunk := stream.read(65536):
            digest.update(chunk)
    return {"path": str(path.relative_to(root)), "sha256": digest.hexdigest(), "bytes": size}


def export_fixtures(scenario: str, destination: Path) -> list[JsonObject]:
    """Create exact finite external input archives in one fresh private test directory."""
    selected = cases(scenario)
    root = destination.absolute()
    if root.exists() or root.is_symlink() or root.parent.resolve() != root.parent:
        raise ValueError("release_fixture_directory_rejected")  # noqa: EM101
    root.mkdir(mode=0o700)
    total = 0
    result: list[JsonObject] = []
    for index, case in enumerate(selected):
        directory = root / str(index)
        directory.mkdir(mode=0o700)
        source = directory / "source.tar"
        fixture = FIXTURE.FakeRunner()
        _ = fixture.run(
            ["git", "archive", "--format=tar", "--output=" + str(source), FIXTURE.REVISION],
            cwd=directory,
            capture=False,
        )
        if case.mutation in ("source", "migrations"):
            mutate_source(source, case)
        image = directory / "image.tar"
        FIXTURE.image_archive(
            image,
            diff_ids=["sha256:" + "e" * 64]
            if case.mutation == "fail" and case.value == "archive-layers"
            else None,
        )
        if case.mutation == "archive":
            mutate_archive(image, case.name)
        pins: JsonObject = {"source": fixture_pin(source, root), "image": fixture_pin(image, root)}
        total += source.stat().st_size + image.stat().st_size
        if total > MAX_FIXTURE_TOTAL:
            raise ValueError("release_fixture_total_limit")  # noqa: EM101
        result.append(pins)
    return result


def run_scenario(scenario: str, fixture_directory: Path | None = None) -> JsonObject:
    """Run every closed case in one family, creating only private inert fixture files."""
    selected = cases(scenario)
    files = export_fixtures(scenario, fixture_directory) if fixture_directory is not None else None
    observations: list[JsonValue] = []
    with tempfile.TemporaryDirectory(prefix="simplestchat-release-oracle.") as value:
        base = Path(value).resolve()
        for index, case in enumerate(selected):
            directory = base / str(index)
            directory.mkdir(mode=0o700)
            record = observe(case, directory)
            record["fixtureFiles"] = files[index] if files is not None else None
            observations.append(record)
    return {"schemaVersion": 1, "scenario": scenario, "cases": observations}


@dataclass
class Options(argparse.Namespace):
    """Keep the single closed CLI selector typed after argparse validation."""

    scenario: str = ""
    fixture_directory: str | None = None


def main(arguments: Sequence[str] | None = None) -> int:
    """Emit one bounded ordinary JSON result from the fixed scenario CLI."""
    parser = argparse.ArgumentParser(description=__doc__, allow_abbrev=False)
    _ = parser.add_argument("--scenario", required=True, choices=SCENARIOS)
    _ = parser.add_argument(
        "--fixture-directory", help="Fresh private directory for fixed input tars"
    )
    args = parser.parse_args(arguments, namespace=Options())
    directory = Path(args.fixture_directory) if args.fixture_directory is not None else None
    output = json.dumps(run_scenario(args.scenario, directory), sort_keys=True, allow_nan=False)
    if len(output.encode()) > MAX_OUTPUT:
        raise ValueError("release_build_oracle_output_limit")  # noqa: EM101
    print(output)  # noqa: T201 -- The closed oracle CLI returns its original JSON record.
    return 0


if __name__ == "__main__":
    sys.exit(main())
