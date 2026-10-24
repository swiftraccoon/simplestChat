"""Run unchanged offline release preparation on bounded, inert file fixtures.

The actual archive, hash, copy and trust functions determine every verdict. Only
the UTC date and named copy-I/O faults are adapted. No process, network, engine,
signature verification or host action occurs. Exported inputs stay pristine:
the reference runs on a separate private copy.
"""

from __future__ import annotations

import argparse
import errno
import hashlib
import json
import os
import shutil
import stat
import tempfile
from datetime import UTC, datetime, tzinfo
from pathlib import Path
from typing import TYPE_CHECKING, final, override
from unittest.mock import patch

from test_release_fetch import fixture_files
from test_support import ROOT

# isort: split
import release_attestation as attest
import release_trust as trust
import security_archive
from release_json import JsonObject, JsonValue, decode_json, object_value

if TYPE_CHECKING:
    from collections.abc import Sequence
    from typing import BinaryIO

_ = ROOT
TODAY = "2050-01-01"
ORIGINAL = ("image.tar", "release.json", "outcome.json", "source.json")
COPIES = (
    "sbom.spdx.json",
    "image-security.json",
    "runtime-proof.json",
    "vex.openvex.json",
)
EVIDENCE = (
    ("sbomSha256", "spdx/sbom.spdx.json", "sbom"),
    ("nativeSha256", "native.json", "native"),
    ("elfSha256", "elf.json", "elf"),
    ("secretPathMapSha256", "secret-paths.json", "secret-paths"),
    ("databaseEvidenceSha256", "database-status.json", "database-status"),
    ("runtimeProofSha256", "runtime-proof.json", "runtime-proof"),
    ("vexSha256", "vex.openvex.json", "vex"),
    (
        "runtimeLicenseEvidenceSha256",
        "runtime-license-evidence.json",
        "runtime-licenses",
    ),
)
HASH_ONLY = (
    "native.json",
    "elf.json",
    "secret-paths.json",
    "database-status.json",
    "runtime-license-evidence.json",
)
MAX_FIXTURE_BYTES = 2 * 1024**2
MAX_FIXTURE_FILES = 32
MAX_OUTPUT_BYTES = 65536

SCENARIOS = (
    "success",
    "required",
    "opaque-source",
    "opaque-sbom",
    "hash-only-empty",
    "hash-only-raw",
    "hash-only-symlink",
    "copied-symlink",
    "encoding-security-bom",
    "encoding-security-utf16",
    "encoding-runtime-utf32",
    "ignored-large-integer",
    "ignored-bool-null",
    "ignored-surrogate",
    "ignored-depth",
    "ignored-values",
    "security-nonfinite",
    "security-duplicate",
    "security-passed-integer",
    "self-test-integer",
    "checks-missing",
    "checks-extra",
    "check-passed-integer",
    "selected-config",
    "image-id",
    "security-revision",
    "security-archive",
    "security-platform",
    "build-passed",
    "build-revision",
    "build-image",
    "hash-sbom",
    "hash-native",
    "hash-elf",
    "hash-secret-paths",
    "hash-database-status",
    "hash-runtime-proof",
    "hash-vex",
    "hash-runtime-licenses",
    "conflict-sbom",
    "conflict-image-security",
    "conflict-runtime-proof",
    "conflict-vex",
    "conflict-predicate",
    "copy-read-sbom",
    "copy-read-image-security",
    "copy-read-runtime-proof",
    "copy-read-vex",
    "copy-write-sbom",
    "copy-write-image-security",
    "copy-write-runtime-proof",
    "copy-write-vex",
    "runtime-required-integer",
    "runtime-schema-boolean",
    "runtime-passed-integer",
    "runtime-required-mismatch",
    "runtime-image",
    "runtime-archive",
    "runtime-revision",
    "runtime-platform",
    "optional-vex-nonempty",
    "required-vex-empty",
    "required-vex-multiple",
    "required-scope",
    "required-binary-digest",
    "required-native-digest",
    "required-elf-digest",
    "required-sbom-digest",
    "expiry-today",
    "expiry-yesterday",
    "expiry-invalid",
    "expiry-unicode",
)
COPY_LABELS = ("sbom", "image-security", "runtime-proof", "vex")
PROFILE = {
    "hash-only-symlink": "nofollow-hash-only",
    "ignored-surrogate": "unicode-scalar",
    "ignored-depth": "depth-128",
    "ignored-values": "values-100000",
}


@final
class FixedDateTime(datetime):
    """Keep real calendar parsing and the reference's exclusive UTC date check."""

    @classmethod
    @override
    def now(cls, tz: tzinfo | None = None) -> FixedDateTime:
        return cls(2050, 1, 1, tzinfo=UTC).astimezone(tz)


def encoded(value: JsonValue) -> bytes:
    """Use ordinary reference JSON spelling, including escaped opaque text."""
    return json.dumps(value).encode()


def digest(data: bytes) -> str:
    """Pin exact fixture or retained bytes."""
    return hashlib.sha256(data).hexdigest()


def write(path: Path, data: bytes) -> None:
    """Create only a fresh fixture member; never replace an original input."""
    with path.open("xb") as output:
        _ = output.write(data)


def required(scenario: str) -> bool:
    """Choose the fixed conditional-proof fixture before applying one mutation."""
    return scenario == "required" or scenario.startswith(("required-", "expiry-"))


def conditional_fixture(
    scenario: str,
    result: JsonObject,
    proof: JsonObject,
    vex: JsonObject,
    evidence: dict[str, bytes],
) -> None:
    """Bind one valid required proof before its selected mutation."""
    if required(scenario):
        result["vexRequired"] = True
        result["binarySha256"] = "b" * 64
        proof.update(
            required=True,
            scope="managed-default-server-dtls",
            sourceReview={"expires": "2050-01-02"},
        )
        for key, name in (
            ("nativeSha256", "native.json"),
            ("elfSha256", "elf.json"),
            ("sbomSha256", "spdx/sbom.spdx.json"),
        ):
            proof[key] = digest(evidence[name])
        proof["binarySha256"] = result["binarySha256"]
        vex["statements"] = [{"status": "not_affected"}]


def evidence_cases(scenario: str, security: Path, evidence: dict[str, bytes]) -> None:
    """Keep opaque and symlink cases in the exact reference input domain."""
    if scenario == "opaque-sbom":
        evidence["spdx/sbom.spdx.json"] = b"opaque SBOM bytes; no SPDX parser at preparation\n"
    if scenario in {"hash-only-empty", "hash-only-raw"}:
        evidence.update(
            dict.fromkeys(HASH_ONLY, b"" if scenario.endswith("empty") else b"\x00\xffnot JSON\n")
        )
    if scenario in {"hash-only-symlink", "copied-symlink"}:
        name = "native.json" if scenario == "hash-only-symlink" else "spdx/sbom.spdx.json"
        target = security / "inert-evidence-target"
        write(target, evidence[name])
        del evidence[name]
        (security / name).symlink_to(os.path.relpath(target, (security / name).parent))


def record_cases(scenario: str, result: JsonObject, build: JsonObject, proof: JsonObject) -> None:
    """Apply one closed security, build or runtime identity mutation."""
    changes: dict[str, tuple[JsonObject, str, JsonValue]] = {
        "security-passed-integer": (result, "passed", 1),
        "self-test-integer": (result, "secretDetectorSelfTest", 1),
        "selected-config": (result, "selectedImage", {"archiveConfigSha256": "f" * 64}),
        "image-id": (result, "imageId", "sha256:" + "f" * 64),
        "security-revision": (result, "revision", "f" * 40),
        "security-archive": (result, "archiveSha256", "f" * 64),
        "security-platform": (result, "platform", "linux/arm64"),
        "build-passed": (build, "passed", False),
        "build-revision": (build, "revision", "f" * 40),
        "build-image": (build, "exportedImageId", "sha256:" + "f" * 64),
        "runtime-required-integer": (result, "vexRequired", 1),
        "runtime-schema-boolean": (proof, "schemaVersion", True),
        "runtime-passed-integer": (proof, "passed", 1),
        "runtime-required-mismatch": (proof, "required", True),
        "runtime-image": (proof, "imageId", "sha256:" + "f" * 64),
        "runtime-archive": (proof, "archiveSha256", "f" * 64),
        "runtime-revision": (proof, "revision", "f" * 40),
        "runtime-platform": (proof, "platform", "linux/arm64"),
        "required-scope": (proof, "scope", "all-programs"),
    }
    if scenario in changes:
        target, key, value = changes[scenario]
        target[key] = value
    checks = object_value(result["checks"])
    if scenario == "checks-missing":
        del checks["secrets"]
    if scenario == "checks-extra":
        checks["extra"] = {"passed": True}
    if scenario == "check-passed-integer":
        object_value(checks["secrets"])["passed"] = 1


def runtime_cases(scenario: str, proof: JsonObject, vex: JsonObject) -> None:
    """Select disposition cardinality, conditional digests and real date cases."""
    if scenario == "optional-vex-nonempty":
        vex["statements"] = [{}]
    if scenario in {"required-vex-empty", "required-vex-multiple"}:
        vex["statements"] = [] if scenario.endswith("empty") else [{}, {}]
    if scenario.startswith("required-") and scenario.endswith("-digest"):
        part = scenario.removeprefix("required-").removesuffix("-digest")
        proof[part + "Sha256"] = "f" * 64
    if scenario.startswith("expiry-"):
        expires = {
            "today": TODAY,
            "yesterday": "2049-12-31",
            "invalid": "2050-02-30",
            "unicode": "\uff12\uff10\uff15\uff10-\uff10\uff11-\uff10\uff12",
        }
        object_value(proof["sourceReview"])["expires"] = expires[scenario.removeprefix("expiry-")]


def extra_values(scenario: str, result: JsonObject) -> None:
    """Preserve or explicitly identify the ignored JSON-value domain."""
    if scenario == "ignored-large-integer":
        result["ignored"] = 10**200 + 1
    if scenario == "ignored-bool-null":
        result["ignored"] = {
            "yes": True,
            "no": False,
            "none": None,
            "integer": 9007199254740993,
        }
    if scenario == "ignored-surrogate":
        result["ignored"] = "\ud800"
    if scenario == "ignored-depth":
        nested: JsonValue = None
        for _ in range(140):
            nested = [nested]
        result["ignored"] = nested
    if scenario == "ignored-values":
        values: list[JsonValue] = [None] * 100_001
        result["ignored"] = values
    if scenario == "security-nonfinite":
        result["ignored"] = float("nan")


def security_bytes(scenario: str, result: JsonObject) -> bytes:
    """Use the byte decoder profile, including fixed encoding and ambiguity cases."""
    result_bytes = encoded(result)
    if scenario == "encoding-security-bom":
        result_bytes = b"\xef\xbb\xbf" + result_bytes
    if scenario == "encoding-security-utf16":
        result_bytes = result_bytes.decode().encode("utf-16")
    if scenario == "security-duplicate":
        result_bytes = b'{"passed":true,' + result_bytes[1:]
    return result_bytes


def fixture(base: Path, scenario: str) -> tuple[Path, Path, JsonObject]:
    """Construct actual Docker-save metadata and matching scanner/runtime evidence."""
    artifact, security = base / "artifact", base / "security"
    artifact.mkdir(mode=0o700)
    (security / "spdx").mkdir(mode=0o700, parents=True)
    files = fixture_files()
    # The shared fixture's revision is deliberately read rather than assumed.
    manifest = object_value(decode_json(files["release.json"]))
    selection = attest.selectors("owner/repo", str(manifest["revision"]), 789, 1)
    write(artifact / "image.tar", files["image.tar"])
    image = security_archive.image_identity(artifact / "image.tar")
    build = object_value(decode_json(files["outcome.json"]))
    build["exportedImageId"] = image
    result = object_value(decode_json(files["image-security.json"]))
    result.update(
        imageId=image,
        selectedImage={"archiveConfigSha256": image.removeprefix("sha256:")},
        secretDetectorSelfTest=True,
        checks={name: {"passed": True} for name in ("vulnerabilities", "licenses", "secrets")},
    )
    proof = object_value(decode_json(files["runtime-proof.json"]))
    proof["imageId"] = image
    vex: JsonObject = {"statements": []}
    evidence: dict[str, bytes] = dict.fromkeys(HASH_ONLY, b"{}")
    evidence["spdx/sbom.spdx.json"] = files["sbom.spdx.json"]
    source = files["source.json"]
    conditional_fixture(scenario, result, proof, vex, evidence)
    if scenario == "opaque-source":
        source = b"opaque source bytes; no JSON schema at preparation\n"
    evidence_cases(scenario, security, evidence)
    record_cases(scenario, result, build, proof)
    runtime_cases(scenario, proof, vex)
    extra_values(scenario, result)
    proof_bytes = encoded(proof)
    evidence["runtime-proof.json"] = (
        proof_bytes.decode().encode("utf-32")
        if scenario == "encoding-runtime-utf32"
        else proof_bytes
    )
    evidence["vex.openvex.json"] = encoded(vex)
    for name, data in evidence.items():
        write(security / name, data)
    for field, name, label in EVIDENCE:
        result[field] = digest((security / name).read_bytes())
        if scenario == "hash-" + label:
            result[field] = "f" * 64
    result_bytes = security_bytes(scenario, result)
    write(security / "outcome.json", result_bytes)
    write(artifact / "release.json", files["release.json"])
    write(artifact / "outcome.json", encoded(build))
    write(artifact / "source.json", source)
    for index, label in enumerate(COPY_LABELS):
        if scenario == "conflict-" + label:
            write(artifact / COPIES[index], b"existing destination remains unchanged\n")
    if scenario == "conflict-predicate":
        write(artifact / trust.PREDICATE, b"existing predicate remains unchanged\n")
    return artifact, security, selection


@final
class ReadFault:
    """The real copy loop writes three bytes, then receives one fixed read error."""

    def __init__(self, source: BinaryIO) -> None:
        """Borrow the already-owned original reader without adding a file handle."""
        self.source = source
        self.read_once = False

    def read(self, _size: int = -1) -> bytes:
        """Return one real prefix, then fail the next copy-loop read."""
        if self.read_once:
            raise OSError(errno.EIO, "inert copy read fault")
        self.read_once = True
        return self.source.read(3)


@final
class WriteFault:
    """Retain a real three-byte destination prefix before one fixed write error."""

    def __init__(self, output: BinaryIO) -> None:
        """Borrow the original create-new destination owned by the reference."""
        self.output = output

    def write(self, data: bytes) -> int:
        """Write a real prefix before raising the fixed synthetic I/O refusal."""
        _ = self.output.write(data[:3])
        raise OSError(errno.EIO, "inert copy write fault")


def files(directory: Path) -> JsonObject:
    """Observe actual retained artifact bytes without parsing partial JSON."""
    result: JsonObject = {}
    for path in sorted(directory.iterdir()):
        data = path.read_bytes()
        result[path.name] = {
            "sha256": digest(data),
            "bytes": len(data),
            "mode": stat.S_IMODE(path.stat().st_mode),
        }
    return result


def originals(directory: Path) -> dict[str, tuple[int, int, int, int, str]]:
    """Detect replacement, mode/size changes and in-place writes to original inputs."""
    result: dict[str, tuple[int, int, int, int, str]] = {}
    for name in ORIGINAL:
        path = directory / name
        metadata = path.stat()
        result[name] = (
            metadata.st_dev,
            metadata.st_ino,
            metadata.st_mode,
            metadata.st_mtime_ns,
            digest(path.read_bytes()),
        )
    return result


def export_inputs(base: Path, output: Path) -> None:
    """Export only bounded test inputs, refusing existing or noncanonical destinations."""
    if (
        not output.is_absolute()
        or output.parent.resolve() != output.parent
        or output.exists()
        or output.is_symlink()
    ):
        message = "oracle_fixture_directory"
        raise ValueError(message)
    members = [path for path in base.rglob("*") if path.is_file() and not path.is_symlink()]
    if (
        len(members) > MAX_FIXTURE_FILES
        or sum(path.stat().st_size for path in members) > MAX_FIXTURE_BYTES
    ):
        message = "oracle_fixture_size"
        raise ValueError(message)
    output.mkdir(mode=0o700)
    for name in ("artifact", "security"):
        _ = shutil.copytree(base / name, output / name, symlinks=True)


def copy_fault(scenario: str) -> JsonObject | None:
    """Select only a named copy index and a three-byte read/write adapter."""
    for kind in ("read", "write"):
        for index, label in enumerate(COPY_LABELS):
            if scenario == f"copy-{kind}-{label}":
                return {"kind": "copy-" + kind, "copy": index, "retainedBytes": 3}
    return None


def run_scenario(scenario: str, fixture_directory: Path | None = None) -> JsonObject:
    """Execute one closed whole reference case; never accept an external success receipt."""
    if scenario not in SCENARIOS:
        message = "oracle_scenario"
        raise ValueError(message)
    previous_umask = os.umask(0o077)
    try:
        with tempfile.TemporaryDirectory(prefix="release-prepare-oracle-") as temporary:
            base = Path(temporary)
            artifact, security, selection = fixture(base, scenario)
            before = originals(artifact)
            if fixture_directory is not None:
                export_inputs(base, fixture_directory)
            copy_order: list[JsonValue] = []
            fault = copy_fault(scenario)
            original_copy = shutil.copyfileobj

            def copy(source: BinaryIO, output: BinaryIO, length: int = 0) -> None:
                index = len(copy_order)
                copy_order.append(COPIES[index])
                if fault is not None and fault["copy"] == index:
                    if fault["kind"] == "copy-read":
                        original_copy(ReadFault(source), output, length)
                    else:
                        original_copy(source, WriteFault(output), length)
                else:
                    original_copy(source, output, length)

            passed = False
            predicate: JsonObject | None = None
            with (
                patch.object(trust, "datetime", FixedDateTime),
                patch.object(shutil, "copyfileobj", copy),
            ):
                try:
                    predicate = attest.prepare(artifact, security, selection)
                except (ValueError, OSError, KeyError):
                    pass
                else:
                    passed = True
            predicate_path = artifact / trust.PREDICATE
            observation: JsonObject = {
                "passed": passed,
                "files": files(artifact),
                "originalUnchanged": before == originals(artifact),
                "copyOrder": copy_order,
                "failureClass": None if passed else "release_attestation_failed",
                "predicate": predicate,
                "predicateSha256": digest(predicate_path.read_bytes()) if passed else None,
            }
            return {
                "schemaVersion": 1,
                "scenario": scenario,
                "input": {
                    "selection": selection,
                    "today": TODAY,
                    "fault": fault,
                    "rustProfile": PROFILE.get(scenario),
                },
                "observation": observation,
            }
    finally:
        _ = os.umask(previous_umask)


class Options(argparse.Namespace):
    """Only the two closed oracle selectors cross the CLI boundary."""

    scenario: str = ""
    fixture_directory: Path | None = None


def main(argv: Sequence[str] | None = None) -> int:
    """Print one bounded ordinary JSON result; an unknown scenario is a usage error."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("--scenario", required=True, choices=SCENARIOS)
    _ = parser.add_argument("--fixture-directory", type=Path)
    options = parser.parse_args(argv, namespace=Options())
    result = run_scenario(options.scenario, options.fixture_directory)
    output = json.dumps(result, sort_keys=True, allow_nan=False) + "\n"
    if len(output.encode()) > MAX_OUTPUT_BYTES:
        message = "oracle_output_size"
        raise ValueError(message)
    print(output, end="")  # noqa: T201 -- Closed test-oracle protocol, no private input values.
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
