"""Offline production-release behavior; never contacts Docker or a remote host."""

from contextlib import contextmanager
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location("build_release", ROOT / "build/build-release.py")
BUILD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BUILD)
ARTIFACT = BUILD.ARTIFACT
REVISION = "a" * 40
TAG = f"simplestchat-release/production:{REVISION}"


def add_file(archive, name, data, mode=0o644):
    member = tarfile.TarInfo(name)
    member.size = len(data)
    member.mode = mode
    archive.addfile(member, io.BytesIO(data))


def image_archive(path, *, revision=REVISION, tags=None, user="10001:10001", architecture="amd64", extra=None, modern=False, index_change=None, image_count=1):
    config = {
        "architecture": architecture, "os": "linux", "config": {
            "User": user, "Cmd": ["/app/simplestChat"], "Entrypoint": None,
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
            descriptor = lambda data: {"digest": "sha256:" + hashlib.sha256(data).hexdigest(), "size": len(data)}
            oci_data = json.dumps({
                "schemaVersion": 2, "config": descriptor(config_data), "layers": [descriptor(layer_data)],
            }).encode()
            oci_path = "blobs/sha256/" + hashlib.sha256(oci_data).hexdigest()
            index = {"schemaVersion": 2, "manifests": [{
                **descriptor(oci_data), "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "annotations": {"io.containerd.image.name": "docker.io/" + TAG, "org.opencontainers.image.ref.name": REVISION},
            }]}
            if index_change:
                index_change(index)
            add_file(archive, "index.json", json.dumps(index).encode())
            add_file(archive, "oci-layout", b'{"imageLayoutVersion":"1.0.0"}')
            add_file(archive, oci_path, oci_data)
        add_file(archive, "manifest.json", json.dumps([{
            "Config": config_path, "RepoTags": tags if tags is not None else [TAG], "Layers": [layer_path],
        }] * image_count).encode())
        add_file(archive, config_path, config_data)
        add_file(archive, layer_path, layer_data)
        if extra:
            extra(archive)


def release_manifest(archive):
    return {
        "schemaVersion": 1, "revision": REVISION, "platform": "linux/amd64",
        "archiveSha256": ARTIFACT.sha256_file(archive), "imageTag": TAG,
        "migrations": {"1": hashlib.sha384(b"SELECT 1;\n").hexdigest()},
        "createdAt": "2026-09-13T12:00:00Z",
    }


class FakeRunner:
    def __init__(self, *, dirty=False, ignored=True, fail=None, running="", builder="docker", final_revision=REVISION,
                 save_help="Options:\n      --platform string   Export a specific platform\n", server_api="1.48"):
        self.output = None
        self.calls = []
        self.dirty = dirty
        self.ignored = ignored
        self.fail = fail
        self.running = running
        self.builder = builder
        self.save_help = save_help
        self.server_api = server_api
        self.final_revision = final_revision
        self.revision_reads = 0
        self.context = None

    def run(self, argv, **kwargs):
        self.calls.append((argv, kwargs))
        if argv[:2] == ["git", "check-ignore"]:
            return (0 if self.ignored else 1), ""
        if argv[:2] == ["git", "rev-parse"]:
            self.revision_reads += 1
            return 0, REVISION if self.revision_reads == 1 else self.final_revision
        if argv[:2] == ["git", "status"]:
            return 0, " M src/main.rs" if self.dirty else ""
        if argv[:2] == ["git", "archive"]:
            path = Path(next(value.split("=", 1)[1] for value in argv if value.startswith("--output=")))
            with tarfile.open(path, "w") as archive:
                for directory in ("build", "web", "migrations"):
                    member = tarfile.TarInfo(directory)
                    member.type = tarfile.DIRTYPE
                    member.mode = 0o775
                    archive.addfile(member)
                for name in ("Dockerfile", ".dockerignore", "Cargo.lock", "web/package-lock.json", "build/pip-constraints.txt"):
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
        if command[:2] == ["buildx", "build"]:
            self.context = Path(command[-1])
            assert self.context.parent.stat().st_mode & 0o777 == 0o700
            assert self.context.stat().st_mode & 0o777 == 0o755
            assert (self.context / "Dockerfile").stat().st_mode & 0o777 == 0o644
            assert (self.context / "build/install-openssl.sh").stat().st_mode & 0o777 == 0o755
            if self.fail == "build":
                raise BUILD.BuildError("fixture build failure")
            return 0, ""
        if command[:2] == ["image", "save"]:
            if self.fail != "export":
                output = Path(command[command.index("--output") + 1])
                image_archive(output)
            return 0, ""
        raise AssertionError(f"Unexpected Docker command: {argv}")


class ReleaseBuildTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="simplestchat-release-test.")
        self.root = Path(self.temporary.name).resolve()
        self.output = self.root / "release"
        self.environment = patch.dict(os.environ, {}, clear=True)
        self.environment.start()

    def tearDown(self):
        self.environment.stop()
        self.temporary.cleanup()

    def build(self, runner):
        return BUILD.build_release(self.output, 120, root=self.root, runner=runner)

    def test_production_only_export_normalizes_source_and_records_metadata(self):
        runner = FakeRunner()
        previous_umask = os.umask(0o077)
        try:
            self.build(runner)
        finally:
            os.umask(previous_umask)
        manifest = ARTIFACT.validate_manifest(self.output / "release.json")
        ARTIFACT.verify_archive(self.output / "image.tar", manifest)
        self.assertEqual(manifest["migrations"], {"1": hashlib.sha384(b"SELECT 1;\n").hexdigest()})
        self.assertTrue(json.loads((self.output / "outcome.json").read_text())["passed"])
        self.assertFalse(runner.context.exists(), "Owned temporary source is cleaned up")
        build = next(argv for argv, _ in runner.calls if "build" in argv)
        self.assertIn("--pull", build)
        self.assertEqual(build[build.index("--target") + 1], "production")
        self.assertEqual(build[build.index("--platform") + 1], "linux/amd64")
        self.assertEqual(build[build.index("--tag") + 1], TAG)
        for argv, _ in runner.calls:
            self.assertTrue({"push", "run", "stop", "restart", "prune", "loadtest"}.isdisjoint(argv))

    def test_dirty_checkout_refused_before_output_or_docker(self):
        runner = FakeRunner(dirty=True)
        with self.assertRaisesRegex(BUILD.BuildError, "checkout changes"):
            self.build(runner)
        self.assertFalse(self.output.exists())
        self.assertTrue(all(argv[0] == "git" for argv, _ in runner.calls))

    def test_nonignored_existing_and_unsafe_output_refused_before_docker(self):
        for value in (self.output, self.root, self.root / "bad,name", self.root / "missing/child"):
            with self.subTest(value=value):
                runner = FakeRunner(ignored=False)
                with self.assertRaises(BUILD.BuildError):
                    BUILD.build_release(value, 120, root=self.root, runner=runner)
                self.assertTrue(all(argv[0] == "git" for argv, _ in runner.calls))

    def test_path_outside_checkout_does_not_require_ignore_rule(self):
        repo = self.root / "repo"
        repo.mkdir()
        runner = FakeRunner(ignored=False)
        BUILD.build_release(self.output, 120, root=repo, runner=runner)
        self.assertFalse(any(argv[:2] == ["git", "check-ignore"] for argv, _ in runner.calls))

    def test_invalid_deadline_has_no_subprocess_or_output_effects(self):
        for timeout in (0, 59, 7201, True, "120"):
            runner = FakeRunner()
            with self.assertRaises(BUILD.BuildError):
                BUILD.build_release(self.output, timeout, root=self.root, runner=runner)
            self.assertEqual(runner.calls, [])
            self.assertFalse(self.output.exists())

    def test_build_export_and_revision_failures_retain_failed_attempt(self):
        for kind in ("build", "export", "revision"):
            with self.subTest(kind=kind):
                self.output = self.root / kind
                runner = FakeRunner(fail=kind, final_revision="b" * 40 if kind == "revision" else REVISION)
                with self.assertRaises((BUILD.BuildError, OSError)):
                    self.build(runner)
                self.assertFalse((self.output / "release.json").exists())
                self.assertFalse(json.loads((self.output / "outcome.json").read_text())["passed"])
                self.assertEqual(sum("build" in argv for argv, _ in runner.calls), 1)
                self.assertFalse(runner.context.exists())

    def test_running_public_or_custom_builder_refused_before_build(self):
        for name, runner in (("public", FakeRunner(running="container")), ("builder", FakeRunner(builder="remote"))):
            self.output = self.root / name
            with self.assertRaises(BUILD.BuildError):
                self.build(runner)
            self.assertFalse(any("build" in argv for argv, _ in runner.calls))
            self.assertFalse((self.output / "release.json").exists())

    def test_remote_endpoint_refused_without_connecting_to_it(self):
        with patch.dict(os.environ, {"DOCKER_HOST": "ssh://root@fixture.invalid"}):
            runner = FakeRunner()
            with self.assertRaisesRegex(BUILD.BuildError, "local Unix"):
                self.build(runner)
        self.assertTrue(all(argv[0] == "git" for argv, _ in runner.calls))

    def test_export_capabilities_are_checked_before_source_archive_build_or_export(self):
        runner = FakeRunner()
        self.build(runner)
        calls = [argv for argv, _ in runner.calls]
        prefix = ["docker", "--host", "unix:///var/run/docker.sock"]
        cli = calls.index(prefix + ["image", "save", "--help"])
        server = calls.index(prefix + ["version", "--format", "{{.Server.APIVersion}}"])
        archive = next(index for index, argv in enumerate(calls) if argv[:2] == ["git", "archive"])
        build = next(index for index, argv in enumerate(calls) if "build" in argv)
        export = next(index for index, argv in enumerate(calls) if "--output" in argv)
        self.assertLess(cli, server)
        self.assertLess(server, archive)
        self.assertLess(archive, build)
        self.assertLess(build, export)

    def test_missing_or_malformed_export_flag_fails_before_daemon_probe_and_retains_failure(self):
        for index, help_text in enumerate(("", "Options:\n  -o, --output string\n",
                                          "Description mentions --platform only", "  --platform-other string\n",
                                          "  --platform string\n  --platform string\n")):
            with self.subTest(help_text=help_text):
                self.output = self.root / f"unsupported-cli-{index}"
                runner = FakeRunner(save_help=help_text)
                with self.assertRaisesRegex(BUILD.BuildError, "Docker CLI must support"):
                    self.build(runner)
                self.assertFalse(any("build" in argv or "--output" in argv or "version" in argv
                                     or argv[:2] == ["git", "archive"] for argv, _ in runner.calls))
                self.assertFalse((self.output / "release.json").exists())
                self.assertFalse(json.loads((self.output / "outcome.json").read_text())["passed"])

    def test_unsupported_or_malformed_server_api_refuses_build_and_retains_failure(self):
        versions = ("1.47", "1.9", "0.99", "", "1", "1.48.0", "01.48", "1.048", "1.48-dev",
                    "1.48\n1.49", " 1.48", "1.48 ", "1.٤٨", "1.-48", "1000.0", "1.1000")
        for index, version in enumerate(versions):
            with self.subTest(version=version):
                self.output = self.root / f"unsupported-api-{index}"
                runner = FakeRunner(server_api=version)
                with self.assertRaisesRegex(BUILD.BuildError, "Docker server API"):
                    self.build(runner)
                self.assertFalse(any("build" in argv or "--output" in argv or argv[:2] == ["git", "archive"]
                                     for argv, _ in runner.calls))
                self.assertFalse((self.output / "release.json").exists())
                self.assertFalse(json.loads((self.output / "outcome.json").read_text())["passed"])

    def test_supported_api_comparison_is_numeric_and_uses_the_checked_endpoint_environment(self):
        for version in ("1.48", "1.49", "1.100", "2.0"):
            with self.subTest(version=version):
                runner = FakeRunner(server_api=version)
                with patch.dict(os.environ, {"DOCKER_HOST": "unix:///var/run/docker.sock",
                                             "DOCKER_API_VERSION": "1.40", "BUILDX_BUILDER": "remote"}):
                    docker, environment = BUILD.docker_preflight(runner, self.root)
                self.assertEqual(docker, ["docker", "--host", "unix:///var/run/docker.sock"])
                self.assertNotIn("DOCKER_API_VERSION", environment)
                self.assertNotIn("BUILDX_BUILDER", environment)
                for argv, options in runner.calls:
                    self.assertEqual(argv[:3], docker)
                    self.assertEqual(options["env"], environment)

    def test_failed_cli_or_server_probe_does_not_attempt_a_build(self):
        for failure in ("save-help", "server-version"):
            with self.subTest(failure=failure):
                self.output = self.root / failure
                runner = FakeRunner(fail=failure)
                with self.assertRaisesRegex(BUILD.BuildError, "probe failed"):
                    self.build(runner)
                self.assertFalse(any("build" in argv or "--output" in argv for argv, _ in runner.calls))
                self.assertFalse(json.loads((self.output / "outcome.json").read_text())["passed"])

    def test_unfinished_benchmark_guard_prevents_any_docker_call(self):
        @contextmanager
        def unfinished():
            raise BUILD.BuildError("Benchmark cleanup is unfinished")
            yield

        runner = FakeRunner()
        with patch.object(BUILD, "benchmark_guard", unfinished), self.assertRaisesRegex(BUILD.BuildError, "unfinished"):
            self.build(runner)
        self.assertTrue(all(argv[0] == "git" for argv, _ in runner.calls))

    def test_real_benchmark_guard_checks_record_and_shared_lock(self):
        state = self.root / "benchmark"
        state.mkdir(mode=0o700)
        lock = state / "workload.lock"
        lock.touch(mode=0o600)
        record = state / "current.json"
        record.write_text('{"schemaVersion":1,"finalized":false}')
        record.chmod(0o600)
        original_stat = Path.stat

        def owned_stat(path, *args, **kwargs):
            values = list(original_stat(path, *args, **kwargs))
            values[4] = 0
            return os.stat_result(values)

        with patch.object(Path, "stat", owned_stat):
            with self.assertRaisesRegex(BUILD.BuildError, "unfinished"), BUILD.benchmark_guard(state):
                self.fail("An unfinished benchmark cannot enter the build")
            record.write_text('{"schemaVersion":1,"finalized":true}')
            with BUILD.benchmark_guard(state):
                with self.assertRaisesRegex(BUILD.BuildError, "active"), BUILD.benchmark_guard(state):
                    self.fail("The shared lock must exclude concurrent workloads")

    def test_real_runner_retains_failure_and_timeout_evidence(self):
        self.output.mkdir()
        runner = BUILD.Runner(self.output)
        with self.assertRaisesRegex(BUILD.BuildError, "Command failed"):
            runner.run([sys.executable, "-c", "print('original failure'); raise SystemExit(7)"], cwd=self.root)
        self.assertIn("original failure", next(self.output.glob("01-*.log")).read_text())
        self.assertEqual(json.loads(next(self.output.glob("01-*.outcome.json")).read_text())["exitStatus"], 7)
        with self.assertRaisesRegex(BUILD.BuildError, "timed out"):
            runner.run([sys.executable, "-c", "import time; time.sleep(5)"], cwd=self.root, timeout=0.1)
        self.assertTrue(json.loads(next(self.output.glob("02-*.outcome.json")).read_text())["timedOut"])

    def test_persistent_release_guard_survives_missing_runtime_directory(self):
        parent = self.root / 'public'
        parent.mkdir(mode=0o700)
        record = parent / 'release-state.json'
        record.write_text('{"schemaVersion":1,"finalized":false}')
        record.chmod(0o600)
        original_stat = Path.lstat

        def owned_stat(path, *args, **kwargs):
            values = list(original_stat(path, *args, **kwargs))
            values[4] = 0
            return os.stat_result(values)

        with patch.object(Path, 'lstat', owned_stat):
            with self.assertRaisesRegex(BUILD.BuildError, 'Release cleanup is unfinished'):
                with BUILD.benchmark_guard(self.root / 'absent-runtime', record):
                    self.fail('Persistent release uncertainty must block building after reboot')
            record.write_text('{"schemaVersion":1,"finalized":true}')
            with BUILD.benchmark_guard(self.root / 'absent-runtime', record):
                pass
            record.chmod(0o644)
            with self.assertRaisesRegex(BUILD.BuildError, 'ownership'):
                with BUILD.benchmark_guard(self.root / 'absent-runtime', record):
                    self.fail('Unprotected release records are not authoritative')

    def test_cleanup_does_not_probe_groups_and_falls_back_only_to_owned_child(self):
        process = Mock(pid=12345)
        with patch.object(BUILD.os, "killpg", side_effect=PermissionError("fixture group permission")) as group:
            outcome = BUILD.Runner._stop(process)
        group.assert_called_once_with(12345, signal.SIGTERM)
        process.send_signal.assert_called_once_with(signal.SIGTERM)
        process.wait.assert_called_once_with(timeout=15)
        self.assertTrue(outcome["groupSignalDenied"])
        self.assertFalse(outcome["forcedKill"])

    def test_cleanup_escalates_once_when_owned_child_ignores_term(self):
        process = Mock(pid=12345)
        process.wait.side_effect = [subprocess.TimeoutExpired("fixture", 15), None]
        with patch.object(BUILD.os, "killpg") as group:
            outcome = BUILD.Runner._stop(process)
        self.assertEqual([call.args for call in group.call_args_list], [(12345, signal.SIGTERM), (12345, signal.SIGKILL)])
        self.assertEqual([call.kwargs for call in process.wait.call_args_list], [{"timeout": 15}, {"timeout": 5}])
        self.assertTrue(outcome["forcedKill"])

    def test_duplicate_cli_option_fails_before_any_output(self):
        result = subprocess.run([
            sys.executable, str(ROOT / "build/build-release.py"), "--output", str(self.output),
            "--output", str(self.root / "other"),
        ], text=True, capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 2)
        self.assertIn("duplicate option", result.stderr)
        self.assertFalse(self.output.exists())


class ReleaseArtifactTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="simplestchat-artifact-test.")
        self.root = Path(self.temporary.name)
        self.archive = self.root / "image.tar"
        image_archive(self.archive)
        self.manifest = release_manifest(self.archive)

    def tearDown(self):
        self.temporary.cleanup()

    def test_manifest_rejects_unknown_duplicate_and_invalid_fields(self):
        path = self.root / "release.json"
        mutations = [
            {"extra": True}, {"schemaVersion": True}, {"revision": "main"},
            {"platform": "linux/arm64"}, {"imageTag": "latest"}, {"archiveSha256": "bad"},
            {"migrations": {}}, {"migrations": {"001": "a" * 96}},
            {"createdAt": "2026-02-31T00:00:00Z"},
        ]
        for mutation in mutations:
            path.write_text(json.dumps(self.manifest | mutation))
            with self.subTest(mutation=mutation), self.assertRaises(ARTIFACT.ArtifactError):
                ARTIFACT.validate_manifest(path)
        path.write_text('{"schemaVersion": 1, "schemaVersion": 1}')
        with self.assertRaisesRegex(ARTIFACT.ArtifactError, "Duplicate"):
            ARTIFACT.validate_manifest(path)

    def test_archive_identity_platform_user_revision_and_tag_are_checked(self):
        ARTIFACT.verify_archive(self.archive, self.manifest)
        for values in ({"user": "root"}, {"architecture": "arm64"}, {"revision": "b" * 40}, {"tags": [TAG, "unrelated:latest"]}):
            image_archive(self.archive, **values)
            self.manifest["archiveSha256"] = ARTIFACT.sha256_file(self.archive)
            with self.subTest(values=values), self.assertRaises(ARTIFACT.ArtifactError):
                ARTIFACT.verify_archive(self.archive, self.manifest)

    def test_modern_docker_and_oci_layouts_agree_on_exactly_one_image(self):
        image_archive(self.archive, modern=True)
        self.manifest["archiveSha256"] = ARTIFACT.sha256_file(self.archive)
        ARTIFACT.verify_archive(self.archive, self.manifest)
        for change in (
            lambda index: index["manifests"].append(index["manifests"][0]),
            lambda index: index["manifests"][0]["annotations"].update({"io.containerd.image.name": "unrelated:latest"}),
            lambda index: index["manifests"][0].update({"digest": "sha256:" + "b" * 64}),
            lambda index: index["manifests"][0].update({"size": 1}),
        ):
            image_archive(self.archive, modern=True, index_change=change)
            self.manifest["archiveSha256"] = ARTIFACT.sha256_file(self.archive)
            with self.assertRaises(ARTIFACT.ArtifactError):
                ARTIFACT.verify_archive(self.archive, self.manifest)

    def test_multiple_docker_images_are_rejected(self):
        image_archive(self.archive, image_count=2)
        self.manifest["archiveSha256"] = ARTIFACT.sha256_file(self.archive)
        with self.assertRaisesRegex(ARTIFACT.ArtifactError, "exactly one"):
            ARTIFACT.verify_archive(self.archive, self.manifest)

    def test_wrong_digest_is_rejected_before_tar_parsing(self):
        self.archive.write_bytes(b"not a tar")
        with self.assertRaisesRegex(ARTIFACT.ArtifactError, "SHA256"):
            ARTIFACT.verify_archive(self.archive, self.manifest)

    def test_traversal_links_duplicate_metadata_and_multiple_images_are_rejected(self):
        def link(archive):
            member = tarfile.TarInfo("link")
            member.type = tarfile.SYMTYPE
            member.linkname = "/etc/passwd"
            archive.addfile(member)

        for extra in (
            lambda archive: add_file(archive, "../escape", b"invalid"), link,
            lambda archive: add_file(archive, "manifest.json", b"[]"),
        ):
            image_archive(self.archive, extra=extra)
            self.manifest["archiveSha256"] = ARTIFACT.sha256_file(self.archive)
            with self.assertRaises(ARTIFACT.ArtifactError):
                ARTIFACT.verify_archive(self.archive, self.manifest)

    def test_source_archive_links_are_not_extracted(self):
        with tarfile.open(self.archive, "w") as archive:
            member = tarfile.TarInfo("link")
            member.type = tarfile.SYMTYPE
            member.linkname = "/etc/passwd"
            archive.addfile(member)
        with self.assertRaises(BUILD.BuildError):
            BUILD.unpack_source(self.archive, self.root / "source")
        self.assertFalse((self.root / "source/link").exists())


if __name__ == "__main__":
    unittest.main()
