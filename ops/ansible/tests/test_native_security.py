"""Offline invariants for finite native workloads, provenance and exact cleanup."""

from __future__ import annotations

import hashlib
import io
import json
import os
import platform
import shutil
import stat
import sys
import tempfile
import unittest
from pathlib import Path
from typing import cast, override
from unittest.mock import Mock, patch

from test_support import ROOT, obj, objects, string, yaml_value

# isort: split
import bounded_process
import native_security as native
import native_security_cache as image_cache
import security_codeql_resources as resources
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
        self.root = Path(temporary.name).resolve()
        _ = shutil.copytree(ROOT / "security/native", self.root / "security/native")
        machine = patch.object(platform, "machine", return_value="x86_64")
        _ = machine.start()
        self.addCleanup(machine.stop)
        budget = patch.object(resources, "detect", return_value=resources.Budget(4, 6144, 14000))
        _ = budget.start()
        self.addCleanup(budget.stop)

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
                "NINJA": "/untrusted/ninja",
                "FAKE_SECRET": "canary",
            },
        ):
            env = native.worker_environment("stun")
        self.assertEqual({key for key in env if key.startswith("MS_FUZZ_")}, {"MS_FUZZ_STUN"})
        self.assertEqual(env["MS_FUZZ_STUN"], "1")
        self.assertIn("halt_on_error=1", env["ASAN_OPTIONS"])
        self.assertEqual(env["MESON_ARGS"], "--wrap-mode=nodownload")
        self.assertEqual(env["NINJA"], "/opt/native-tools/pip_meson_ninja/bin/ninja")
        self.assertNotIn("FAKE_SECRET", env)

    def test_sandbox_has_no_network_mounts_socket_ports_or_privilege(self) -> None:
        """Compile and test containers have immutable image identity and hard resources."""
        argv = native.sandbox_args(IMAGE, RUN_ID, DIGEST)
        for required in (
            "--platform=linux/amd64",
            "--network=none",
            "--read-only",
            "--cap-drop=ALL",
            "--cpus=4",
            "--memory=9216m",
            "--memory-swap=9216m",
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
        with patch.dict("os.environ", {}, clear=True):
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

    def compiled(self, mode: str = "asan") -> tuple[Path, JsonObject]:
        """Create one bounded binary identity fixture without claiming runtime success."""
        directory = self.root / "compiled"
        directory.mkdir()
        header = bytearray(native.ELF_HEADER_BYTES)
        header[:6] = b"\x7fELF\x02\x01"
        header[18:20] = (62).to_bytes(2, "little")
        _ = (directory / "binary").write_bytes(header)
        receipt: JsonObject = {
            "schemaVersion": 1,
            "status": "built",
            "mode": mode,
            "imageId": IMAGE,
            "inputsSha256": DIGEST,
            "architecture": "amd64",
            "binaryBytes": len(header),
            "binarySha256": hashlib.sha256(header).hexdigest(),
            "worker": {"status": "built", "mode": mode},
        }
        _ = (directory / "receipt.json").write_text(json.dumps(receipt))
        return directory, receipt

    def test_compiled_artifact_rejects_wrong_identity_and_bytes_before_execution(self) -> None:
        """A same-named cache cannot change mode, tool image, corpus, ISA or executable."""
        directory, receipt = self.compiled()
        options = native.Options(mode="asan", image=IMAGE, compiled_directory=directory)
        self.assertEqual(native.compiled_receipt(directory, options, DIGEST), receipt)
        for key, value in (
            ("mode", "ubsan"),
            ("imageId", "sha256:" + "f" * 64),
            ("inputsSha256", "e" * 64),
            ("architecture", "arm64"),
            ("status", "passed"),
            ("binaryBytes", 65),
            ("binarySha256", "0" * 64),
        ):
            _ = (directory / "receipt.json").write_text(json.dumps({**receipt, key: value}))
            with (
                self.subTest(key=key),
                patch.object(native, "inputs_digest", return_value=DIGEST),
                patch.object(native, "checked_image"),
                patch.object(native, "command") as engine,
                self.assertRaises(native.SecurityError),
            ):
                _ = native.run_native(options, ["engine"], self.root)
            engine.assert_not_called()
        _ = (directory / "receipt.json").write_text(json.dumps(receipt))
        _ = (directory / "binary").write_bytes(b"invalid" * 20)
        with self.assertRaisesRegex(native.SecurityError, "compiled_binary_architecture"):
            _ = native.compiled_receipt(directory, options, DIGEST)

    def test_compiled_binary_rejects_symlink_empty_oversized_and_other_isa(self) -> None:
        """Cache restore cannot allocate unbounded input or execute another architecture."""
        directory, _ = self.compiled()
        path = directory / "binary"
        data = path.read_bytes()
        for size in (0, native.MAX_BINARY + 1):
            with path.open("wb") as output:
                _ = output.truncate(size)
            with (
                self.subTest(size=size),
                self.assertRaisesRegex(native.SecurityError, "compiled_binary_size"),
            ):
                _ = native.binary_identity(path)
        _ = path.write_bytes(data[:18] + (183).to_bytes(2, "little") + data[20:])
        with self.assertRaisesRegex(native.SecurityError, "compiled_binary_architecture"):
            _ = native.binary_identity(path)
        path.unlink()
        path.symlink_to(directory / "receipt.json")
        with self.assertRaises(OSError):
            _ = native.binary_identity(path)

    def test_compiled_restore_still_executes_and_requires_the_complete_fresh_suite(self) -> None:
        """A build receipt can only provide bytes; it never bypasses runtime validation."""
        directory, receipt = self.compiled("replay")
        output = self.root / "runtime"
        output.mkdir()
        options = native.Options(mode="replay", image=IMAGE, compiled_directory=directory)
        result = {
            "status": "passed",
            "mode": "replay",
            "completed": [case.identifier for case in native.corpus()],
        }
        _ = (output / "native.stdout.log").write_text(native.EVENT + json.dumps(result))
        with (
            patch.object(native, "inputs_digest", return_value=DIGEST),
            patch.object(native, "checked_image", return_value={"Architecture": "amd64"}),
            patch.object(native, "command", return_value=CONTAINER.encode()),
            patch.object(native, "send_binary") as transfer,
            patch.object(native, "capture", return_value=0) as execute,
            patch.object(native, "cleanup") as cleanup,
        ):
            report = native.run_native(options, ["engine"], output)
        self.assertEqual(report["status"], "passed")
        self.assertTrue(report["compiledReused"])
        self.assertEqual(report["worker"], result)
        self.assertEqual(execute.call_count, 1)
        self.assertTrue("exec" in execute.call_args.args[0])
        self.assertEqual(
            execute.call_args.args[0][-2:], ["--compiled-sha256", receipt["binarySha256"]]
        )
        transfer.assert_called_once()
        self.assertEqual(
            transfer.call_args.args[2:], (directory / "binary", receipt["binarySha256"])
        )
        cleanup.assert_called_once()

    def test_failed_compile_never_publishes_a_reusable_artifact(self) -> None:
        """Build failures and cancellation retain exact cleanup but no partial cache."""
        for status in (1, 77):
            output = self.root / ("failed-" + str(status))
            output.mkdir()
            directory = self.root / ("cache-" + str(status))
            options = native.Options(mode="asan", image=IMAGE, compiled_directory=directory)
            with (
                self.subTest(status=status),
                patch.object(native, "inputs_digest", return_value=DIGEST),
                patch.object(native, "checked_image"),
                patch.object(native, "command", return_value=CONTAINER.encode()),
                patch.object(native, "capture", return_value=status),
                patch.object(native, "cleanup") as cleanup,
                self.assertRaisesRegex(native.SecurityError, "native_build_failed_exit_"),
            ):
                _ = native.compile_native(options, ["engine"], output)
            cleanup.assert_called_once()
            self.assertFalse(directory.exists())
            self.assertEqual(list(self.root.glob("native-compiled-*")), [])

    def test_complete_build_publishes_only_after_identity_check_and_cleanup(self) -> None:
        """A copied binary becomes reusable only after exact cleanup succeeds."""
        fixture, receipt = self.compiled()
        binary = (fixture / "binary").read_bytes()
        output = self.root / "compile-success"
        output.mkdir()
        destination = self.root / "published"
        result = {
            "status": "built",
            "mode": "asan",
            "binaryBytes": len(binary),
            "binarySha256": receipt["binarySha256"],
            "phaseSeconds": {"compile": 3.5},
        }
        _ = (output / "compile.stdout.log").write_text(native.BUILD_EVENT + json.dumps(result))
        options = native.Options(mode="asan", image=IMAGE, compiled_directory=destination)

        def transfer(_engine: list[str], identifier: str, mode: str, destination: Path) -> None:
            """Write only the fixed executable into a private staging path."""
            self.assertEqual(identifier, CONTAINER)
            self.assertEqual(mode, "asan")
            _ = destination.write_bytes(binary)

        def clean(_engine: list[str], _run_id: str, _image: str) -> None:
            """Observe that publication has not happened while cleanup is pending."""
            self.assertFalse(destination.exists())

        with (
            patch.object(native, "inputs_digest", return_value=DIGEST),
            patch.object(native, "checked_image"),
            patch.object(native, "command", return_value=CONTAINER.encode()),
            patch.object(native, "export_binary", side_effect=transfer),
            patch.object(native, "capture", return_value=0),
            patch.object(native, "cleanup", side_effect=clean),
        ):
            report = native.compile_native(options, ["engine"], output)
        self.assertEqual(report["status"], "built")
        self.assertEqual(report["cleanup"], "verified")
        checked = native.compiled_receipt(destination, options, DIGEST)
        self.assertEqual(checked["binarySha256"], receipt["binarySha256"])
        self.assertEqual(stat.S_IMODE((destination / "binary").stat().st_mode), 0o755)

    def test_binary_transport_is_bounded_and_uses_fixed_unprivileged_exec_paths(self) -> None:
        """Read-only tmpfs artifacts travel through exec streams, never engine mount copying."""
        fixture, receipt = self.compiled()
        checksum = string(receipt, "binarySha256")
        with patch.object(bounded_process, "run", return_value=(0, b"", b"")) as execute:
            native.send_binary(["engine"], CONTAINER, fixture / "binary", checksum)
            self.assertEqual(execute.call_args.args[0][1:4], ["exec", "--interactive", CONTAINER])
            self.assertEqual(
                execute.call_args.kwargs["input_data"], (fixture / "binary").read_bytes()
            )
            limits = cast("bounded_process.Limits", execute.call_args.kwargs["limits"])
            self.assertEqual(limits.timeout, 120)
            self.assertEqual(
                execute.call_args.args[0][-4:],
                ["--compiled-sha256", checksum, "--compiled-bytes", "64"],
            )
        with (
            patch.object(bounded_process, "run") as execute,
            self.assertRaisesRegex(native.SecurityError, "compiled_transfer_changed"),
        ):
            native.send_binary(["engine"], CONTAINER, fixture / "binary", "f" * 64)
        execute.assert_not_called()
        with patch.object(bounded_process, "run", return_value=(0, b"", b"")) as execute:
            native.export_binary(["engine"], CONTAINER, "asan", self.root / "exported")
            self.assertEqual(
                execute.call_args.args[0],
                [
                    "engine",
                    "exec",
                    CONTAINER,
                    "/usr/bin/cat",
                    "/work/build/" + native.TARGETS["asan"],
                ],
            )
            limits = cast("bounded_process.Limits", execute.call_args.kwargs["limits"])
            self.assertEqual(limits.stdout, native.MAX_BINARY)

    def test_receiver_checks_stream_size_digest_and_exclusive_fixed_destination(self) -> None:
        """Truncated, changed and oversized transfers fail before runtime is invoked."""
        fixture, receipt = self.compiled()
        data = (fixture / "binary").read_bytes()
        checksum = string(receipt, "binarySha256")
        original_open = os.open
        for index, (payload, size, expected) in enumerate(
            (
                (data, len(data), ""),
                (data, len(data) + 1, "compiled_import_mismatch"),
                (data + b"extra", len(data), "compiled_import_size"),
                (data[:-1] + b"x", len(data), "compiled_import_mismatch"),
            )
        ):
            destination = self.root / ("received-" + str(index))

            def open_destination(
                path: Path, flags: int, mode: int, owned_destination: Path = destination
            ) -> int:
                """Substitute only the test's owned destination and preserve open protections."""
                self.assertEqual(path, Path("/work/native-program"))
                self.assertEqual(flags, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW)
                self.assertEqual(mode, 0o700)
                return original_open(owned_destination, flags, mode)

            with (
                self.subTest(expected=expected),
                patch.object(sys, "platform", "linux"),
                patch.object(Path, "is_dir", return_value=True),
                patch.object(os, "open", side_effect=open_destination),
                patch.object(sys, "stdin", io.TextIOWrapper(io.BytesIO(payload))),
            ):
                options = native.Options(compiled_sha256=checksum, compiled_bytes=size)
                if expected:
                    with self.assertRaisesRegex(native.SecurityError, expected):
                        native.receive_binary(options)
                else:
                    native.receive_binary(options)
                    self.assertEqual(destination.read_bytes(), data)

    def test_restored_worker_runs_every_replay_input_without_recompiling(self) -> None:
        """Cached input affects compilation only; all original family executions remain."""
        cases = native.corpus()
        with (
            patch.object(native, "worker_sources", return_value=cases),
            patch.object(native, "binary_identity", return_value=(64, DIGEST)),
            patch.object(native, "worker_compile") as compile_target,
            patch.object(native, "worker_command", return_value=0.1) as execute,
            patch.object(native, "read_regular", return_value=b"rpm receipt"),
            patch.object(native, "new_file"),
            patch.object(Path, "mkdir"),
        ):
            result = native.worker(native.Options(mode="replay", compiled_sha256=DIGEST))
        compile_target.assert_not_called()
        self.assertEqual(result["completed"], [case.identifier for case in cases])
        self.assertEqual(execute.call_count, 17)
        for call, case in zip(execute.call_args_list, cases, strict=True):
            self.assertEqual(call.args[0][0], "/work/native-program")
            self.assertEqual(call.kwargs, {"timeout": 10, "family": case.family})
            self.assertEqual(call.args[0][1:-1], native.replay_args(Path("unused"))[1:-1])

    def test_restored_worker_rechecks_binary_before_any_runtime_command(self) -> None:
        """A changed transfer fails inside the sandbox before its bytes can execute."""
        with (
            patch.object(native, "worker_sources", return_value=native.corpus()),
            patch.object(native, "binary_identity", return_value=(64, "f" * 64)),
            patch.object(native, "worker_command") as execute,
            self.assertRaisesRegex(native.SecurityError, "compiled_runtime_mismatch"),
        ):
            _ = native.worker(native.Options(mode="asan", compiled_sha256=DIGEST))
        execute.assert_not_called()

    def test_build_cache_identity_binds_mode_image_source_and_trust(self) -> None:
        """The three instrumented programs and rebuilt tool images never share keys."""
        with patch.object(image_cache, "cache_key", return_value="prepared-main-one"):
            baseline = image_cache.compiled_key(ROOT, IMAGE, "asan")
            self.assertNotEqual(baseline, image_cache.compiled_key(ROOT, IMAGE, "ubsan"))
            self.assertNotEqual(
                baseline, image_cache.compiled_key(ROOT, "sha256:" + "e" * 64, "asan")
            )
        with patch.object(image_cache, "cache_key", return_value="prepared-untrusted-other"):
            self.assertNotEqual(baseline, image_cache.compiled_key(ROOT, IMAGE, "asan"))

    def test_all_build_phases_preserve_instrumentation_and_bounded_generator_parallelism(
        self,
    ) -> None:
        """Every target keeps the upstream release flags and full tagged install."""
        for mode in native.MODES:
            commands = native.compile_commands(mode, 4)
            setup = commands["configure"]
            self.assertIn("--wrap-mode=nodownload", setup)
            self.assertIn("-Db_ndebug=true", setup)
            self.assertIn("-Db_lundef=false", setup)
            self.assertIn("-Db_sanitize=" + ("undefined" if mode == "ubsan" else "address"), setup)
            for phase in ("generator", "compile"):
                self.assertEqual(commands[phase][4:6], ["-j", "4"])
            self.assertEqual(commands["compile"][-1], native.TARGETS[mode])
            self.assertEqual(
                commands["install"][-3:], ["--no-rebuild", "--tags", native.TARGETS[mode]]
            )
        env = native.worker_environment()
        for option in (
            "detect_leaks=1",
            "strict_init_order=1",
            "check_initialization_order=1",
            "detect_container_overflow=1",
        ):
            self.assertIn(option, env["ASAN_OPTIONS"])

    def test_resources_fall_back_with_real_cpu_and_memory_ceilings(self) -> None:
        """Four jobs require their real memory allowance; smaller runners stay bounded."""
        for workers, memory in ((1, 3072), (2, 5120), (3, 7168), (4, 9216)):
            with patch.object(
                resources,
                "detect",
                return_value=resources.Budget(workers, 2048, memory),
            ):
                args = native.sandbox_args(IMAGE, RUN_ID, DIGEST)
            self.assertIn(f"--cpus={workers}", args)
            self.assertIn(f"--memory={memory}m", args)
            self.assertIn(f"--env=MEDIASOUP_BUILD_JOBS={workers}", args)
        with (
            patch.object(resources, "detect", return_value=resources.Budget(1, 1024, 1024)),
            self.assertRaisesRegex(native.SecurityError, "native_insufficient_memory"),
        ):
            _ = native.sandbox_args(IMAGE, RUN_ID, DIGEST)

    def preparation(self) -> Path:
        """Write a successful preparation receipt for an immutable fixture image."""
        output = self.root / "prepare-cache"
        output.mkdir()
        native.write_json(
            output / "report.json",
            {
                "schemaVersion": 1,
                "status": "passed",
                "imageId": IMAGE,
                "inputsSha256": DIGEST,
                "architecture": "amd64",
            },
        )
        return output

    def test_prepared_image_key_tracks_build_inputs_and_modes_not_unrelated_changes(self) -> None:
        """Tool/source bytes and chmod invalidate preparation while unrelated docs do not."""
        context = self.root / "context"
        context.mkdir()
        _ = native.inputs_digest(ROOT, context)
        original = image_cache.cache_key(context)
        _ = (context / "unrelated-operations.md").write_text("Unrelated documentation changed\n")
        self.assertEqual(image_cache.cache_key(context), original)
        pin = context / "security/native/toolchain.json"
        mode = stat.S_IMODE(pin.stat().st_mode)
        content_digest = native.inputs_digest(context)
        pin.chmod(mode | stat.S_IXUSR)
        self.assertEqual(native.inputs_digest(context), content_digest)
        self.assertNotEqual(image_cache.cache_key(context), original)
        pin.chmod(mode)
        _ = pin.write_text(pin.read_text() + "\n")
        changed = image_cache.cache_key(context)
        self.assertNotEqual(changed, original)
        with patch.object(platform, "machine", return_value="aarch64"):
            self.assertNotEqual(image_cache.cache_key(context), changed)
        with patch.dict(os.environ, {"GITHUB_REF": "refs/heads/main", "GITHUB_EVENT_NAME": "push"}):
            trusted = image_cache.cache_key(context)
        with patch.dict(os.environ, {"GITHUB_EVENT_NAME": "pull_request"}):
            self.assertNotEqual(image_cache.cache_key(context), trusted)

    def test_prepared_image_roundtrip_checks_exact_id_inputs_and_architecture(self) -> None:
        """A Docker-save archive is loaded only after its receipt and hash are checked."""
        preparation = self.preparation()
        directory = self.root / "image-cache"
        output = self.root / "loaded-preparation"
        info: JsonObject = {
            "Id": IMAGE,
            "Os": "linux",
            "Architecture": "amd64",
            "Config": {"Labels": {native.INPUT_LABEL: DIGEST}},
        }

        def command(argv: list[str], *, timeout: int = 30) -> bytes:
            self.assertGreater(timeout, 0)
            if argv[1:3] == ["image", "save"]:
                self.assertEqual(argv[-1], IMAGE)
                _ = Path(argv[4]).write_bytes(b"owned-image-archive")
                return b""
            if argv[1:3] == ["image", "load"]:
                return b"loaded"
            self.assertEqual(argv[-1], IMAGE)
            return json.dumps(info).encode()

        with (
            patch.object(native, "inputs_digest", return_value=DIGEST),
            patch.object(
                native, "source_files", return_value=[self.root / "security/native/corpus.json"]
            ),
            patch.object(native, "command", side_effect=command) as engine,
        ):
            key = image_cache.cache_key(self.root)
            image_cache.save(["engine"], self.root, preparation, directory, key)
            image_cache.load(["engine"], self.root, directory, output, key)
            info["Architecture"] = "arm64"
            with self.assertRaisesRegex(native.SecurityError, "native_image_architecture_mismatch"):
                image_cache.load(["engine"], self.root, directory, self.root / "wrong-isa", key)
        self.assertEqual(engine.call_count, 6)
        self.assertFalse((self.root / "wrong-isa/report.json").exists())
        self.assertEqual(
            (output / "report.json").read_bytes(), (preparation / "report.json").read_bytes()
        )
        self.assertEqual({path.name for path in directory.iterdir()}, {"image.tar", "receipt.json"})

    def test_prepared_cache_rejects_tampering_before_engine_load(self) -> None:
        """Changing any bound metadata or archive bytes prevents the engine from seeing it."""
        report = object_value(decode_json((self.preparation() / "report.json").read_bytes()))
        directory = self.root / "cache"
        directory.mkdir()
        archive = directory / "image.tar"
        _ = archive.write_bytes(b"archive")
        with (
            patch.object(native, "inputs_digest", return_value=DIGEST),
            patch.object(
                native, "source_files", return_value=[self.root / "security/native/corpus.json"]
            ),
        ):
            key = image_cache.cache_key(self.root)
            valid: JsonObject = {
                "key": key,
                "preparation": report,
                "archiveBytes": 7,
                "archiveSha256": hashlib.sha256(b"archive").hexdigest(),
            }
            variants = (
                {**valid, "key": "another-key"},
                {**valid, "archiveSha256": "0" * 64},
                {**valid, "archiveBytes": 8},
                {**valid, "preparation": {**report, "inputsSha256": "0" * 64}},
                {**valid, "preparation": {**report, "architecture": "arm64"}},
                {**valid, "preparation": {**report, "imageId": "mutable:tag"}},
                {**valid, "preparation": {**report, "status": "failed"}},
            )
            for receipt in variants:
                _ = (directory / "receipt.json").write_text(json.dumps(receipt))
                with (
                    self.subTest(receipt=receipt),
                    patch.object(native, "command") as engine,
                    self.assertRaises(native.SecurityError),
                ):
                    image_cache.load(["engine"], self.root, directory, self.root / "output", key)
                engine.assert_not_called()

    def test_interrupted_prepared_export_never_publishes_a_partial_cache(self) -> None:
        """Cancellation before the receipt is complete cannot expose a reusable archive."""
        preparation = self.preparation()
        directory = self.root / "image-cache"
        with (
            patch.object(native, "inputs_digest", return_value=DIGEST),
            patch.object(
                native, "source_files", return_value=[self.root / "security/native/corpus.json"]
            ),
            patch.object(native, "checked_image"),
            patch.object(native, "command", side_effect=KeyboardInterrupt),
            self.assertRaises(KeyboardInterrupt),
        ):
            image_cache.save(
                ["engine"], self.root, preparation, directory, image_cache.cache_key(self.root)
            )
        self.assertFalse(directory.exists())
        self.assertEqual(list(self.root.glob("native-image-*")), [])

    def test_prepared_archive_limits_reject_empty_large_and_symlink_inputs(self) -> None:
        """Restored archives must be ordinary bounded files before hashing or engine access."""
        archive = self.root / "archive"
        for size in (0, image_cache.MAX_ARCHIVE + 1):
            with archive.open("wb") as stream:
                _ = stream.truncate(size)
            with (
                self.subTest(size=size),
                self.assertRaisesRegex(native.SecurityError, "cache_archive_size"),
            ):
                _ = image_cache.archive_identity(archive)
        archive.unlink()
        archive.symlink_to(self.root / "security/native/corpus.json")
        with self.assertRaises(OSError):
            _ = image_cache.archive_identity(archive)

    def test_native_security_is_optional_local_and_not_automated(self) -> None:
        """Local prepared-image and compiled-binary tools have no hosted execution job."""
        workflow = yaml_value(
            (ROOT / ".github/workflows/security.yml").read_text(), scalars_as_strings=True
        )
        self.assertNotIn("native-security", obj(workflow, "jobs"))
        self.assertNotIn("security-deep", obj(workflow, "jobs"))
        self.assertEqual(native.MODES, ("asan", "ubsan", "replay"))

    def test_automated_native_execution_is_refused_before_engine_or_output(self) -> None:
        """Direct native entrypoints cannot bypass the optional-local execution boundary."""
        output = self.root / "automated-output"
        with (
            patch.dict("os.environ", {"CI": "true"}),
            patch.object(native, "engine_prefix") as engine,
            patch.object(native, "prepare") as prepare,
        ):
            self.assertEqual(native.main(["prepare", "--output", str(output)]), 1)
        engine.assert_not_called()
        prepare.assert_not_called()
        self.assertFalse(output.exists())


if __name__ == "__main__":
    _ = unittest.main()
