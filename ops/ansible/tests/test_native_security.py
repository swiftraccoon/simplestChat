"""Offline invariants for finite native workloads, provenance and exact cleanup."""

from __future__ import annotations

import hashlib
import json
import os
import platform
import shutil
import tempfile
import unittest
from pathlib import Path
from typing import override
from unittest.mock import Mock, patch

from test_support import ROOT, obj, objects, string

# isort: split
import bounded_process
import native_security as native
from release_json import JsonObject, decode_json, object_value

IMAGE = "sha256:" + "a" * 64
CONTAINER = "b" * 64
RUN_ID = "c" * 32
DIGEST = "d" * 64


class NativeSecurityTests(unittest.TestCase):
    """A failing or incomplete local check must never acquire a passing receipt."""

    root: Path = Path()

    @override
    def setUp(self) -> None:
        """Own one temporary copy of the small reviewed input corpus."""
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        _ = shutil.copytree(ROOT / "security/native", self.root / "security/native")
        machine = patch.object(platform, "machine", return_value="x86_64")
        _ = machine.start()
        self.addCleanup(machine.stop)

    def manifest(self) -> JsonObject:
        """Read a fixture as the maintained strict JSON domain."""
        return object_value(decode_json((self.root / "security/native/corpus.json").read_bytes()))

    def save(self, manifest: JsonObject) -> None:
        """Change only this test's owned corpus manifest."""
        _ = (self.root / "security/native/corpus.json").write_text(json.dumps(manifest))

    def test_reviewed_corpus_covers_every_native_family_with_exact_bytes(self) -> None:
        """Every family is nonempty and every declared byte count/digest is verified."""
        cases = native.corpus(self.root)
        self.assertEqual({case.family for case in cases}, set(native.FAMILIES))
        self.assertEqual(len(cases), 17)
        for case in cases:
            self.assertEqual(case.digest, hashlib.sha256(case.data).hexdigest())
            self.assertGreater(len(case.data), 0)
            self.assertLessEqual(len(case.data), native.MAX_INPUT)

    def test_corpus_rejects_duplicate_missing_family_and_unlisted_inputs(self) -> None:
        """Silently skipped, duplicated and newly unreviewed inputs invalidate replay."""
        original = self.manifest()
        cases = objects(original, "cases")
        variants: tuple[tuple[str, list[JsonObject]], ...] = (
            ("corpus_duplicate", [*cases, cases[0]]),
            ("corpus_missing_family", [case for case in cases if case["family"] != "dtls"]),
            (
                "missing_native_unit_fixture",
                [case for case in cases if case["id"] != "rtp-unit-packet1"],
            ),
        )
        for code, items in variants:
            with self.subTest(code=code):
                self.save({**original, "cases": list(items)})
                with self.assertRaisesRegex(native.SecurityError, code):
                    _ = native.corpus(self.root)
        self.save(original)
        _ = (self.root / "security/native/corpus/stun/unreviewed.hex").write_text("00\n")
        with self.assertRaisesRegex(native.SecurityError, "corpus_unlisted_file"):
            _ = native.corpus(self.root)

    def test_corpus_digest_size_path_encoding_and_symlink_guards(self) -> None:
        """The manifest cannot select another path or bless different bytes implicitly."""
        original = self.manifest()
        cases = objects(original, "cases")
        for key, value, code in (
            ("path", "../outside.hex", "corpus_path"),
            ("sha256", "0" * 64, "corpus_digest_mismatch"),
            ("bytes", 999, "corpus_size_mismatch"),
            ("family", "unknown", "corpus_family"),
        ):
            with self.subTest(key=key):
                changed: list[JsonObject] = [{**cases[0], key: value}, *cases[1:]]
                self.save({**original, "cases": list(changed)})
                with self.assertRaisesRegex(native.SecurityError, code):
                    _ = native.corpus(self.root)
        self.save(original)
        path = self.root / "security/native" / string(cases[0], "path")
        _ = path.write_text("not hexadecimal\n")
        with self.assertRaisesRegex(native.SecurityError, "corpus_encoding"):
            _ = native.corpus(self.root)
        path.unlink()
        path.symlink_to(self.root / "elsewhere")
        with self.assertRaises(OSError):
            _ = native.corpus(self.root)

    def test_corpus_rejects_oversized_and_duplicate_json_before_replay(self) -> None:
        """Parsing limits apply before allocation or native invocation."""
        path = self.root / "security/native/corpus.json"
        _ = path.write_bytes(b"x" * (native.MIB + 1))
        with self.assertRaisesRegex(native.SecurityError, "input_size_exceeded"):
            _ = native.corpus(self.root)
        _ = path.write_text('{"schemaVersion":1,"schemaVersion":1}')
        with self.assertRaises(ValueError):
            _ = native.corpus(self.root)

    def test_replay_uses_one_file_without_a_mutation_directory_or_unbounded_options(self) -> None:
        """LibFuzzer's file-input mode executes the reviewed input and exits."""
        path = Path("/work/corpus/stun/one")
        argv = native.replay_args(path)
        self.assertEqual(argv[-1], str(path))
        for flag in ("-timeout=5", "-rss_limit_mb=2048", "-max_len=65536"):
            self.assertIn(flag, argv)
        self.assertFalse(any(arg.startswith(("-runs=", "-max_total_time=")) for arg in argv))
        self.assertEqual(native.MODES, ("asan", "ubsan", "replay"))

    def test_family_environment_drops_ambient_fuzz_and_sanitizer_overrides(self) -> None:
        """The upstream target treats even '0' as enabled, so other flags must be absent."""
        with patch.dict(
            os.environ,
            {
                "MS_FUZZ_DTLS": "0",
                "ASAN_OPTIONS": "halt_on_error=0",
                "MESON_ARGS": "-Db_sanitize=none",
                "FAKE_SECRET": "canary",
            },
        ):
            env = native.worker_environment("stun")
        self.assertEqual({key for key in env if key.startswith("MS_FUZZ_")}, {"MS_FUZZ_STUN"})
        self.assertEqual(env["MS_FUZZ_STUN"], "1")
        self.assertIn("halt_on_error=1", env["ASAN_OPTIONS"])
        self.assertEqual(env["MESON_ARGS"], "--wrap-mode=nodownload")
        self.assertNotIn("FAKE_SECRET", env)

    def test_sandbox_has_no_network_mounts_socket_ports_or_privilege(self) -> None:
        """Compile and test containers have immutable image identity and hard resources."""
        argv = native.sandbox_args(IMAGE, RUN_ID, DIGEST)
        for required in (
            "--platform=linux/amd64",
            "--network=none",
            "--read-only",
            "--cap-drop=ALL",
            "--cpus=2",
            "--memory=6g",
            "--memory-swap=6g",
            "--pids-limit=256",
            "--ulimit=core=0:0",
            "--security-opt=no-new-privileges:true",
            "--user=65532:65532",
            "--tmpfs=/work:rw,exec,nosuid,nodev,size=5g,mode=1777",
            "--tmpfs=/tmp:rw,noexec,nosuid,nodev,size=256m,mode=1777",
            "--rm",
        ):
            self.assertIn(required, argv)
        self.assertEqual(argv[-1], IMAGE)
        self.assertFalse(
            any(
                arg.startswith(("--mount", "--volume", "--publish", "--privileged")) for arg in argv
            )
        )
        with self.assertRaisesRegex(native.SecurityError, "immutable_image_required"):
            _ = native.sandbox_args("builder:latest", RUN_ID, DIGEST)

    def test_image_must_match_current_inputs_and_exact_local_id(self) -> None:
        """An immutable but stale builder cannot silently test earlier source."""
        valid: JsonObject = {
            "Id": IMAGE,
            "Os": "linux",
            "Architecture": "amd64",
            "Config": {"Labels": {native.INPUT_LABEL: DIGEST}},
        }
        with patch.object(native, "command", return_value=json.dumps(valid).encode()):
            self.assertEqual(native.checked_image(["engine"], IMAGE, DIGEST)["Id"], IMAGE)
            with self.assertRaisesRegex(native.SecurityError, "image_inputs_mismatch"):
                _ = native.checked_image(["engine"], IMAGE, "other")
        with patch.object(
            native, "command", return_value=json.dumps({**valid, "Id": "a" * 64}).encode()
        ):
            self.assertEqual(native.checked_image(["engine"], IMAGE, DIGEST)["Os"], "linux")
        for value in ("builder:latest", "a" * 12, "sha256:" + "A" * 64):
            with self.subTest(value=value), self.assertRaises(native.SecurityError):
                _ = native.canonical_image_id(value)
        for architecture in ("arm64", "386", "", None):
            with (
                self.subTest(architecture=architecture),
                patch.object(
                    native,
                    "command",
                    return_value=json.dumps({**valid, "Architecture": architecture}).encode(),
                ),
                self.assertRaisesRegex(native.SecurityError, "native_image_architecture_mismatch"),
            ):
                _ = native.checked_image(["engine"], IMAGE, DIGEST)

    def test_preparation_and_execution_require_the_real_runner_architecture(self) -> None:
        """Both supported native targets are explicit and reject mismatched builder images."""
        output = self.root / "prepare"
        output.mkdir()
        _ = (output / "image.id").write_text(IMAGE)
        info: JsonObject = {
            "Id": IMAGE,
            "Os": "linux",
            "Architecture": "amd64",
            "Config": {"Labels": {native.INPUT_LABEL: DIGEST}},
        }
        for machine, architecture in (
            ("x86_64", "amd64"),
            ("aarch64", "arm64"),
            ("arm64", "arm64"),
        ):
            with (
                self.subTest(machine=machine),
                patch.object(platform, "machine", return_value=machine),
                patch.object(native, "inputs_digest", return_value=DIGEST),
                patch.object(native, "capture", return_value=0) as capture,
                patch.object(
                    native,
                    "command",
                    return_value=json.dumps({**info, "Architecture": architecture}).encode(),
                ),
            ):
                result = native.prepare(["engine"], output)
                target = "--platform=linux/" + architecture
                self.assertEqual(capture.call_args.args[0][:3], ["engine", "build", target])
                self.assertIn(target, native.sandbox_args(IMAGE, RUN_ID, DIGEST))
                self.assertEqual(result["imageId"], IMAGE)
                self.assertEqual(result["architecture"], architecture)
                other = "arm64" if architecture == "amd64" else "amd64"
                with (
                    patch.object(
                        native,
                        "command",
                        return_value=json.dumps({**info, "Architecture": other}).encode(),
                    ),
                    self.assertRaisesRegex(
                        native.SecurityError, "native_image_architecture_mismatch"
                    ),
                ):
                    _ = native.prepare(["engine"], output)

    def test_unsupported_runner_architecture_is_rejected(self) -> None:
        """A target without an authenticated native toolchain cannot run the suite."""
        with (
            patch.object(platform, "machine", return_value="riscv64"),
            self.assertRaisesRegex(native.SecurityError, "native_host_architecture_unsupported"),
        ):
            _ = native.sandbox_args(IMAGE, RUN_ID, DIGEST)

    def test_cleanup_revalidates_full_id_image_and_exact_run_label(self) -> None:
        """Unknown or relabelled resources are never removed by name/prefix."""
        valid: JsonObject = {
            "Id": CONTAINER,
            "Image": IMAGE,
            "Config": {"Labels": {native.RUN_LABEL: RUN_ID}},
        }
        variants: tuple[tuple[JsonObject, bool], ...] = (
            (valid, True),
            ({**valid, "Image": "sha256:" + "e" * 64}, False),
            ({**valid, "Id": "short"}, False),
            ({**valid, "Config": {"Labels": {}}}, False),
        )
        for info, success in variants:
            fake = Mock(side_effect=[(CONTAINER + "\n").encode(), json.dumps(info).encode(), b""])
            with self.subTest(info=info), patch.object(native, "command", fake):
                if success:
                    native.cleanup(["engine"], RUN_ID, IMAGE)
                    self.assertEqual(fake.call_args.args[0], ["engine", "rm", "--force", CONTAINER])
                else:
                    with self.assertRaisesRegex(
                        native.SecurityError, "container_ownership_changed"
                    ):
                        native.cleanup(["engine"], RUN_ID, IMAGE)
                    self.assertEqual(fake.call_count, 2)

    def test_run_timeout_always_attempts_exact_owned_cleanup(self) -> None:
        """A silent compiler/test cannot leave an unbounded attached client or claim success."""
        output = self.root / "run"
        output.mkdir()
        options = native.Options(image=IMAGE, mode="replay")
        with (
            patch.object(native, "inputs_digest", return_value=DIGEST),
            patch.object(native, "checked_image", return_value={"Architecture": "amd64"}),
            patch.object(native, "command", return_value=CONTAINER.encode()),
            patch.object(
                native, "capture", side_effect=bounded_process.ProcessError("command_timed_out")
            ),
            patch.object(native, "cleanup") as cleanup,
            self.assertRaisesRegex(bounded_process.ProcessError, "command_timed_out"),
        ):
            _ = native.run_native(options, ["engine"], output)
        self.assertEqual(cleanup.call_count, 1)
        self.assertEqual(cleanup.call_args.args[2], IMAGE)
        self.assertTrue((output / "ownership.json").exists())

    def test_incomplete_worker_result_fails_even_when_container_exits_zero(self) -> None:
        """A zero status without every requested corpus case is not a passing replay."""
        output = self.root / "run"
        output.mkdir()
        options = native.Options(image=IMAGE, mode="replay")
        _ = (output / "native.stdout.log").write_text(
            native.EVENT
            + json.dumps({"status": "passed", "mode": "replay", "completed": []})
            + "\n"
        )
        with (
            patch.object(native, "inputs_digest", return_value=DIGEST),
            patch.object(native, "checked_image", return_value={"Architecture": "amd64"}),
            patch.object(native, "command", return_value=CONTAINER.encode()),
            patch.object(native, "capture", return_value=0),
            patch.object(native, "cleanup"),
            self.assertRaisesRegex(native.SecurityError, "incomplete_worker_result"),
        ):
            _ = native.run_native(options, ["engine"], output)

    def test_output_directory_is_exclusive_and_failure_does_not_write_into_an_old_run(self) -> None:
        """Retrying the same path cannot overwrite or supplement earlier evidence."""
        output = self.root / "existing"
        output.mkdir()
        self.assertEqual(native.main(["prepare", "--output", str(output)]), 1)
        self.assertEqual(list(output.iterdir()), [])

    def test_toolchain_has_reviewed_both_linux_architectures_and_no_floating_urls(self) -> None:
        """Compiler, symbolizer and runtime originate from one exact official archive."""
        pin = object_value(decode_json((ROOT / "security/native/toolchain.json").read_bytes()))
        self.assertEqual(set(obj(pin, "platforms")), {"linux-x86_64", "linux-aarch64"})
        for value in obj(pin, "platforms").values():
            asset = object_value(value)
            self.assertRegex(string(asset, "sha256"), r"^[0-9a-f]{64}$")
            self.assertIn("/llvmorg-23.1.2/", string(asset, "url"))
            self.assertNotIn("latest", string(asset, "url"))
        dockerfile = (ROOT / "build/native-security.Dockerfile").read_text()
        self.assertIn("digest.hexdigest() != asset['sha256']", dockerfile)
        self.assertIn("CC=/opt/llvm/bin/clang CXX=/opt/llvm/bin/clang++", dockerfile)


if __name__ == "__main__":
    _ = unittest.main()
