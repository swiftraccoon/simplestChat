"""Offline production-release behavior; never contacts Docker or a remote host."""

# Fixture failures intentionally explain the violated command contract.
# ruff: noqa: EM101, EM102, TRY003

from __future__ import annotations

import hashlib
import io
import json
import os
import signal
import subprocess
import sys
import tarfile
import tempfile
import unittest
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, TypedDict, final, override
from unittest.mock import patch

from test_support import ROOT

# Bootstrap flat checkout imports before loading helpers.
# isort: split
import release_artifact as ARTIFACT  # noqa: N812 -- Keep established helper aliases.
import release_build as BUILD  # noqa: N812 -- Keep established helper aliases.
import release_fetch_receiver as RECEIVER  # noqa: N812 -- Keep established helper aliases.
from release_json import JsonObject, JsonValue, array_value, decode_json, json_value, object_value

if TYPE_CHECKING:
    from collections.abc import Callable, Iterator, Mapping, Sequence

REVISION = "a" * 40
TAG = f"simplestchat-release/production:{REVISION}"
IMAGE_ID = "sha256:" + "b" * 64
LAYER_ID = "sha256:" + "d" * 64


def add_file(archive: tarfile.TarFile, name: str, data: bytes, mode: int = 0o644) -> None:
    """Add an inert file with controlled metadata to a fixture archive."""
    member = tarfile.TarInfo(name)
    member.size = len(data)
    member.mode = mode
    archive.addfile(member, io.BytesIO(data))


def image_archive(  # noqa: PLR0913 -- Explicit boundary options preserve the subprocess contract.
    path: Path,
    *,
    revision: str = REVISION,
    tags: list[str] | None = None,
    user: str = "10001:10001",
    architecture: str = "amd64",
    extra: Callable[[tarfile.TarFile], None] | None = None,
    modern: bool = False,
    index_change: Callable[[JsonObject], None] | None = None,
    image_count: int = 1,
    diff_ids: list[str] | None = None,
) -> None:
    """Create a synthetic Docker or OCI archive for identity-validation cases."""
    config = {
        "architecture": architecture,
        "os": "linux",
        "rootfs": {"type": "layers", "diff_ids": diff_ids or [LAYER_ID]},
        "config": {
            "User": user,
            "Cmd": ["/app/simplestChat"],
            "Entrypoint": None,
            "Labels": {"org.opencontainers.image.revision": revision},
        },
    }
    with tarfile.open(path, "w") as archive:
        config_data = json.dumps(config).encode()
        layer_data = b"fixture-layer-not-executed"
        config_path = "config.json"
        layer_path = "layer.tar"
        if modern:
            config_path = "blobs/sha256/" + hashlib.sha256(config_data).hexdigest()
            layer_path = "blobs/sha256/" + hashlib.sha256(layer_data).hexdigest()

            def descriptor(data: bytes) -> JsonObject:
                return {"digest": "sha256:" + hashlib.sha256(data).hexdigest(), "size": len(data)}

            oci_data = json.dumps(
                {
                    "schemaVersion": 2,
                    "config": descriptor(config_data),
                    "layers": [descriptor(layer_data)],
                }
            ).encode()
            oci_path = "blobs/sha256/" + hashlib.sha256(oci_data).hexdigest()
            index: JsonObject = {
                "schemaVersion": 2,
                "manifests": [
                    {
                        **descriptor(oci_data),
                        "mediaType": "application/vnd.oci.image.manifest.v1+json",
                        "annotations": {
                            "io.containerd.image.name": "docker.io/" + TAG,
                            "org.opencontainers.image.ref.name": REVISION,
                        },
                    }
                ],
            }
            if index_change:
                index_change(index)
            add_file(archive, "index.json", json.dumps(index).encode())
            add_file(archive, "oci-layout", b'{"imageLayoutVersion":"1.0.0"}')
            add_file(archive, oci_path, oci_data)
        add_file(
            archive,
            "manifest.json",
            json.dumps(
                [
                    {
                        "Config": config_path,
                        "RepoTags": tags if tags is not None else [TAG],
                        "Layers": [layer_path],
                    }
                ]
                * image_count
            ).encode(),
        )
        add_file(archive, config_path, config_data)
        add_file(archive, layer_path, layer_data)
        if extra:
            extra(archive)


def release_manifest(archive: Path) -> ARTIFACT.Manifest:
    """Describe the fixture archive using the production manifest schema."""
    return {
        "schemaVersion": 1,
        "revision": REVISION,
        "platform": "linux/amd64",
        "archiveSha256": ARTIFACT.sha256_file(archive),
        "imageTag": TAG,
        "migrations": {"1": hashlib.sha384(b"SELECT 1;\n").hexdigest()},
        "createdAt": "2026-09-13T12:00:00Z",
    }


class RunConfiguration(TypedDict):
    """The exact subprocess options observed by the command fixture."""

    cwd: Path
    timeout: float
    env: Mapping[str, str] | None
    allow_failure: bool
    capture: bool


class ImageChanges(TypedDict, total=False):
    """Supported image identity mutations for archive rejection cases."""

    user: str
    architecture: str
    revision: str
    tags: list[str]


@dataclass
class FakeProcess:
    """An owned child fixture with explicit signal and timeout behavior."""

    pid: int = 12345
    deadlines: list[float | None] = field(default_factory=list)
    signals: list[int] = field(default_factory=list)
    failures: list[Exception] = field(default_factory=list)

    def wait(self, timeout: float | None = None) -> int:
        """Record a child wait and raise the next scripted timeout when requested."""
        self.deadlines.append(timeout)
        if self.failures:
            raise self.failures.pop(0)
        return 0

    def send_signal(self, signal: int) -> None:
        """Record signals sent to the owned fixture child."""
        self.signals.append(signal)


@final
class FakeRunner:
    """A deterministic command boundary that never invokes external tools."""

    def __init__(  # noqa: PLR0913 -- Explicit boundary options preserve the subprocess contract.
        self,
        *,
        dirty: bool = False,
        ignored: bool = True,
        fail: str | None = None,
        running: str = "",
        builder: str = "docker",
        final_revision: str = REVISION,
        save_help: str = "Options:\n      --platform string   Export a specific platform\n",
        server_api: str = "1.48",
        image: JsonObject | None = None,
        existing_tag: str = "",
        selected_images: list[str] | None = None,
    ) -> None:
        """Initialize explicit fixture or transport state before use."""
        self.output: Path | None = None
        self.calls: list[tuple[list[str], RunConfiguration]] = []
        self.dirty = dirty
        self.ignored = ignored
        self.fail = fail
        self.running = running
        self.builder = builder
        self.save_help = save_help
        self.server_api = server_api
        self.final_revision = final_revision
        self.revision_reads = 0
        self.context: Path | None = None
        self.image: JsonObject = (
            image
            if image is not None
            else {
                "id": IMAGE_ID,
                "os": "linux",
                "architecture": "amd64",
                "user": "10001:10001",
                "labels": {"org.opencontainers.image.revision": REVISION},
                "cmd": ["/app/simplestChat"],
                "entrypoint": None,
                "rootfs": {"Type": "layers", "Layers": [LAYER_ID]},
            }
        )
        self.existing_tag = existing_tag
        self.selected_images: Iterator[str] = iter(selected_images or [IMAGE_ID, IMAGE_ID])

    def run(  # noqa: C901, PLR0913, PLR0911, PLR0912, PLR0915 -- Keep ordered transaction checks together.
        self,
        argv: Sequence[str],
        *,
        cwd: Path,
        timeout: float = 30,
        env: Mapping[str, str] | None = None,
        allow_failure: bool = False,
        capture: bool = True,
    ) -> tuple[int, str]:
        """Execute the requested command and retain its bounded result."""
        argv = list(argv)
        self.calls.append(
            (
                argv,
                {
                    "cwd": cwd,
                    "timeout": timeout,
                    "env": env,
                    "allow_failure": allow_failure,
                    "capture": capture,
                },
            )
        )
        if argv[:2] == ["git", "check-ignore"]:
            return (0 if self.ignored else 1), ""
        if argv[:2] == ["git", "rev-parse"]:
            self.revision_reads += 1
            return 0, REVISION if self.revision_reads == 1 else self.final_revision
        if argv[:2] == ["git", "status"]:
            return 0, " M src/main.rs" if self.dirty else ""
        if argv[:2] == ["git", "archive"]:
            path = Path(
                next(value.split("=", 1)[1] for value in argv if value.startswith("--output="))
            )
            with tarfile.open(path, "w") as archive:
                for directory in ("build", "web", "migrations"):
                    member = tarfile.TarInfo(directory)
                    member.type = tarfile.DIRTYPE
                    member.mode = 0o775
                    archive.addfile(member)
                for name in (
                    "Dockerfile",
                    ".dockerignore",
                    "Cargo.lock",
                    "web/package-lock.json",
                    "build/pip-constraints.txt",
                ):
                    add_file(archive, name, b"pinned input", mode=0o664)
                add_file(archive, "build/install-openssl.sh", b"#!/bin/sh\n", mode=0o775)
                add_file(archive, "migrations/001_fixture.sql", b"SELECT 1;\n")
            return 0, ""
        if argv[:3] == ["docker", "context", "inspect"]:
            return 0, "unix:///var/run/docker.sock"
        if argv[:3] != ["docker", "--host", "unix:///var/run/docker.sock"]:
            raise AssertionError(f"Unexpected command: {argv}")
        command = argv[3:]
        if command == ["image", "save", "--help"]:
            if self.fail == "save-help":
                raise BUILD.BuildError("fixture Docker CLI probe failed")
            return 0, self.save_help
        if command == ["version", "--format", "{{.Server.APIVersion}}"]:
            if self.fail == "server-version":
                raise BUILD.BuildError("fixture Docker server probe failed")
            return 0, self.server_api
        if command[:1] == ["ps"]:
            return 0, self.running
        if command[:2] == ["buildx", "inspect"]:
            return 0, f"Name: default\nDriver: {self.builder}\nNodes:\n"
        if command[:2] == ["image", "inspect"]:
            if self.fail == "inspect":
                raise BUILD.BuildError("fixture selected image unavailable")
            if command[3] == "{{.Id}}":
                return 0, next(self.selected_images)
            assert command[-1] == IMAGE_ID  # noqa: S101 -- Offline fixture assertion, never a production guard.
            # Render the exact requested Go template so malformed JSON fails
            # in the production decoder instead of being hidden by this fake.
            rendered = command[3]
            expressions = {
                ".Id": "id",
                ".Os": "os",
                ".Architecture": "architecture",
                ".Config.User": "user",
                ".Config.Labels": "labels",
                ".Config.Cmd": "cmd",
                '(index .Config "Entrypoint")': "entrypoint",
                ".RootFS": "rootfs",
            }
            for expression, key in expressions.items():
                rendered = rendered.replace(
                    "{{json " + expression + "}}", json.dumps(self.image.get(key))
                )
            assert "{{" not in rendered  # noqa: S101 -- Offline fixture assertion, never a production guard.
            return 0, rendered
        if command[:2] == ["image", "ls"]:
            return 0, self.existing_tag
        if command[:2] == ["image", "tag"]:
            assert command == ["image", "tag", IMAGE_ID, TAG]  # noqa: S101 -- Offline fixture assertion, never a production guard.
            return 0, ""
        if command[:2] == ["buildx", "build"]:
            self.context = Path(command[-1])
            assert self.context.parent.stat().st_mode & 0o777 == 0o700  # noqa: S101, PLR2004 -- Offline fixture assertion, never a production guard.
            assert self.context.stat().st_mode & 0o777 == 0o755  # noqa: S101, PLR2004 -- Offline fixture assertion, never a production guard.
            assert (self.context / "Dockerfile").stat().st_mode & 0o777 == 0o644  # noqa: S101, PLR2004 -- Offline fixture assertion, never a production guard.
            assert (self.context / "build/install-openssl.sh").stat().st_mode & 0o777 == 0o755  # noqa: S101, PLR2004 -- Offline fixture assertion, never a production guard.
            if self.fail == "build":
                raise BUILD.BuildError("fixture build failure")
            return 0, ""
        if command[:2] == ["image", "save"]:
            if self.fail != "export":
                output = Path(command[command.index("--output") + 1])
                image_archive(
                    output,
                    revision="c" * 40 if self.fail == "archive-revision" else REVISION,
                    diff_ids=["sha256:" + "e" * 64] if self.fail == "archive-layers" else None,
                )
            return 0, ""
        raise AssertionError(f"Unexpected Docker command: {argv}")


class ReleaseBuildTests(unittest.TestCase):
    """Verify pinned builds, export-only behavior and retained failure evidence."""

    def __init__(self, method_name: str = "runTest") -> None:
        """Initialize explicit fixture or transport state before use."""
        super().__init__(method_name)
        self.root: Path = ROOT
        self.output: Path = ROOT

    @override
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="simplestchat-release-test.")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.output = self.root / "release"
        environment = dict(os.environ)
        self.addCleanup(os.environ.update, environment)
        self.addCleanup(os.environ.clear)
        os.environ.clear()

    def build(self, runner: FakeRunner) -> Path:
        """Build into this test's private output using its controlled command runner."""
        return BUILD.build_release(self.output, 120, root=self.root, runner=runner)

    def test_production_only_export_normalizes_source_and_records_metadata(self) -> None:
        """Verify production only export normalizes source and records metadata."""
        runner = FakeRunner()
        previous_umask = os.umask(0o077)
        try:
            _ = self.build(runner)
        finally:
            _ = os.umask(previous_umask)
        manifest = ARTIFACT.validate_manifest(self.output / "release.json")
        _ = ARTIFACT.verify_archive(self.output / "image.tar", manifest)
        self.assertEqual(manifest["migrations"], {"1": hashlib.sha384(b"SELECT 1;\n").hexdigest()})
        self.assertTrue(
            object_value(decode_json((self.output / "outcome.json").read_text()))["passed"]
        )
        assert runner.context is not None  # noqa: S101 -- Offline fixture assertion, never a production guard.
        self.assertFalse(runner.context.exists(), "Owned temporary source is cleaned up")
        build = next(argv for argv, _ in runner.calls if "build" in argv)
        self.assertIn("--pull", build)
        self.assertEqual(build[build.index("--target") + 1], "production")
        self.assertEqual(build[build.index("--platform") + 1], "linux/amd64")
        self.assertEqual(build[build.index("--tag") + 1], TAG)
        for argv, _ in runner.calls:
            self.assertTrue(
                {"push", "run", "stop", "restart", "prune", "loadtest"}.isdisjoint(argv)
            )

    def test_dirty_checkout_refused_before_output_or_docker(self) -> None:
        """Verify dirty checkout refused before output or docker."""
        runner = FakeRunner(dirty=True)
        with self.assertRaisesRegex(BUILD.BuildError, "checkout changes"):
            _ = self.build(runner)
        self.assertFalse(self.output.exists())
        self.assertTrue(all(argv[0] == "git" for argv, _ in runner.calls))

    def test_export_only_reuses_the_exact_image_and_keeps_the_existing_artifact_contract(
        self,
    ) -> None:
        """Verify export only reuses the exact image and keeps the existing artifact contract."""
        runner = FakeRunner(builder="unrelated-remote-builder")
        _ = BUILD.build_release(self.output, 120, root=self.root, runner=runner, image_id=IMAGE_ID)
        manifest = ARTIFACT.validate_manifest(self.output / "release.json")
        _ = ARTIFACT.verify_archive(self.output / "image.tar", manifest)
        source = object_value(decode_json((self.output / "source.json").read_text()))
        self.assertEqual(set(source), {"revision", "inputsSha256"})
        self.assertEqual(source["revision"], REVISION)
        self.assertEqual(
            set(object_value(source["inputsSha256"])),
            {
                "Dockerfile",
                ".dockerignore",
                "Cargo.lock",
                "web/package-lock.json",
                "build/pip-constraints.txt",
            },
        )
        outcome = object_value(decode_json((self.output / "outcome.json").read_text()))
        self.assertTrue(outcome["passed"])
        self.assertEqual(outcome["exportedImageId"], IMAGE_ID)
        commands = [argv[3:] for argv, _ in runner.calls if argv[:2] == ["docker", "--host"]]
        self.assertIn(["image", "tag", IMAGE_ID, TAG], commands)
        inspect = next(command for command in commands if command[:2] == ["image", "inspect"])
        self.assertEqual(inspect[-1], IMAGE_ID)
        save = next(
            command
            for command in commands
            if command[:2] == ["image", "save"] and "--output" in command
        )
        self.assertEqual(save[-1], TAG)
        self.assertEqual(
            sum(
                command == ["image", "inspect", "--format", "{{.Id}}", TAG] for command in commands
            ),
            2,
        )
        for command in commands:
            self.assertTrue(
                {"build", "buildx", "pull", "run", "push", "stop", "restart", "prune"}.isdisjoint(
                    command
                )
            )

    def test_generated_build_and_export_evidence_pass_the_real_github_receiver_contract(
        self,
    ) -> None:
        """Verify generated build and export evidence pass the real github receiver contract."""
        for label, image_id in (("build", None), ("export", IMAGE_ID)):
            with self.subTest(mode=label):
                output = self.root / f"receiver-contract-{label}"
                _ = BUILD.build_release(
                    output, 120, root=self.root, runner=FakeRunner(), image_id=image_id
                )
                manifest = RECEIVER.validate_build_evidence(output, {"revision": REVISION})
                self.assertEqual(manifest, ARTIFACT.validate_manifest(output / "release.json"))
                source_path = output / "source.json"
                source = object_value(decode_json(source_path.read_text()))
                self.assertEqual(set(source), {"revision", "inputsSha256"})
                # Reproduce the producer/consumer mismatch: source.json is a
                # closed schema; extra image evidence belongs in outcome.json.
                source["exportedImageId"] = IMAGE_ID
                _ = source_path.write_text(json.dumps(source))
                with self.assertRaisesRegex(RECEIVER.FetchError, "source_evidence_rejected"):
                    _ = RECEIVER.validate_build_evidence(output, {"revision": REVISION})

    def test_export_requires_an_immutable_id_before_any_side_effect(self) -> None:
        """Verify export requires an immutable id before any side effect."""
        for image_id in (
            "simplestchat-ci:production",
            "sha256:" + "b" * 63,
            "sha256:" + "B" * 64,
            "",
            True,
            123,
        ):
            with self.subTest(image_id=image_id):
                runner = FakeRunner()
                with self.assertRaisesRegex(BUILD.BuildError, "exact sha256"):
                    _ = BUILD.build_release(
                        self.output, 120, root=self.root, runner=runner, image_id=image_id
                    )
                self.assertEqual(runner.calls, [])
                self.assertFalse(self.output.exists())

    def test_export_rejects_wrong_image_identity_before_tagging_or_saving(self) -> None:
        """Verify export rejects wrong image identity before tagging or saving."""
        mutations: list[JsonObject] = [
            {"id": "sha256:" + "c" * 64},
            {"os": "windows"},
            {"architecture": "arm64"},
            {"user": "root"},
            {"labels": None},
            {"labels": {}},
            {"labels": {"org.opencontainers.image.revision": "c" * 40}},
            {"cmd": ["/app/load_test"]},
            {"entrypoint": ["/bin/sh"]},
        ]
        for index, mutation in enumerate(mutations):
            with self.subTest(mutation=mutation):
                self.output = self.root / f"image-identity-{index}"
                runner = FakeRunner()
                runner.image.update(mutation)
                with self.assertRaisesRegex(BUILD.BuildError, "production runtime identity"):
                    _ = BUILD.build_release(
                        self.output, 120, root=self.root, runner=runner, image_id=IMAGE_ID
                    )
                self.assertFalse(
                    any(
                        "tag" in argv or "--output" in argv or "build" in argv
                        for argv, _ in runner.calls
                    )
                )
                self.assertFalse((self.output / "release.json").exists())
                self.assertFalse(
                    object_value(decode_json((self.output / "outcome.json").read_text()))["passed"]
                )

    def test_export_failure_never_falls_back_to_building(self) -> None:
        """Verify export failure never falls back to building."""
        for failure in ("inspect", "export", "archive-revision", "archive-layers"):
            with self.subTest(failure=failure):
                self.output = self.root / f"export-failure-{failure}"
                runner = FakeRunner(fail=failure)
                with self.assertRaises((BUILD.BuildError, ARTIFACT.ArtifactError, OSError)):
                    _ = BUILD.build_release(
                        self.output, 120, root=self.root, runner=runner, image_id=IMAGE_ID
                    )
                self.assertFalse(any("build" in argv or "pull" in argv for argv, _ in runner.calls))
                self.assertFalse((self.output / "release.json").exists())
                self.assertFalse(
                    object_value(decode_json((self.output / "outcome.json").read_text()))["passed"]
                )

    def test_export_binds_archive_layers_to_selected_image_and_requires_bounded_layer_metadata(
        self,
    ) -> None:
        """Verify this export binds archive layers to selected image and case."""
        runner = FakeRunner(fail="archive-layers")
        with self.assertRaisesRegex(BUILD.BuildError, "filesystem layers differ"):
            _ = BUILD.build_release(
                self.output, 120, root=self.root, runner=runner, image_id=IMAGE_ID
            )
        self.assertFalse((self.output / "release.json").exists())
        rootfs_values: list[JsonValue] = [
            None,
            {},
            {"Type": "layers", "Layers": []},
            {"Type": "other", "Layers": [LAYER_ID]},
            {"Type": "layers", "Layers": ["sha256:wrong"]},
            json_value({"Type": "layers", "Layers": [LAYER_ID] * 1001}),
        ]
        for index, rootfs in enumerate(rootfs_values):
            with self.subTest(rootfs_index=index):
                self.output = self.root / f"invalid-layer-metadata-{index}"
                runner = FakeRunner()
                runner.image["rootfs"] = rootfs
                with self.assertRaisesRegex(BUILD.BuildError, "filesystem layer identities"):
                    _ = BUILD.build_release(
                        self.output, 120, root=self.root, runner=runner, image_id=IMAGE_ID
                    )
                self.assertFalse(
                    any("tag" in argv or "--output" in argv for argv, _ in runner.calls)
                )

    def test_export_refuses_different_retained_tag_without_overwriting_it(self) -> None:
        """Verify export refuses different retained tag without overwriting it."""
        runner = FakeRunner(existing_tag="sha256:" + "c" * 64)
        with self.assertRaisesRegex(BUILD.BuildError, "different image"):
            _ = BUILD.build_release(
                self.output, 120, root=self.root, runner=runner, image_id=IMAGE_ID
            )
        self.assertFalse(any("tag" in argv or "--output" in argv for argv, _ in runner.calls))
        self.assertFalse((self.output / "release.json").exists())

    def test_export_reuses_matching_tag_and_refuses_selection_changes(self) -> None:
        """Verify export reuses matching tag and refuses selection changes."""
        runner = FakeRunner(existing_tag=IMAGE_ID)
        _ = BUILD.build_release(self.output, 120, root=self.root, runner=runner, image_id=IMAGE_ID)
        self.assertFalse(any("tag" in argv for argv, _ in runner.calls))
        for index, selected in enumerate(
            (["sha256:" + "c" * 64], [IMAGE_ID, "sha256:" + "c" * 64])
        ):
            self.output = self.root / f"tag-change-{index}"
            runner = FakeRunner(selected_images=selected)
            with self.assertRaisesRegex(BUILD.BuildError, "selection changed"):
                _ = BUILD.build_release(
                    self.output, 120, root=self.root, runner=runner, image_id=IMAGE_ID
                )
            self.assertFalse((self.output / "release.json").exists())
            self.assertFalse(
                object_value(decode_json((self.output / "outcome.json").read_text()))["passed"]
            )

    def test_export_preserves_source_and_active_workload_preflights(self) -> None:
        """Verify export preserves source and active workload preflights."""
        for label, runner in (
            ("dirty", FakeRunner(dirty=True)),
            ("active", FakeRunner(running="owned-container")),
            ("source-change", FakeRunner(final_revision="c" * 40)),
        ):
            with self.subTest(label=label):
                self.output = self.root / label
                with self.assertRaises(BUILD.BuildError):
                    _ = BUILD.build_release(
                        self.output, 120, root=self.root, runner=runner, image_id=IMAGE_ID
                    )
                self.assertFalse((self.output / "release.json").exists())
                self.assertFalse(any("build" in argv for argv, _ in runner.calls))
                if label == "dirty":
                    self.assertFalse(self.output.exists())
                else:
                    self.assertFalse(
                        object_value(decode_json((self.output / "outcome.json").read_text()))[
                            "passed"
                        ]
                    )

    def test_nonignored_existing_and_unsafe_output_refused_before_docker(self) -> None:
        """Verify nonignored existing and unsafe output refused before docker."""
        for value in (self.output, self.root, self.root / "bad,name", self.root / "missing/child"):
            with self.subTest(value=value):
                runner = FakeRunner(ignored=False)
                with self.assertRaises(BUILD.BuildError):
                    _ = BUILD.build_release(value, 120, root=self.root, runner=runner)
                self.assertTrue(all(argv[0] == "git" for argv, _ in runner.calls))

    def test_path_outside_checkout_does_not_require_ignore_rule(self) -> None:
        """Verify path outside checkout does not require ignore rule."""
        repo = self.root / "repo"
        repo.mkdir()
        runner = FakeRunner(ignored=False)
        _ = BUILD.build_release(self.output, 120, root=repo, runner=runner)
        self.assertFalse(any(argv[:2] == ["git", "check-ignore"] for argv, _ in runner.calls))

    def test_invalid_deadline_has_no_subprocess_or_output_effects(self) -> None:
        """Verify invalid deadline has no subprocess or output effects."""
        for timeout in (0, 59, 7201, True, "120"):
            runner = FakeRunner()
            with self.assertRaises(BUILD.BuildError):
                _ = BUILD.build_release(self.output, timeout, root=self.root, runner=runner)
            self.assertEqual(runner.calls, [])
            self.assertFalse(self.output.exists())

    def test_build_export_and_revision_failures_retain_failed_attempt(self) -> None:
        """Verify build export and revision failures retain failed attempt."""
        for kind in ("build", "export", "revision"):
            with self.subTest(kind=kind):
                self.output = self.root / kind
                runner = FakeRunner(
                    fail=kind, final_revision="b" * 40 if kind == "revision" else REVISION
                )
                with self.assertRaises((BUILD.BuildError, OSError)):
                    _ = self.build(runner)
                self.assertFalse((self.output / "release.json").exists())
                self.assertFalse(
                    object_value(decode_json((self.output / "outcome.json").read_text()))["passed"]
                )
                self.assertEqual(sum("build" in argv for argv, _ in runner.calls), 1)
                assert runner.context is not None  # noqa: S101 -- Offline fixture assertion, never a production guard.
                self.assertFalse(runner.context.exists())

    def test_running_public_or_custom_builder_refused_before_build(self) -> None:
        """Verify running public or custom builder refused before build."""
        for name, runner in (
            ("public", FakeRunner(running="container")),
            ("builder", FakeRunner(builder="remote")),
        ):
            self.output = self.root / name
            with self.assertRaises(BUILD.BuildError):
                _ = self.build(runner)
            self.assertFalse(any("build" in argv for argv, _ in runner.calls))
            self.assertFalse((self.output / "release.json").exists())

    def test_remote_endpoint_refused_without_connecting_to_it(self) -> None:
        """Verify remote endpoint refused without connecting to it."""
        with patch.dict(os.environ, {"DOCKER_HOST": "ssh://root@fixture.invalid"}):
            runner = FakeRunner()
            with self.assertRaisesRegex(BUILD.BuildError, "local Unix"):
                _ = self.build(runner)
        self.assertTrue(all(argv[0] == "git" for argv, _ in runner.calls))

    def test_export_capabilities_are_checked_before_source_archive_build_or_export(self) -> None:
        """Verify export capabilities are checked before source archive build or export."""
        runner = FakeRunner()
        _ = self.build(runner)
        calls = [argv for argv, _ in runner.calls]
        prefix = ["docker", "--host", "unix:///var/run/docker.sock"]
        cli = calls.index([*prefix, "image", "save", "--help"])
        server = calls.index([*prefix, "version", "--format", "{{.Server.APIVersion}}"])
        archive = next(index for index, argv in enumerate(calls) if argv[:2] == ["git", "archive"])
        build = next(index for index, argv in enumerate(calls) if "build" in argv)
        export = next(index for index, argv in enumerate(calls) if "--output" in argv)
        self.assertLess(cli, server)
        self.assertLess(server, archive)
        self.assertLess(archive, build)
        self.assertLess(build, export)

    def test_missing_or_malformed_export_flag_fails_before_daemon_probe_and_retains_failure(
        self,
    ) -> None:
        """Verify missing or malformed export flag fails before daemon probe and retains failure."""
        for index, help_text in enumerate(
            (
                "",
                "Options:\n  -o, --output string\n",
                "Description mentions --platform only",
                "  --platform-other string\n",
                "  --platform string\n  --platform string\n",
            )
        ):
            with self.subTest(help_text=help_text):
                self.output = self.root / f"unsupported-cli-{index}"
                runner = FakeRunner(save_help=help_text)
                with self.assertRaisesRegex(BUILD.BuildError, "Docker CLI must support"):
                    _ = self.build(runner)
                self.assertFalse(
                    any(
                        "build" in argv
                        or "--output" in argv
                        or "version" in argv
                        or argv[:2] == ["git", "archive"]
                        for argv, _ in runner.calls
                    )
                )
                self.assertFalse((self.output / "release.json").exists())
                self.assertFalse(
                    object_value(decode_json((self.output / "outcome.json").read_text()))["passed"]
                )

    def test_unsupported_or_malformed_server_api_refuses_build_and_retains_failure(self) -> None:
        """Verify unsupported or malformed server api refuses build and retains failure."""
        versions = (
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
        for index, version in enumerate(versions):
            with self.subTest(version=version):
                self.output = self.root / f"unsupported-api-{index}"
                runner = FakeRunner(server_api=version)
                with self.assertRaisesRegex(BUILD.BuildError, "Docker server API"):
                    _ = self.build(runner)
                self.assertFalse(
                    any(
                        "build" in argv or "--output" in argv or argv[:2] == ["git", "archive"]
                        for argv, _ in runner.calls
                    )
                )
                self.assertFalse((self.output / "release.json").exists())
                self.assertFalse(
                    object_value(decode_json((self.output / "outcome.json").read_text()))["passed"]
                )

    def test_supported_api_comparison_is_numeric_and_uses_the_checked_endpoint_environment(
        self,
    ) -> None:
        """Verify supported api comparison is numeric and uses the checked endpoint environment."""
        for version in ("1.48", "1.49", "1.100", "2.0"):
            with self.subTest(version=version):
                runner = FakeRunner(server_api=version)
                with patch.dict(
                    os.environ,
                    {
                        "DOCKER_HOST": "unix:///var/run/docker.sock",
                        "DOCKER_API_VERSION": "1.40",
                        "BUILDX_BUILDER": "remote",
                    },
                ):
                    docker, environment = BUILD.docker_preflight(runner, self.root)
                self.assertEqual(docker, ["docker", "--host", "unix:///var/run/docker.sock"])
                self.assertNotIn("DOCKER_API_VERSION", environment)
                self.assertNotIn("BUILDX_BUILDER", environment)
                for argv, options in runner.calls:
                    self.assertEqual(argv[:3], docker)
                    self.assertEqual(options["env"], environment)

    def test_failed_cli_or_server_probe_does_not_attempt_a_build(self) -> None:
        """Verify failed cli or server probe does not attempt a build."""
        for failure in ("save-help", "server-version"):
            with self.subTest(failure=failure):
                self.output = self.root / failure
                runner = FakeRunner(fail=failure)
                with self.assertRaisesRegex(BUILD.BuildError, "probe failed"):
                    _ = self.build(runner)
                self.assertFalse(
                    any("build" in argv or "--output" in argv for argv, _ in runner.calls)
                )
                self.assertFalse(
                    object_value(decode_json((self.output / "outcome.json").read_text()))["passed"]
                )

    def test_unfinished_benchmark_guard_prevents_any_docker_call(self) -> None:
        """Verify unfinished benchmark guard prevents any docker call."""
        runner = FakeRunner()
        with (
            patch.object(
                BUILD,
                "benchmark_guard",
                side_effect=BUILD.BuildError("Benchmark cleanup is unfinished"),
            ),
            self.assertRaisesRegex(BUILD.BuildError, "unfinished"),
        ):
            _ = self.build(runner)
        self.assertTrue(all(argv[0] == "git" for argv, _ in runner.calls))

    def test_real_benchmark_guard_checks_record_and_shared_lock(self) -> None:
        """Verify real benchmark guard checks record and shared lock."""
        state = self.root / "benchmark"
        state.mkdir(mode=0o700)
        lock = state / "workload.lock"
        lock.touch(mode=0o600)
        record = state / "current.json"
        _ = record.write_text('{"schemaVersion":1,"finalized":false}')
        record.chmod(0o600)
        original_stat = Path.stat

        def owned_stat(path: Path, *, follow_symlinks: bool = True) -> os.stat_result:
            values = list(original_stat(path, follow_symlinks=follow_symlinks))
            values[4] = 0
            return os.stat_result(values)

        with patch.object(Path, "stat", owned_stat):
            with (
                self.assertRaisesRegex(BUILD.BuildError, "unfinished"),
                BUILD.benchmark_guard(state),
            ):
                self.fail("An unfinished benchmark cannot enter the build")
            _ = record.write_text('{"schemaVersion":1,"finalized":true}')
            with (
                BUILD.benchmark_guard(state),
                self.assertRaisesRegex(BUILD.BuildError, "active"),
                BUILD.benchmark_guard(state),
            ):
                self.fail("The shared lock must exclude concurrent workloads")

    def test_real_runner_retains_failure_and_timeout_evidence(self) -> None:
        """Verify real runner retains failure and timeout evidence."""
        self.output.mkdir()
        runner = BUILD.Runner(self.output)
        with self.assertRaisesRegex(BUILD.BuildError, "Command failed"):
            _ = runner.run(
                [sys.executable, "-c", "print('original failure'); raise SystemExit(7)"],
                cwd=self.root,
            )
        self.assertIn("original failure", next(self.output.glob("01-*.log")).read_text())
        self.assertEqual(
            object_value(decode_json(next(self.output.glob("01-*.outcome.json")).read_text()))[
                "exitStatus"
            ],
            7,
        )
        with self.assertRaisesRegex(BUILD.BuildError, "timed out"):
            _ = runner.run(
                [sys.executable, "-c", "import time; time.sleep(5)"], cwd=self.root, timeout=0.1
            )
        self.assertTrue(
            object_value(decode_json(next(self.output.glob("02-*.outcome.json")).read_text()))[
                "timedOut"
            ]
        )

    def test_persistent_release_guard_survives_missing_runtime_directory(self) -> None:
        """Verify persistent release guard survives missing runtime directory."""
        parent = self.root / "public"
        parent.mkdir(mode=0o700)
        record = parent / "release-state.json"
        _ = record.write_text('{"schemaVersion":1,"finalized":false}')
        record.chmod(0o600)
        original_stat = Path.lstat

        def owned_stat(path: Path) -> os.stat_result:
            values = list(original_stat(path))
            values[4] = 0
            return os.stat_result(values)

        with patch.object(Path, "lstat", owned_stat):
            with (
                self.assertRaisesRegex(BUILD.BuildError, "Release cleanup is unfinished"),
                BUILD.benchmark_guard(self.root / "absent-runtime", record),
            ):
                self.fail("Persistent release uncertainty must block building after reboot")
            _ = record.write_text('{"schemaVersion":1,"finalized":true}')
            with BUILD.benchmark_guard(self.root / "absent-runtime", record):
                pass
            record.chmod(0o644)
            with (
                self.assertRaisesRegex(BUILD.BuildError, "ownership"),
                BUILD.benchmark_guard(self.root / "absent-runtime", record),
            ):
                self.fail("Unprotected release records are not authoritative")

    def test_cleanup_does_not_probe_groups_and_falls_back_only_to_owned_child(self) -> None:
        """Verify cleanup does not probe groups and falls back only to owned child."""
        process = FakeProcess()
        with patch.object(
            os, "killpg", side_effect=PermissionError("fixture group permission")
        ) as group:
            outcome = BUILD.Runner.stop_process(process)
        group.assert_called_once_with(12345, signal.SIGTERM)
        self.assertEqual(process.signals, [signal.SIGTERM])
        self.assertEqual(process.deadlines, [15])
        self.assertTrue(outcome["groupSignalDenied"])
        self.assertFalse(outcome["forcedKill"])

    def test_cleanup_escalates_once_when_owned_child_ignores_term(self) -> None:
        """Verify cleanup escalates once when owned child ignores term."""
        process = FakeProcess()
        process.failures = [subprocess.TimeoutExpired("fixture", 15)]
        with patch.object(os, "killpg") as group:
            outcome = BUILD.Runner.stop_process(process)
        self.assertEqual(
            [call.args for call in group.call_args_list],
            [(12345, signal.SIGTERM), (12345, signal.SIGKILL)],
        )
        self.assertEqual(process.deadlines, [15, 5])
        self.assertTrue(outcome["forcedKill"])

    def test_duplicate_cli_option_fails_before_any_output(self) -> None:
        """Verify duplicate cli option fails before any output."""
        result = subprocess.run(  # noqa: S603 -- Execute explicit argv without a shell at the process boundary.
            [
                sys.executable,
                str(ROOT / "build/build-release.py"),
                "--output",
                str(self.output),
                "--output",
                str(self.root / "other"),
            ],
            text=True,
            check=False,
            capture_output=True,
            timeout=10,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("duplicate option", result.stderr)
        self.assertFalse(self.output.exists())


class ReleaseArtifactTests(unittest.TestCase):
    """Verify production archive metadata and extraction safety."""

    def __init__(self, method_name: str = "runTest") -> None:
        """Initialize explicit fixture or transport state before use."""
        super().__init__(method_name)
        self.root: Path = ROOT
        self.archive: Path = ROOT
        self.manifest: ARTIFACT.Manifest = {
            "schemaVersion": 1,
            "revision": REVISION,
            "platform": "linux/amd64",
            "archiveSha256": "c" * 64,
            "imageTag": TAG,
            "migrations": {},
            "createdAt": "2026-09-13T12:00:00Z",
        }

    @override
    def setUp(self) -> None:
        temporary = tempfile.TemporaryDirectory(prefix="simplestchat-artifact-test.")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.archive = self.root / "image.tar"
        image_archive(self.archive)
        self.manifest = release_manifest(self.archive)

    def test_manifest_rejects_unknown_duplicate_and_invalid_fields(self) -> None:
        """Verify manifest rejects unknown duplicate and invalid fields."""
        path = self.root / "release.json"
        mutations: list[JsonObject] = [
            {"extra": True},
            {"schemaVersion": True},
            {"revision": "main"},
            {"platform": "linux/arm64"},
            {"imageTag": "latest"},
            {"archiveSha256": "bad"},
            {"migrations": {}},
            {"migrations": {"001": "a" * 96}},
            {"createdAt": "2026-02-31T00:00:00Z"},
        ]
        for mutation in mutations:
            _ = path.write_text(json.dumps(self.manifest | mutation))
            with self.subTest(mutation=mutation), self.assertRaises(ARTIFACT.ArtifactError):
                _ = ARTIFACT.validate_manifest(path)
        _ = path.write_text('{"schemaVersion": 1, "schemaVersion": 1}')
        with self.assertRaisesRegex(ARTIFACT.ArtifactError, "Duplicate"):
            _ = ARTIFACT.validate_manifest(path)

    def test_archive_identity_platform_user_revision_and_tag_are_checked(self) -> None:
        """Verify archive identity platform user revision and tag are checked."""
        _ = ARTIFACT.verify_archive(self.archive, self.manifest)
        changes: list[ImageChanges] = [
            {"user": "root"},
            {"architecture": "arm64"},
            {"revision": "b" * 40},
            {"tags": [TAG, "unrelated:latest"]},
        ]
        for values in changes:
            image_archive(self.archive, **values)
            self.manifest["archiveSha256"] = ARTIFACT.sha256_file(self.archive)
            with self.subTest(values=values), self.assertRaises(ARTIFACT.ArtifactError):
                _ = ARTIFACT.verify_archive(self.archive, self.manifest)

    def test_modern_docker_and_oci_layouts_agree_on_exactly_one_image(self) -> None:
        """Verify modern docker and oci layouts agree on exactly one image."""
        image_archive(self.archive, modern=True)
        self.manifest["archiveSha256"] = ARTIFACT.sha256_file(self.archive)
        _ = ARTIFACT.verify_archive(self.archive, self.manifest)
        changes: list[Callable[[JsonObject], None]] = [
            lambda index: array_value(index["manifests"]).append(
                array_value(index["manifests"])[0]
            ),
            lambda index: object_value(
                object_value(array_value(index["manifests"])[0])["annotations"]
            ).update({"io.containerd.image.name": "unrelated:latest"}),
            lambda index: object_value(array_value(index["manifests"])[0]).update(
                {"digest": "sha256:" + "b" * 64}
            ),
            lambda index: object_value(array_value(index["manifests"])[0]).update({"size": 1}),
        ]
        for change in changes:
            image_archive(self.archive, modern=True, index_change=change)
            self.manifest["archiveSha256"] = ARTIFACT.sha256_file(self.archive)
            with self.assertRaises(ARTIFACT.ArtifactError):
                _ = ARTIFACT.verify_archive(self.archive, self.manifest)

    def test_multiple_docker_images_are_rejected(self) -> None:
        """Verify multiple docker images are rejected."""
        image_archive(self.archive, image_count=2)
        self.manifest["archiveSha256"] = ARTIFACT.sha256_file(self.archive)
        with self.assertRaisesRegex(ARTIFACT.ArtifactError, "exactly one"):
            _ = ARTIFACT.verify_archive(self.archive, self.manifest)

    def test_wrong_digest_is_rejected_before_tar_parsing(self) -> None:
        """Verify wrong digest is rejected before tar parsing."""
        _ = self.archive.write_bytes(b"not a tar")
        with self.assertRaisesRegex(ARTIFACT.ArtifactError, "SHA256"):
            _ = ARTIFACT.verify_archive(self.archive, self.manifest)

    def test_traversal_links_duplicate_metadata_and_multiple_images_are_rejected(self) -> None:
        """Verify traversal links duplicate metadata and multiple images are rejected."""

        def link(archive: tarfile.TarFile) -> None:
            member = tarfile.TarInfo("link")
            member.type = tarfile.SYMTYPE
            member.linkname = "/etc/passwd"
            archive.addfile(member)

        additions: list[Callable[[tarfile.TarFile], None]] = [
            lambda archive: add_file(archive, "../escape", b"invalid"),
            link,
            lambda archive: add_file(archive, "manifest.json", b"[]"),
        ]
        for extra in additions:
            image_archive(self.archive, extra=extra)
            self.manifest["archiveSha256"] = ARTIFACT.sha256_file(self.archive)
            with self.assertRaises(ARTIFACT.ArtifactError):
                _ = ARTIFACT.verify_archive(self.archive, self.manifest)

    def test_source_archive_links_are_not_extracted(self) -> None:
        """Verify source archive links are not extracted."""
        with tarfile.open(self.archive, "w") as archive:
            member = tarfile.TarInfo("link")
            member.type = tarfile.SYMTYPE
            member.linkname = "/etc/passwd"
            archive.addfile(member)
        with self.assertRaises(BUILD.BuildError):
            BUILD.unpack_source(self.archive, self.root / "source")
        self.assertFalse((self.root / "source/link").exists())


if __name__ == "__main__":
    _ = unittest.main()
