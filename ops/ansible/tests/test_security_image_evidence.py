"""Bind scanner observations to native receipts, binary identities and the exact database."""

from __future__ import annotations

import copy
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
from urllib.parse import parse_qs, urlsplit

from test_support import ROOT

# isort: split
import security_image as runner
import security_image_policy as policy
import security_native as native_producer
from release_json import JsonObject, JsonValue, array_value, decode_json, object_value, string_value
from security_tools import ToolError
from test_security_native import Fixture

DIGEST = "a" * 64
LIBRARY = "/usr/lib64/libc.so.6"
CXX_LICENSE = "GPL-3.0-or-later WITH GCC-exception-3.1"


def rust_package(name: str, version: str = "1") -> JsonObject:
    """Represent a Syft binary-observed Rust artifact before source-license enrichment."""
    return {
        "id": name + "-" + version,
        "name": name,
        "version": version,
        "type": "rust-crate",
        "purl": "pkg:cargo/" + name + "@" + version,
        "licenses": [],
    }


def rust_evidence() -> tuple[list[JsonObject], JsonObject, JsonObject]:
    """Model local, registry and build-only source identities without runtime inference."""
    licenses: list[JsonValue] = [
        {
            "name": "simplestChat",
            "version": "1",
            "source": "local",
            "first_party": True,
            "cargo_publish": False,
            "license": {"expression": None, "license_file": None},
        },
        {
            "name": "serde",
            "version": "1",
            "source": "crates.io",
            "first_party": False,
            "license": {"expression": "MIT OR Apache-2.0", "license_file": None},
        },
        {
            "name": "mediasoup",
            "version": "1",
            "source": "local",
            "first_party": False,
            "license": {"expression": "ISC", "license_file": None},
        },
        {
            "name": "build-helper",
            "version": "1",
            "source": "crates.io",
            "first_party": False,
            "license": {"expression": "MIT", "license_file": None},
        },
    ]
    graph: list[JsonValue] = [
        {
            "name": item["name"],
            "version": item["version"],
            "source": item["source"],
            "kind": "build" if item["name"] == "build-helper" else "runtime",
        }
        for item in (object_value(value) for value in licenses)
    ]
    for value in licenses:
        object_value(value)["evidence"] = {"compilerArtifact": True, "embeddedMetadata": True}
    metadata: JsonObject = {
        "format": 1,
        "sha256": "a" * 64,
        "compressedSha256": "b" * 64,
        "packageCount": len(graph),
    }
    packages = [rust_package(name) for name in ("simplestChat", "serde", "mediasoup")]
    return (
        packages,
        {"rust_licenses": licenses, "rust_dependency_metadata": dict(metadata)},
        {"auditable": {**metadata, "packages": graph}},
    )


def rpm_package() -> JsonObject:
    """Use the recorded Syft 1.52 RPM source/file/digest shape, with synthetic file bytes."""
    return {
        "id": "glibc-observed",
        "name": "glibc",
        "version": "2.43-8.fc44",
        "type": "rpm",
        "purl": "pkg:rpm/fedora/glibc@2.43-8.fc44?arch=aarch64&distro=fedora-44",
        "licenses": [{"spdxExpression": "LGPL-2.1-or-later"}],
        "metadataType": "rpm-db-entry",
        "metadata": {
            "name": "glibc",
            "version": "2.43",
            "release": "8.fc44",
            "epoch": 0,
            "architecture": "aarch64",
            "sourceRpm": "glibc-2.43-8.fc44.src.rpm",
            "files": [{"path": LIBRARY, "digest": {"algorithm": "sha256", "value": DIGEST}}],
        },
    }


def native_inventory() -> JsonObject:
    """Keep native production, tests, Windows inputs and builder keys distinguishable."""
    return {
        "binary": {"sha256": DIGEST, "size": 123},
        "native_manifest_sha256": runner.digest(ROOT / "vendor/native-components.json"),
        "cargo_lock_sha256": runner.digest(ROOT / "Cargo.lock"),
        "static_archives": [{"sha256": DIGEST, "library": "worker"}],
        "wrap_components": [
            {"name": "abseil", "version": "1", "usage": "production", "license": "Apache-2.0"},
            {
                "name": "unordered_dense",
                "version": "1",
                "usage": "production",
                "license": "MIT",
                "linkage": "header-only",
            },
            {"name": "Catch2", "version": "1", "usage": "native-test", "license": "BSL-1.0"},
            {"name": "wingetopt", "version": "1", "usage": "windows-only", "license": "ISC"},
        ],
        "adapted_component": {
            "name": "libwebrtc-subset",
            "version": "m77",
            "license": "BSD-3-Clause",
        },
        "openssl": {"version": "3.5.8", "license": "Apache-2.0"},
        "registry_component": {"name": "AWS-LC", "version": "5.7.0", "license": "ISC AND MIT"},
        "toolchain": {
            "static_cxx_owner": "libstdc++-static\t0:16.2.1-2.fc44.aarch64\t"
            + "gcc-16.2.1-2.fc44.src.rpm",
            "static_cxx_license": CXX_LICENSE,
            "builder_signing_keys": ["gpg-pubkey-fixture-identity"],
        },
    }


def sbom(packages: list[JsonObject]) -> JsonObject:
    """Retain scanner provenance while including private config blobs to remove."""
    return {
        "descriptor": {"name": "syft", "version": "1.52.0"},
        "distro": {"id": "fedora", "versionID": "44"},
        "artifacts": list[JsonValue](packages),
        "source": {
            "metadata": {
                "imageID": "sha256:" + DIGEST,
                "manifestDigest": "sha256:" + DIGEST,
                "config": "PRIVATE-CONFIG-FIXTURE",
                "manifest": "PRIVATE-MANIFEST-FIXTURE",
            }
        },
    }


class RustEvidenceTests(unittest.TestCase):
    """A scanner crate name must join both binary metadata and its authenticated source."""

    def test_normalized_source_join_and_unpublished_first_party_policy(self) -> None:
        """Current receipt source enums join and build-only packages need not be runtime."""
        packages, native, elf = rust_evidence()
        policy.rust_license_join(packages, native, elf)
        expressions = {
            string_value(item["name"]): object_value(array_value(item["licenses"])[0])[
                "spdxExpression"
            ]
            for item in packages
        }
        self.assertEqual(
            expressions,
            {"simplestChat": "NOASSERTION", "serde": "MIT OR Apache-2.0", "mediasoup": "ISC"},
        )
        self.assertNotIn("build-helper", expressions)
        verdict = policy.license_verdict(
            packages, policy.load_policy(ROOT / "security/image-policy.json"), []
        )
        self.assertTrue(verdict["passed"])

    def test_real_native_producer_receipt_joins_without_schema_translation(self) -> None:
        """Exercise both maintained components so hand-written source enums cannot conceal drift."""
        with tempfile.TemporaryDirectory() as temporary:
            fixture = Fixture(Path(temporary))
            with (
                patch.object(native_producer, "SYSTEM_ROOT", fixture.system),
                patch.object(native_producer, "command", side_effect=fixture.command),
            ):
                native_producer.produce(fixture.arguments)
            native = object_value(decode_json(fixture.arguments.output.read_bytes()))
            metadata = native_producer.binary_dependencies(fixture.binary)[1]
        records = [object_value(item) for item in array_value(native["rust_licenses"])]
        packages = [
            rust_package(string_value(item["name"]), string_value(item["version"]))
            for item in records
        ]
        elf: JsonObject = {"auditable": metadata}
        policy.rust_license_join(packages, native, elf)
        app = next(item for item in packages if item["name"] == "simplestChat")
        self.assertEqual(
            object_value(array_value(app["licenses"])[0])["spdxExpression"], "NOASSERTION"
        )
        openssl = next(item for item in packages if item["name"] == "openssl-sys")
        self.assertEqual(object_value(array_value(openssl["licenses"])[0])["spdxExpression"], "MIT")
        worker = next(item for item in packages if item["name"] == "mediasoup-sys")
        self.assertEqual(worker["licenses"], [])

    def test_metadata_digest_or_coverage_mismatch_cannot_join(self) -> None:
        """A license receipt must bind this graph and acknowledge every observed node."""
        for key, changed in (
            ("sha256", "c" * 64),
            ("compressedSha256", "c" * 64),
            ("packageCount", 9),
            ("format", True),
        ):
            packages, native, elf = rust_evidence()
            object_value(native["rust_dependency_metadata"])[key] = changed
            with self.subTest(key=key), self.assertRaisesRegex(ToolError, "metadata_binding"):
                policy.rust_license_join(packages, native, elf)
        for key, changed in (("compilerArtifact", "true"), ("embeddedMetadata", False)):
            packages, native, elf = rust_evidence()
            record = object_value(array_value(native["rust_licenses"])[0])
            object_value(record["evidence"])[key] = changed
            with self.subTest(key=key), self.assertRaisesRegex(ToolError, "license_coverage"):
                policy.rust_license_join(packages, native, elf)

    def test_embedded_only_record_is_retained_in_policy_inventory(self) -> None:
        """Metadata-only evidence still requires a real license and remains policy-visible."""
        packages, native, elf = rust_evidence()
        record = object_value(array_value(native["rust_licenses"])[1])
        object_value(record["evidence"])["compilerArtifact"] = False
        policy.rust_license_join(packages, native, elf)
        self.assertEqual(len(packages), 3)
        serde = next(item for item in packages if item["name"] == "serde")
        self.assertEqual(
            object_value(array_value(serde["licenses"])[0])["spdxExpression"], "MIT OR Apache-2.0"
        )

    def test_source_mismatch_missing_and_ambiguous_license_records_fail(self) -> None:
        """Equal names and versions cannot borrow a license from a different source."""
        for source in (
            "local",
            "registry+https://github.com/rust-lang/crates.io-index",
            None,
            "git",
        ):
            with self.subTest(source=source):
                packages, native, elf = rust_evidence()
                object_value(array_value(native["rust_licenses"])[1])["source"] = source
                with self.assertRaisesRegex(ToolError, "license_source"):
                    policy.rust_license_join(packages, native, elf)
        for duplicate in (True, False):
            packages, native, elf = rust_evidence()
            records = array_value(native["rust_licenses"])
            if duplicate:
                records.append(copy.deepcopy(records[1]))
            else:
                del records[1]
            with self.assertRaisesRegex(ToolError, "license_source"):
                policy.rust_license_join(packages, native, elf)

    def test_unobserved_duplicate_or_missing_runtime_binary_identities_fail(self) -> None:
        """Neither an extra scanner package nor missing runtime coverage can look complete."""
        packages, native, elf = rust_evidence()
        packages.append(rust_package("unobserved"))
        with self.assertRaisesRegex(ToolError, "binary_identity"):
            policy.rust_license_join(packages, native, elf)
        packages, native, elf = rust_evidence()
        graph = array_value(object_value(elf["auditable"])["packages"])
        graph.append(copy.deepcopy(graph[1]))
        with self.assertRaisesRegex(ToolError, "binary_identity"):
            policy.rust_license_join(packages, native, elf)
        packages, native, elf = rust_evidence()
        with self.assertRaisesRegex(ToolError, "runtime_coverage"):
            policy.rust_license_join(packages[:-1], native, elf)

    def test_first_party_marker_requires_publication_evidence_and_exact_identity(self) -> None:
        """NOASSERTION is restricted to the exact unpublished first-party package."""
        for value in (True, None, "false"):
            packages, native, elf = rust_evidence()
            object_value(array_value(native["rust_licenses"])[0])["cargo_publish"] = value
            with self.subTest(value=value), self.assertRaisesRegex(ToolError, "first_party"):
                policy.rust_license_join(packages, native, elf)
        packages, native, elf = rust_evidence()
        record = object_value(array_value(native["rust_licenses"])[1])
        record.update(
            {"first_party": True, "cargo_publish": False, "license": {"expression": None}}
        )
        with self.assertRaisesRegex(ToolError, "first_party"):
            policy.rust_license_join(packages, native, elf)

    def test_unknown_third_party_license_is_preserved_and_blocked(self) -> None:
        """A verified license-file hash is evidence of terms, not an inferred SPDX license."""
        packages, native, elf = rust_evidence()
        record = object_value(array_value(native["rust_licenses"])[1])
        record["license"] = {
            "expression": None,
            "license_file": {"path": "LICENSE", "sha256": DIGEST},
        }
        policy.rust_license_join(packages, native, elf)
        self.assertEqual(packages[1]["licenses"], [])
        self.assertFalse(
            policy.license_verdict(
                packages, policy.load_policy(ROOT / "security/image-policy.json"), []
            )["passed"]
        )


class NativeEvidenceTests(unittest.TestCase):
    """Static provenance and image observations keep distinct applicability and identities."""

    def test_only_production_native_inputs_enter_inventory(self) -> None:
        """Native tests, Windows-only inputs and builder signing keys are not runtime packages."""
        packages = policy.native_packages(native_inventory())
        names = {string_value(item["name"]) for item in packages}
        self.assertEqual(
            names,
            {
                "abseil",
                "unordered_dense",
                "libwebrtc-subset",
                "OpenSSL",
                "AWS-LC",
                "libstdc++-static",
            },
        )
        for item in packages:
            if item["name"] != "libstdc++-static":
                self.assertEqual(item["type"], "binary")
                self.assertEqual(item["cpes"], [])
                self.assertEqual(item["purl"], "")
            self.assertEqual(
                object_value(array_value(item["locations"])[0])["path"], "/app/simplestChat"
            )

    def test_static_cxx_uses_exact_fedora_rpm_identity_and_declared_license(self) -> None:
        """Static libstdc++ retains RPM identity without inventing one for generic inputs."""
        runtime = next(
            item
            for item in policy.native_packages(native_inventory())
            if item["name"] == "libstdc++-static"
        )
        self.assertEqual(runtime["type"], "rpm")
        self.assertEqual(runtime["version"], "16.2.1-2.fc44")
        self.assertEqual(runtime["metadataType"], "rpm-db-entry")
        metadata = object_value(runtime["metadata"])
        self.assertEqual(
            metadata,
            {
                "name": "libstdc++-static",
                "version": "16.2.1",
                "release": "2.fc44",
                "epoch": 0,
                "architecture": "aarch64",
                "sourceRpm": "gcc-16.2.1-2.fc44.src.rpm",
                "files": [],
            },
        )
        purl = urlsplit(string_value(runtime["purl"]))
        self.assertEqual(purl.path, "rpm/fedora/libstdc%2B%2B-static@16.2.1-2.fc44")
        self.assertEqual(
            parse_qs(purl.query),
            {
                "arch": ["aarch64"],
                "distro": ["fedora-44"],
                "upstream": ["gcc-16.2.1-2.fc44.src.rpm"],
            },
        )
        self.assertEqual(
            object_value(array_value(runtime["licenses"])[0])["spdxExpression"], CXX_LICENSE
        )

    def test_invalid_static_rpm_identity_is_not_accepted_as_generic_inventory(self) -> None:
        """Architecture and source-package corruption fail before advisory identity construction."""
        original = string_value(object_value(native_inventory()["toolchain"])["static_cxx_owner"])
        for owner in (
            original.replace(".aarch64", ".unknown"),
            original.replace("gcc-16.2.1-2.fc44.src.rpm", "(none)"),
            original.replace("0:", "invalid:"),
        ):
            native = native_inventory()
            object_value(native["toolchain"])["static_cxx_owner"] = owner
            with self.subTest(owner=owner), self.assertRaisesRegex(ToolError, "static_rpm_nevra"):
                _ = policy.native_packages(native)

    def test_sbom_enrichment_retains_observations_and_removes_private_blobs(self) -> None:
        """The SBOM retains provenance and observed packages without copying image config."""
        packages, native, elf = rust_evidence()
        native.update(native_inventory())
        packages.append(rpm_package())
        value = policy.enrich_sbom(
            sbom(packages), native, elf, policy.load_policy(ROOT / "security/image-policy.json")
        )
        observed_ids = {string_value(item["id"]) for item in packages}
        actual_ids = {
            string_value(object_value(item)["id"]) for item in array_value(value["artifacts"])
        }
        self.assertTrue(observed_ids < actual_ids)
        metadata = object_value(object_value(value["source"])["metadata"])
        self.assertEqual(metadata["imageID"], "sha256:" + DIGEST)
        self.assertNotIn("PRIVATE-", json.dumps(value))
        self.assertNotIn("config", metadata)
        self.assertNotIn("manifest", metadata)

    def test_native_identity_collision_fails_enrichment(self) -> None:
        """Supplemental component IDs cannot silently overwrite a scanner observation."""
        packages, native, elf = rust_evidence()
        native.update(native_inventory())
        observed = rpm_package()
        observed["id"] = policy.native_packages(native)[0]["id"]
        packages.append(observed)
        with self.assertRaisesRegex(ToolError, "native_package_collision"):
            _ = policy.enrich_sbom(
                sbom(packages), native, elf, policy.load_policy(ROOT / "security/image-policy.json")
            )

    def test_native_receipt_must_bind_the_same_binary_and_have_inventory(self) -> None:
        """An installed receipt must still identify this exact binary and its native inputs."""
        with tempfile.TemporaryDirectory() as temporary:
            tree = Path(temporary)
            path = tree / "rootfs/usr/share/simplestchat/native-components.json"
            path.parent.mkdir(parents=True)
            receipt = native_inventory()
            _ = path.write_text(json.dumps(receipt))
            self.assertEqual(
                runner.native_binding(tree, {"binarySha256": DIGEST, "binaryBytes": 123}), receipt
            )
            with self.assertRaisesRegex(ToolError, "binary_mismatch"):
                _ = runner.native_binding(tree, {"binarySha256": "b" * 64, "binaryBytes": 123})
            with self.assertRaisesRegex(ToolError, "binary_mismatch"):
                _ = runner.native_binding(tree, {"binarySha256": DIGEST, "binaryBytes": 124})
            for key in ("native_manifest_sha256", "cargo_lock_sha256"):
                _ = path.write_text(json.dumps({**receipt, key: "b" * 64}))
                with self.subTest(key=key), self.assertRaisesRegex(ToolError, "source_mismatch"):
                    _ = runner.native_binding(tree, {"binarySha256": DIGEST, "binaryBytes": 123})
            receipt["static_archives"] = []
            _ = path.write_text(json.dumps(receipt))
            with self.assertRaisesRegex(ToolError, "inventory_missing"):
                _ = runner.native_binding(tree, {"binarySha256": DIGEST, "binaryBytes": 123})


class RuntimeAndDatabaseTests(unittest.TestCase):
    """Actual image library bytes and the vulnerability database have independent identities."""

    def test_runtime_library_has_one_matching_source_rpm_and_digest(self) -> None:
        """Only the exact library path and SHA-256 tie the ELF closure to an RPM owner."""
        package = rpm_package()
        elf: JsonObject = {"libraries": [{"path": LIBRARY, "sha256": DIGEST}]}
        self.assertEqual(
            policy.runtime_rpm_bindings([package], elf),
            [
                {
                    "path": LIBRARY,
                    "sha256": DIGEST,
                    "package": package["purl"],
                    "sourceRpm": "glibc-2.43-8.fc44.src.rpm",
                }
            ],
        )
        for algorithm, digest in (("sha1", DIGEST), ("sha256", "b" * 64)):
            altered = rpm_package()
            object_value(array_value(object_value(altered["metadata"])["files"])[0])["digest"] = {
                "algorithm": algorithm,
                "value": digest,
            }
            with self.subTest(algorithm=algorithm), self.assertRaisesRegex(ToolError, "rpm_digest"):
                _ = policy.runtime_rpm_bindings([altered], elf)

    def test_missing_duplicate_owner_or_empty_library_closure_fails(self) -> None:
        """Shared-library coverage cannot accept an unowned file or competing RPM records."""
        elf: JsonObject = {"libraries": [{"path": LIBRARY, "sha256": DIGEST}]}
        for packages in ([], [rpm_package(), rpm_package()]):
            with self.assertRaisesRegex(ToolError, "rpm_owner"):
                _ = policy.runtime_rpm_bindings(packages, elf)
        package = rpm_package()
        object_value(array_value(object_value(package["metadata"])["files"])[0])["path"] = (
            "/usr/lib64/other.so"
        )
        with self.assertRaisesRegex(ToolError, "rpm_owner"):
            _ = policy.runtime_rpm_bindings([package], elf)
        with self.assertRaisesRegex(ToolError, "rpm_empty"):
            _ = policy.runtime_rpm_bindings([rpm_package()], {"libraries": []})

    def test_vulnerability_scan_uses_exact_validated_database_and_no_filters(self) -> None:
        """Current Grype descriptor fields bind the offline scan to preparation evidence."""
        database: JsonObject = {
            "schemaVersion": "v6.1.9",
            "from": "https://grype.anchore.io/databases/v6/fixture?checksum=sha256%3A" + DIGEST,
            "built": "2026-09-30T06:32:47Z",
            "valid": True,
        }
        descriptor: JsonObject = {
            "db": {
                "status": copy.deepcopy(database),
                "providers": {"fedora": {}, "github": {}, "nvd": {}},
            },
            "configuration": {"only-fixed": False, "only-notfixed": False, "ignore-wontfix": ""},
        }
        policy.vulnerability_database_binding({"descriptor": descriptor}, database)
        for key, value in database.items():
            changed = copy.deepcopy(descriptor)
            object_value(object_value(changed["db"])["status"])[key] = (
                not value if type(value) is bool else "different"
            )
            with self.subTest(key=key), self.assertRaisesRegex(ToolError, "database_changed"):
                policy.vulnerability_database_binding({"descriptor": changed}, database)
        for provider in ("fedora", "github", "nvd"):
            changed = copy.deepcopy(descriptor)
            del object_value(object_value(changed["db"])["providers"])[provider]
            with self.assertRaisesRegex(ToolError, "providers_missing"):
                policy.vulnerability_database_binding({"descriptor": changed}, database)
        for flag in ("only-fixed", "only-notfixed", "ignore-wontfix"):
            changed = copy.deepcopy(descriptor)
            object_value(changed["configuration"])[flag] = (
                True if flag != "ignore-wontfix" else "High"
            )
            with self.assertRaisesRegex(ToolError, "vulnerability_filter"):
                policy.vulnerability_database_binding({"descriptor": changed}, database)
