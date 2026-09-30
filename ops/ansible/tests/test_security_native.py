"""Offline native provenance fixtures bind real archive bytes to Cargo build events."""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path
from typing import TYPE_CHECKING, override
from unittest.mock import patch

from test_support import ROOT

# isort: split
import security_native as native
import security_vendor as vendor
from release_json import JsonObject, JsonValue, array_value, decode_json, object_value
from test_security_vendor import tar_bytes

if TYPE_CHECKING:
    from collections.abc import Sequence

VERSION = "0.45.0"
REVISION = "a" * 40
WORKER = "vendor/mediasoup-sys-0.17.0"


def put(root: Path, path: str, body: bytes) -> Path:
    """Create bounded fixture content without external tools."""
    target = root / path
    target.parent.mkdir(parents=True, exist_ok=True)
    _ = target.write_bytes(body)
    return target


def source(identifier: str, body: bytes, url: str | None = None) -> JsonObject:
    """Record exact archive bytes as a fixture source."""
    return {
        "id": identifier,
        "url": url or "https://example.invalid/" + identifier,
        "sha256": vendor.sha256(body),
        "format": "tar.gz",
    }


def artifact(identifier: str, manifest: Path, *, binary: Path | None = None) -> JsonObject:
    """Represent current Cargo JSON events, including a default production profile."""
    return {
        "reason": "compiler-artifact",
        "package_id": identifier,
        "manifest_path": str(manifest),
        "target": {
            "name": "simplestChat" if binary else "fixture",
            "kind": ["bin" if binary else "lib"],
        },
        "profile": {"test": False, "debug_assertions": False, "opt_level": "3"},
        "features": ["default"],
        "filenames": [str(binary)] if binary else [],
        "executable": str(binary) if binary else None,
        "fresh": True,
    }


class Fixture:
    """One entirely local source import, registry unpack and successful build transcript."""

    def __init__(self, root: Path) -> None:
        """Build a minimal complete fixture; no package downloads or compilers run."""
        root = root.resolve()
        self.root: Path = root
        self.cache: Path = root / "cache"
        self.cache.mkdir(mode=0o700)
        self.system: Path = root / "system"
        self.runtime: Path = put(root, "system/lib/libstdc++.a", b"!<arch>\nCXX")
        self.openssl: Path = root / "openssl"
        self.cargo: Path = root / "cargo-home"
        self.registry: Path = self.cargo / "registry/src/index/aws-lc-sys-0.45.0"
        self.binary: Path = put(root, "target/release/simplestChat", b"production binary fixture")
        self.native_manifest: JsonObject = self.make_sources()
        self.events: list[JsonValue] = self.make_events()
        self.arguments: native.Build = native.Build(
            root,
            root / "vendor-evidence/report.json",
            root / "cargo-build.json",
            self.cargo,
            self.openssl,
            root / "native-components.build.json",
        )
        self.save_events()
        self.refresh_vendor_receipt()

    def make_sources(self) -> JsonObject:
        """Build authenticated worker and AWS-LC archives plus their exact local files."""
        installer = b"openssl_version='3.5.8'\nopenssl_sha256='" + b"0" * 64 + b"'\n"
        _ = put(self.root, "build/install-openssl.sh", installer)
        _ = put(
            self.root,
            "Cargo.toml",
            b'[package]\nname="simplestChat"\nversion="0.1.0"\n'
            + b'rust-version="1.98.1"\npublish=false\n',
        )
        aws_files = {
            "Cargo.toml": b'[package]\nname="aws-lc-sys"\nversion="0.45.0"\nlicense="ISC"\n',
            "Cargo.toml.orig": (
                f'[package.metadata.aws-lc-sys]\ncommit-hash="{REVISION}"\n'
            ).encode(),
            "aws-lc/include/openssl/base.h": b'#define AWSLC_VERSION_NUMBER_STRING "5.7.0"\n',
        }
        aws = tar_bytes({"aws-lc-sys-0.45.0/" + name: body for name, body in aws_files.items()})
        for name, body in aws_files.items():
            _ = put(self.registry, name, body)
        _ = put(self.registry, ".cargo-ok", b'{"v":1}')
        _ = put(self.cargo, "registry/cache/index/aws-lc-sys-0.45.0.crate", aws)
        ssl_manifest = b'[package]\nname="openssl-sys"\nversion="0.9.117"\nlicense="MIT"\n'
        ssl = tar_bytes({"openssl-sys-0.9.117/Cargo.toml": ssl_manifest})
        _ = put(self.cargo, "registry/src/index/openssl-sys-0.9.117/Cargo.toml", ssl_manifest)
        _ = put(self.cargo, "registry/cache/index/openssl-sys-0.9.117.crate", ssl)
        _ = put(
            self.root,
            "Cargo.lock",
            (
                f'[[package]]\nname="aws-lc-sys"\nversion="0.45.0"\nsource="{native.SOURCE_REGISTRY}"\nchecksum="{vendor.sha256(aws)}"\n'
                + '[[package]]\nname="openssl-sys"\nversion="0.9.117"\n'
                + f'source="{native.SOURCE_REGISTRY}"\nchecksum="{vendor.sha256(ssl)}"\n'
                + '[[package]]\nname="mediasoup-sys"\nversion="0.17.0"\n'
                + '[[package]]\nname="simplestChat"\nversion="0.1.0"\n'
            ).encode(),
        )
        component: JsonObject = {
            "wrap_components": [
                {
                    "source": "foo-1.0",
                    "name": "foo",
                    "version": "1.0",
                    "linkage": "static",
                    "usage": "production",
                    "license": "MIT",
                }
            ],
            "adapted_component": {
                "name": "libwebrtc",
                "version": "m77",
                "revision": REVISION,
                "path": WORKER + "/deps/libwebrtc",
            },
            "openssl": {
                "version": "3.5.8",
                "source": {
                    "id": "openssl-3.5.8",
                    "url": "https://github.com/openssl/openssl/releases/download/openssl-3.5.8/openssl-3.5.8.tar.gz",
                    "sha256": "0" * 64,
                    "format": "tar.gz",
                },
                "installer_sha256": vendor.sha256(installer),
            },
            "registry_component": {
                "name": "AWS-LC",
                "version": "5.7.0",
                "revision": REVISION,
                "crate": "aws-lc-sys",
                "crate_version": VERSION,
                "source": source(
                    "aws-lc-sys-0.45.0",
                    aws,
                    "https://static.crates.io/crates/aws-lc-sys/aws-lc-sys-0.45.0.crate",
                ),
            },
            "native_links": {
                "mediasoup-sys": {
                    "static": ["stdc++", "mediasoup-worker", "ssl", "crypto"],
                    "system": ["dl", "pthread"],
                },
                "openssl-sys": {"static": ["ssl", "crypto"], "system": []},
                "aws-lc-sys": {"static": ["aws_lc_0_45_0_crypto"], "system": []},
            },
        }
        for key in ("adapted_component", "openssl", "registry_component"):
            object_value(component[key])["license"] = "ISC"
        _ = put(self.root, "vendor/native-components.json", vendor.json_bytes(component))
        self.make_vendor()
        for name in ("ssl", "crypto"):
            _ = put(self.openssl, "lib/lib" + name + ".a", b"!<arch>\n" + name.encode())
        _ = put(
            self.openssl, "include/openssl/opensslv.h", b'# define OPENSSL_VERSION_STR "3.5.8"\n'
        )
        return component

    def make_vendor(self) -> None:
        """Use the actual vendor verifier to produce source evidence for the fixture."""
        foo = tar_bytes({"foo-1.0/source.c": b"int fixture;\n"})
        wrap = (
            "[wrap-file]\ndirectory = foo-1.0\n"
            + "source_url = https://example.invalid/foo-1.0\n"
            + f"source_filename = foo.tar.gz\nsource_hash = {vendor.sha256(foo)}\n"
        ).encode()
        files = {
            "Cargo.toml": b'[package]\nname="mediasoup-sys"\nversion="0.17.0"\n',
            "subprojects/foo.wrap": wrap,
            "deps/libwebrtc/README.md": (
                f"- libwebrtc branch: m77\n- libwebrtc commit: {REVISION}\n"
            ).encode(),
        }
        worker = tar_bytes({"mediasoup-sys-0.17.0/" + name: body for name, body in files.items()})
        for name, body in files.items():
            _ = put(self.root, WORKER + "/" + name, body)
        for body in (foo, worker):
            vendor.write_private(self.cache / vendor.sha256(body), body)
        manifest: JsonObject = {
            "sources": [source("foo-1.0", foo), source("mediasoup-sys-0.17.0", worker)],
            "trees": [
                {
                    "path": WORKER,
                    "source": "mediasoup-sys-0.17.0",
                    "prefix": "mediasoup-sys-0.17.0",
                    "changes": [],
                }
            ],
            "files": [],
            "maintained_files": ["vendor/native-components.json"],
            "wraps": [
                {
                    "path": WORKER + "/subprojects/foo.wrap",
                    "source": "foo-1.0",
                    "patch_source": None,
                    "patch_directory": None,
                    "fallback_urls": [],
                }
            ],
        }
        _ = put(self.root, "vendor/integrity.json", vendor.json_bytes(manifest))

    def make_events(self) -> list[JsonValue]:
        """Record only declared build-script output directories, not a target-directory scan."""
        events: list[JsonValue] = [
            artifact(
                "path+file:///app#simplestChat@0.1.0", self.root / "Cargo.toml", binary=self.binary
            )
        ]
        providers = object_value(self.native_manifest["native_links"])
        for name, value in providers.items():
            identifier = (
                "path+file:///app/vendor/mediasoup-sys-0.17.0#mediasoup-sys@0.17.0"
                if name == "mediasoup-sys"
                else native.SOURCE_REGISTRY
                + "#"
                + name
                + ("@0.45.0" if name == "aws-lc-sys" else "@0.9.117")
            )
            manifest = (
                self.root / WORKER / "Cargo.toml"
                if name == "mediasoup-sys"
                else self.cargo
                / "registry/src/index"
                / ("aws-lc-sys-0.45.0" if name == "aws-lc-sys" else "openssl-sys-0.9.117")
                / "Cargo.toml"
            )
            events.append(artifact(identifier, manifest))
            out = self.root / "target/release/build" / name / "out"
            out.mkdir(parents=True)
            names = native.strings(object_value(value)["static"])
            for library in names:
                if library not in {"stdc++", "ssl", "crypto"}:
                    _ = put(out, "lib" + library + ".a", b"!<arch>\n" + library.encode())
            events.append(
                {
                    "reason": "build-script-executed",
                    "package_id": identifier,
                    "out_dir": str(out),
                    "linked_libs": ["static=" + library for library in names],
                    "linked_paths": [
                        "native=" + str(path)
                        for path in (out, self.openssl / "lib", self.system / "lib")
                    ],
                }
            )
        events.append({"reason": "build-finished", "success": True})
        return events

    def save_events(self) -> None:
        """Write one JSON document per line as Cargo does."""
        _ = put(
            self.root,
            "cargo-build.json",
            ("\n".join(json.dumps(item) for item in self.events) + "\n").encode(),
        )

    def refresh_vendor_receipt(self) -> None:
        """Generate source evidence without requiring any Internet connectivity."""
        vendor.verify(self.root, self.cache, self.root / "vendor-evidence", offline=True)

    def command(self, argv: Sequence[str], *, limit: int = native.MAX_REPORT) -> str:
        """Model only reviewed read-only builder commands; unexpected argv fails the test."""
        _ = limit
        rpm = "libstdc++-static\t0:16.0-1.fc44.x86_64\tgcc-16.0-1.fc44.src.rpm\n"
        if list(argv) == ["rustc", "--version", "--verbose"]:
            return "rustc 1.98.1\nrelease: 1.98.1\nhost: x86_64-unknown-linux-gnu\n"
        if list(argv) == ["c++", "--version"]:
            return "c++ (GCC) fixture\n"
        if list(argv) == [
            "rpm",
            "--query",
            "--file",
            str(self.runtime),
            "--queryformat",
            "%{LICENSE}\n",
        ]:
            return "GPL-3.0-or-later WITH GCC-exception-3.1\n"
        if argv[:3] == ["rpm", "--query", "--all"] or argv[:4] == [
            "rpm",
            "--query",
            "--file",
            str(self.runtime),
        ]:
            return rpm
        message = "Unexpected native provenance command"
        raise AssertionError(message)


class NativeTests(unittest.TestCase):
    """A receipt must identify the complete, current, successful build it describes."""

    def __init__(self, methodName: str = "runTest") -> None:  # noqa: N803 -- unittest API.
        """Declare the typed fixture before unittest setup."""
        super().__init__(methodName)
        self.fixture: Fixture | None = None

    @override
    def setUp(self) -> None:
        """Keep every source, cache, artifact and receipt inside one private directory."""
        temporary = tempfile.TemporaryDirectory(prefix="native-test-")
        self.addCleanup(temporary.cleanup)
        self.fixture = Fixture(Path(temporary.name))

    def current(self) -> Fixture:
        """Return the initialized fixture without suppressing strict typing."""
        if self.fixture is None:
            self.fail("Fixture not initialized")
        return self.fixture

    def produce(self) -> JsonObject:
        """Run the real producer while substituting only builder OS/compiler inspection."""
        fixture = self.current()
        with (
            patch.object(native, "SYSTEM_ROOT", fixture.system),
            patch.object(native, "command", side_effect=fixture.command),
        ):
            native.produce(fixture.arguments)
        return object_value(decode_json(fixture.arguments.output.read_bytes()))

    def test_complete_receipt_binds_sources_binary_archives_and_builder(self) -> None:
        """Positive evidence contains actual bytes and distinctly classified source components."""
        fixture = self.current()
        report = self.produce()
        self.assertEqual(
            object_value(report["binary"])["sha256"], vendor.sha256(fixture.binary.read_bytes())
        )
        self.assertEqual(len(array_value(report["static_archives"])), 7)
        self.assertEqual(object_value(report["registry_component"])["verified_files"], 3)
        self.assertIn(
            "libstdc++-static", str(object_value(report["toolchain"])["static_cxx_owner"])
        )
        self.assertEqual(
            report["cargo_messages_sha256"],
            vendor.sha256(fixture.arguments.cargo_messages.read_bytes()),
        )
        licenses = {
            native.text(object_value(item), "name"): object_value(item)
            for item in array_value(report["rust_licenses"])
        }
        self.assertEqual(object_value(licenses["openssl-sys"]["license"])["expression"], "MIT")
        self.assertTrue(licenses["simplestChat"]["first_party"])
        self.assertIs(licenses["simplestChat"]["cargo_publish"], expr2=False)
        self.assertNotIn("cargo_publish", licenses["openssl-sys"])
        self.assertIsNone(object_value(licenses["simplestChat"]["license"])["expression"])

    def test_first_party_receipt_requires_explicit_unpublished_cargo_policy(self) -> None:
        """The receipt cannot invent a nonpublication policy from an absent license."""
        fixture = self.current()
        manifest = fixture.root / "Cargo.toml"
        original = manifest.read_text()
        for replacement in ("", "publish=true\n", 'publish="false"\n', 'publish=["registry"]\n'):
            with self.subTest(replacement=replacement):
                _ = manifest.write_text(original.replace("publish=false\n", replacement))
                with self.assertRaisesRegex(native.NativeError, "publication policy"):
                    _ = self.produce()

    def test_rust_license_metadata_comes_from_authenticated_archive(self) -> None:
        """An edited unpacked manifest cannot invent a different upstream license declaration."""
        fixture = self.current()
        path = fixture.cargo / "registry/src/index/openssl-sys-0.9.117/Cargo.toml"
        _ = path.write_text(path.read_text().replace('license="MIT"', 'license="unreviewed"'))
        report = self.produce()
        item = next(
            object_value(item)
            for item in array_value(report["rust_licenses"])
            if object_value(item)["name"] == "openssl-sys"
        )
        self.assertEqual(object_value(item["license"])["expression"], "MIT")

    def test_rust_license_archive_checksum_is_required_for_each_built_package(self) -> None:
        """The wrapper crate's license is independently hash-bound, not borrowed from AWS-LC."""
        fixture = self.current()
        _ = put(fixture.cargo, "registry/cache/index/openssl-sys-0.9.117.crate", b"corrupt")
        with self.assertRaisesRegex(vendor.IntegrityError, "SHA-256"):
            _ = self.produce()

    def test_license_files_retain_exact_hash_and_unknowns_stay_unknown(self) -> None:
        """License-file declarations preserve source identity without guessing a SPDX name."""
        record = native.package_license(
            {"license-file": "LICENSE.custom"}, {"LICENSE.custom": b"custom terms"}
        )
        self.assertIsNone(record["expression"])
        self.assertEqual(
            object_value(record["license_file"])["sha256"], vendor.sha256(b"custom terms")
        )
        with self.assertRaisesRegex(native.NativeError, "license file is absent"):
            _ = native.package_license({"license-file": "missing"}, {})
        with self.assertRaises(vendor.IntegrityError):
            _ = native.package_license({"license-file": "../outside"}, {})

    def test_changed_vendor_file_or_receipt_diff_fails_before_build_evidence(self) -> None:
        """A previously successful receipt cannot authorize later native source changes."""
        fixture = self.current()
        _ = put(fixture.root, WORKER + "/Cargo.toml", b"changed source")
        with self.assertRaisesRegex(native.NativeError, "tree bytes differ"):
            _ = self.produce()
        self.assertFalse(fixture.arguments.output.exists())

    def test_incomplete_failed_or_additional_cargo_messages_fail(self) -> None:
        """No receipt may be made from an interrupted, unsuccessful or concatenated build."""
        fixture = self.current()
        base = fixture.events.copy()
        failed: JsonObject = {"reason": "build-finished", "success": False}
        variants: list[list[JsonValue]] = [
            base[:-1],
            [*base[:-1], failed],
            [*base, base[0]],
        ]
        for events in variants:
            with self.subTest(events=events[-1]):
                fixture.events = events
                fixture.save_events()
                with self.assertRaises(native.NativeError):
                    _ = self.produce()

    def test_load_test_features_and_nonrelease_binary_are_rejected(self) -> None:
        """The optional client's dependency graph cannot stand in for production evidence."""
        fixture = self.current()
        event = object_value(fixture.events[0])
        event["features"] = ["default", "load-test"]
        fixture.save_events()
        with self.assertRaisesRegex(native.NativeError, "default release production"):
            _ = self.produce()
        event["features"] = ["default"]
        object_value(event["profile"])["test"] = True
        fixture.save_events()
        with self.assertRaisesRegex(native.NativeError, "default release production"):
            _ = self.produce()

    def test_unknown_dynamic_or_missing_native_links_fail(self) -> None:
        """Changed native linkage requires reviewed provider/library policy changes."""
        fixture = self.current()
        event = next(
            object_value(item)
            for item in fixture.events
            if object_value(item)["reason"] == "build-script-executed"
        )
        original = event["linked_libs"]
        for libraries in (["dylib=ssl"], ["static=unreviewed"], []):
            with self.subTest(libraries=libraries):
                event["linked_libs"] = list[JsonValue](libraries)
                fixture.save_events()
                with self.assertRaises(native.NativeError):
                    _ = self.produce()
        event["linked_libs"] = original

    def test_registry_archive_or_unpacked_source_drift_fails(self) -> None:
        """Cargo's package version alone cannot authenticate a bundled C library."""
        fixture = self.current()
        _ = put(
            fixture.registry, "aws-lc/include/openssl/base.h", b"modified native implementation"
        )
        with self.assertRaisesRegex(native.NativeError, "authenticated crate bytes"):
            _ = self.produce()

    def test_corrupt_registry_archive_is_rejected_before_parsing(self) -> None:
        """The Cargo.lock archive digest remains mandatory for native registry input."""
        fixture = self.current()
        _ = put(fixture.cargo, "registry/cache/index/aws-lc-sys-0.45.0.crate", b"corrupt")
        with self.assertRaisesRegex(vendor.IntegrityError, "SHA-256"):
            _ = self.produce()

    def test_stale_native_manifest_record_and_mutable_registry_source_fail(self) -> None:
        """Source classifications and registry locks cannot become disconnected records."""
        fixture = self.current()
        array_value(fixture.native_manifest["wrap_components"]).append(
            {
                "source": "extra-1.0",
                "name": "extra",
                "version": "1.0",
                "usage": "production",
                "linkage": "static",
                "license": "MIT",
            }
        )
        _ = put(
            fixture.root,
            "vendor/native-components.json",
            vendor.json_bytes(fixture.native_manifest),
        )
        with self.assertRaisesRegex(native.NativeError, "Missing or stale native wrap"):
            _ = native.validate_manifest(fixture.root)
        _ = array_value(fixture.native_manifest["wrap_components"]).pop()
        _ = put(
            fixture.root,
            "vendor/native-components.json",
            vendor.json_bytes(fixture.native_manifest),
        )
        path = fixture.root / "Cargo.lock"
        _ = path.write_text(
            path.read_text().replace(native.SOURCE_REGISTRY, "git+https://example.invalid/main")
        )
        with self.assertRaisesRegex(native.NativeError, "registry source differs"):
            _ = native.validate_manifest(fixture.root)

    def test_missing_thin_or_ambiguous_static_archives_are_rejected(self) -> None:
        """A receipt hashes one self-contained archive from the actual declared search path."""
        fixture = self.current()
        worker = fixture.root / "target/release/build/mediasoup-sys/out/libmediasoup-worker.a"
        worker.unlink()
        with self.assertRaisesRegex(native.NativeError, "Missing or ambiguous"):
            _ = self.produce()
        _ = worker.write_bytes(b"!<thin>\nexternal objects")
        with self.assertRaisesRegex(native.NativeError, "self-contained"):
            _ = self.produce()
        _ = worker.write_bytes(b"!<arch>\nworker")
        _ = put(fixture.openssl, "lib/libmediasoup-worker.a", b"!<arch>\nambiguous")
        with self.assertRaisesRegex(native.NativeError, "Missing or ambiguous"):
            _ = self.produce()

    def test_cargo_output_directory_cannot_escape_the_build(self) -> None:
        """Artifact selection never walks a stale or unrelated target directory."""
        fixture = self.current()
        event = next(
            object_value(item)
            for item in fixture.events
            if object_value(item)["reason"] == "build-script-executed"
        )
        event["out_dir"] = str(fixture.openssl)
        fixture.save_events()
        with self.assertRaisesRegex(native.NativeError, "escaped"):
            _ = self.produce()

    def test_current_openssl_installation_and_toolchain_are_checked(self) -> None:
        """Version records must agree with built headers and compiler identity."""
        fixture = self.current()
        _ = put(
            fixture.openssl, "include/openssl/opensslv.h", b'# define OPENSSL_VERSION_STR "3.0.8"\n'
        )
        with self.assertRaisesRegex(native.NativeError, "OpenSSL headers differ"):
            _ = self.produce()
        with (
            patch.object(native, "command", return_value="release: 1.0.0\n"),
            self.assertRaisesRegex(native.NativeError, "Rust compiler differs"),
        ):
            _ = native.toolchain_evidence(fixture.root, fixture.runtime)

    def test_rpm_signing_keys_are_preserved_separately_from_build_packages(self) -> None:
        """Current Fedora RPM key entries are provenance, not source-backed libraries."""
        key = "gpg-pubkey\t0:36f612dcf27f7d1a48a835e4dbfcf71c6d9f90a6-6786af3b.(none)\t(none)"
        package = "libstdc++-static\t0:16.0-1.fc44.x86_64\tgcc-16.0-1.fc44.src.rpm"
        self.assertEqual(native.builder_rpm_records([key, package]), ([package], [key]))
        fixture = self.current()

        def with_signing_key(argv: Sequence[str], *, limit: int = native.MAX_REPORT) -> str:
            value = fixture.command(argv, limit=limit)
            return value + key + "\n" if argv[:3] == ["rpm", "--query", "--all"] else value

        with patch.object(native, "command", side_effect=with_signing_key):
            evidence = native.toolchain_evidence(fixture.root, fixture.runtime)
        self.assertEqual(evidence["builder_rpms"], [package])
        self.assertEqual(evidence["builder_signing_keys"], [key])
        self.assertEqual(evidence["static_cxx_owner"], package)

    def test_missing_source_rpm_is_not_a_general_package_exception(self) -> None:
        """Only the precise signing-key shape may omit source identity or architecture."""
        key = "gpg-pubkey\t0:36f612dcf27f7d1a48a835e4dbfcf71c6d9f90a6-6786af3b.(none)\t(none)"
        for value in (
            key.replace("gpg-pubkey", "libstdc++-static"),
            key.replace(".(none)", ".x86_64"),
            key.replace("36f612dcf27f7d1a48a835e4dbfcf71c6d9f90a6", "not-a-key"),
            key.replace("6786af3b", "invalid"),
            key.replace("\t(none)", "\tkey.src.rpm"),
            "ordinary\t0:1-1.x86_64\t(none)",
        ):
            with self.subTest(value=value), self.assertRaises(native.NativeError):
                _ = native.builder_rpm_records([value])
        with self.assertRaisesRegex(native.NativeError, "Duplicate"):
            _ = native.builder_rpm_records([key, key])

    def test_existing_receipt_cannot_be_overwritten(self) -> None:
        """Evidence destinations are exclusive; a previous success is never reused."""
        fixture = self.current()
        _ = self.produce()
        original = fixture.arguments.output.read_bytes()
        with self.assertRaises(FileExistsError):
            _ = self.produce()
        self.assertEqual(fixture.arguments.output.read_bytes(), original)

    def test_same_package_host_and_target_events_remain_distinct(self) -> None:
        """Cargo emits distinct output directories when host and target instances differ."""
        fixture = self.current()
        script = next(
            object_value(item).copy()
            for item in fixture.events
            if object_value(item)["reason"] == "build-script-executed"
        )
        script["out_dir"] = str(fixture.root / "target/release/build/other/out")
        fixture.events.insert(-1, script)
        fixture.save_events()
        graph = native.cargo_build(fixture.root, fixture.arguments.cargo_messages.read_bytes())
        self.assertEqual(len(graph.scripts), 4)
        fixture.events.insert(-1, script)
        fixture.save_events()
        with self.assertRaisesRegex(native.NativeError, "Duplicate native build script"):
            _ = native.cargo_build(fixture.root, fixture.arguments.cargo_messages.read_bytes())

    def test_repository_native_sources_match_current_pins(self) -> None:
        """The maintained real manifest has no missing, stale or unbound component records."""
        manifest, integrity = native.validate_manifest(ROOT)
        self.assertEqual(len(array_value(manifest["wrap_components"])), len(integrity.wraps))
        self.assertEqual(object_value(manifest["registry_component"])["name"], "AWS-LC")


if __name__ == "__main__":
    _ = unittest.main()
