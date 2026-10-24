"""Fixed assertions for the signed-musl receive route in one marked guest.

Only closed hashes, counts, inode facts and fixed case labels leave the guest.
Raw frames, signed targets, private filenames and arbitrary errors never do.
"""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import os
import re
import stat
import sys
from pathlib import Path
from typing import cast

sys.path.insert(0, str(Path(__file__).resolve().parent))
import security_vm_fetch_guest as positive
import security_vm_receive_route as route

sys.path.insert(0, "/usr/local/libexec/simplestchat-public")
import bounded_process

ROOT = Path("/root/simplestchat-vm-fixture")
PUBLIC = Path("/srv/simplestchat-public")
WORK = Path("/run/simplestchat-bench")
LIBRARY = Path("/usr/local/libexec/simplestchat-public")
RECEIVER = LIBRARY / "fetch-release.py"
BACKUP = ROOT / "receive-image.original"
CONFIG = ROOT / "receive-proof.json"
RECORD = 65536
DIRECTORY_MODE = 0o700
CURL_FIRST_WORDS = 2
CURL_MINIMUM = (8, 4)
FIELDS = frozenset(
    {
        "schemaVersion",
        "status",
        "passed",
        "settled",
        "phase",
        "failureClass",
        "evidence",
        "revision",
        "artifactId",
        "zipSha256",
        "artifactZipBytes",
        "downloadedBytes",
        "archiveSha256",
        "manifestSha256",
        "elapsedSeconds",
        "finalized",
    }
)
MEMBERS = frozenset(
    {
        "image.tar",
        "release.json",
        "outcome.json",
        "source.json",
        "image-security.json",
        "sbom.spdx.json",
        "runtime-proof.json",
        "vex.openvex.json",
        "release-predicate.json",
        "release-attestation.jsonl",
    }
)


def require(condition: object) -> None:
    """Use only one fixed private-fixture failure."""
    if not condition:
        message = "receive_vm_guest_unproven"
        raise ValueError(message)


def record(data: bytes) -> dict[str, object]:
    """Keep the mature bounded duplicate-free passive decoder."""
    require(len(data) <= RECORD)
    return positive.object_json(data)


def encoded(value: object) -> bytes:
    """Use one stable private JSON spelling."""
    return json.dumps(value, separators=(",", ":"), allow_nan=False).encode() + b"\n"


def checked_file(path: Path, maximum: int, mode: int = 0o600) -> tuple[dict[str, object], bytes]:
    """Read only an original root-owned regular inode, never following links."""
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as source:
        original = os.fstat(source.fileno())
        require(
            stat.S_ISREG(original.st_mode)
            and original.st_uid == 0
            and stat.S_IMODE(original.st_mode) == mode
            and 0 < original.st_size <= maximum
            and positive.stamp(path.lstat()) == positive.stamp(original)
        )
        digest, total, data = hashlib.sha256(), 0, bytearray()
        while chunk := source.read(65536):
            total += len(chunk)
            require(total <= maximum)
            digest.update(chunk)
            if maximum <= RECORD:
                data.extend(chunk)
        require(
            total == original.st_size
            and positive.stamp(os.fstat(source.fileno())) == positive.stamp(original)
            and positive.stamp(path.lstat()) == positive.stamp(original)
        )
        pin: dict[str, object] = {
            "sha256": digest.hexdigest(),
            "bytes": total,
            "device": original.st_dev,
            "inode": original.st_ino,
        }
    return pin, bytes(data)


def write_new(path: Path, data: bytes, mode: int = 0o600) -> os.stat_result:
    """Create one exclusive fixed fixture leaf and durably close its original."""
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, "wb") as output:
        require(output.write(data) == len(data))
        output.flush()
        os.fchmod(output.fileno(), mode)
        os.fsync(output.fileno())
        original = os.fstat(output.fileno())
        require(positive.stamp(path.lstat()) == positive.stamp(original))
    return original


def remove_original(path: Path, original: os.stat_result) -> None:
    """Remove only the exact fixture inode after its expected byte seal."""
    require(positive.stamp(path.lstat()) == positive.stamp(original))
    path.unlink()


def admission(value: dict[str, object]) -> tuple[str, str]:
    """All paths and phase names are compiled, only immutable identities are inputs."""
    require(os.geteuid() == 0)
    run_id, revision = value.get("runId"), value.get("revision")
    require(
        isinstance(run_id, str)
        and re.fullmatch("[a-f0-9]{32}", run_id)
        and isinstance(revision, str)
        and re.fullmatch("[a-f0-9]{40}", revision)
    )
    _, marker = checked_file(route.MARKER, 4096)
    require(marker == (str(run_id) + "\n").encode())
    return str(run_id), str(revision)


def installed(value: dict[str, object]) -> None:
    """Correlate the installed original to the controller's authenticated pins."""
    pin, _ = checked_file(route.BINARY, route.MAX_BINARY, 0o755)
    require(pin["sha256"] == value.get("binarySha256") and pin["bytes"] == value.get("binaryBytes"))


def curl_version(data: bytes) -> str:
    """Project only a numeric version after independent fixed backend/protocol guards."""
    text = data.decode("ascii")
    lines = text.splitlines()
    first = lines[0].split() if lines else []
    require(
        len(first) >= CURL_FIRST_WORDS
        and first[0] == "curl"
        and re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", first[1])
    )
    major, minor, _ = (int(item) for item in first[1].split("."))
    require(
        (major, minor) >= CURL_MINIMUM
        and any(item.startswith("OpenSSL/") for item in first)
        and any(
            line.startswith("Protocols:") and "https" in line.split()[1:]
            for line in text.splitlines()
        )
    )
    return first[1]


def host_tools() -> dict[str, object]:
    """Measure only fixed Debian curl/CA originals, without a credential or target."""
    curl = Path("/usr/bin/curl")
    ca = Path("/etc/ssl/certs/ca-certificates.crt")
    original, _ = checked_file(curl, route.MAX_BINARY, 0o755)
    trust, _ = checked_file(ca, 16 * 1024**2, 0o644)
    status, output, error = bounded_process.run(
        [str(curl), "--disable", "--version"],
        env={"PATH": "/usr/bin:/bin", "LC_ALL": "C"},
        limits=bounded_process.Limits(timeout=10, stdout=16384, stderr=16384),
    )
    require(status == 0 and not error)
    version = curl_version(output)
    observed, _ = checked_file(curl, route.MAX_BINARY, 0o755)
    trust_after, _ = checked_file(ca, 16 * 1024**2, 0o644)
    require(observed == original and trust_after == trust)
    return {
        "sha256": original["sha256"],
        "bytes": original["bytes"],
        "version": version,
        "opensslAdvertised": True,
        "caSha256": trust["sha256"],
        "caBytes": trust["bytes"],
    }


def install_route(value: dict[str, object]) -> dict[str, object]:
    """Replace only this marked fixture's original Python entry with the fixed adapter."""
    run_id, _ = admission(value)
    installed(value)
    source = ROOT / "security_vm_receive_route.py"
    pin, data = checked_file(source, RECORD)
    require(pin["sha256"] == value.get("routeSha256") and data)
    original, _ = checked_file(RECEIVER, 4096, 0o644)
    require(original["sha256"] == value.get("originalRouteSha256"))
    current = RECEIVER.lstat()
    require(current.st_dev == original["device"] and current.st_ino == original["inode"])
    route_config = {
        "schemaVersion": 1,
        "runId": run_id,
        "binarySha256": value["binarySha256"],
        "binaryBytes": value["binaryBytes"],
    }
    _ = write_new(route.CONFIG, encoded(route_config))
    value = dict(value, hostCurl=host_tools())
    _ = write_new(CONFIG, encoded(value))
    saved = ROOT / "original-fetch-release.py"
    require(not saved.exists() and not saved.is_symlink())
    os.link(RECEIVER, saved, follow_symlinks=False)
    require(saved.stat().st_ino == current.st_ino and RECEIVER.stat().st_ino == current.st_ino)
    RECEIVER.unlink()
    _ = write_new(RECEIVER, data, 0o644)
    observed, _ = checked_file(RECEIVER, RECORD, 0o644)
    require(observed["sha256"] == pin["sha256"])
    return {
        "installedRoute": True,
        "binarySha256": value["binarySha256"],
        "routeSha256": pin["sha256"],
        "timeoutSeconds": 300,
        "finalizationSeconds": 30,
        "hostCurl": value["hostCurl"],
    }


def proof() -> dict[str, object]:
    """Rebind the same route and installed binary before every trial observation."""
    _, data = checked_file(CONFIG, RECORD)
    value = record(data)
    _ = admission(value)
    installed(value)
    pin, _ = checked_file(RECEIVER, RECORD, 0o644)
    require(pin["sha256"] == value.get("routeSha256"))
    require(host_tools() == value["hostCurl"])
    return value


def publication(value: dict[str, object]) -> dict[str, object]:
    """Observe original public bytes and identities independently from the candidate."""
    destination = PUBLIC / "releases" / str(value["revision"])
    require(destination.resolve() == destination)
    for folder in (PUBLIC, PUBLIC / "releases", destination, WORK, ROOT):
        metadata = folder.lstat()
        require(
            stat.S_ISDIR(metadata.st_mode)
            and metadata.st_uid == 0
            and stat.S_IMODE(metadata.st_mode) == DIRECTORY_MODE
        )
    result: dict[str, object] = {}
    for name, key, maximum in (
        ("image.tar", "imageSha256", positive.MAX_IMAGE),
        ("release.json", "manifestSha256", positive.MAX_MANIFEST),
    ):
        pin, _ = checked_file(destination / name, maximum)
        require(pin["sha256"] == value[key])
        result[name] = pin
    return result


def lock_free() -> None:
    """Independently acquire the actual fixed workload lock after the session."""
    descriptor = os.open(WORK / "workload.lock", os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    try:
        fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        fcntl.flock(descriptor, fcntl.LOCK_UN)
    finally:
        os.close(descriptor)


def preflight_negative(value: dict[str, object], phase: str) -> dict[str, object]:
    """Exercise the real signed receiver under one deliberate lock/journal refusal."""
    selected = proof()
    before = publication(selected)
    destination = PUBLIC / "releases" / str(selected["revision"])
    attempts = {path.name for path in destination.glob("download.*")}
    envelope = value.get("envelope")
    require(isinstance(envelope, dict))
    descriptor: int | None = None
    journal: os.stat_result | None = None
    try:
        if phase == "lock-negative":
            descriptor = os.open(WORK / "workload.lock", os.O_RDONLY | os.O_NOFOLLOW)
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        else:
            require(phase == "journal-negative")
            journal = write_new(
                WORK / "current.json", encoded({"schemaVersion": 1, "finalized": False})
            )
        status, output, error = bounded_process.run(
            ["/usr/bin/python3", "-I", "-B", str(RECEIVER)],
            env={"PATH": "/usr/bin:/bin", "HOME": "/root", "LC_ALL": "C"},
            input_data=encoded(envelope),
            limits=bounded_process.Limits(timeout=60, stdout=RECORD, stderr=RECORD),
        )
        require(status == 1 and not error and output.count(b"\n") == 1)
        observed = record(output)
        require(
            set(observed) == set(FIELDS)
            and observed["passed"] is False
            and observed["settled"] is False
            and observed["finalized"] is False
            and observed["phase"] == "preflight"
            and observed["downloadedBytes"] == 0
            and observed["failureClass"]
            == ("filesystem_error" if descriptor is not None else "unfinished_operation")
        )
    finally:
        if descriptor is not None:
            try:
                fcntl.flock(descriptor, fcntl.LOCK_UN)
            finally:
                os.close(descriptor)
        if journal is not None:
            remove_original(WORK / "current.json", journal)
    lock_free()
    require(
        publication(selected) == before
        and {path.name for path in destination.glob("download.*")} == attempts
    )
    return {
        "case": phase,
        "nativeExitStatus": status,
        "noReady": True,
        "noNewAttempt": True,
        "publicationUnchanged": True,
        "lockReleased": True,
    }


def conflict(*, restore: bool) -> dict[str, object]:
    """Own one explicit foreign publication and restore only its verified original."""
    value = proof()
    target = PUBLIC / "releases" / str(value["revision"]) / "image.tar"
    foreign = b"owned receive conflict fixture\n"
    if not restore:
        _ = publication(value)
        require(not BACKUP.exists() and not BACKUP.is_symlink())
        os.link(target, BACKUP, follow_symlinks=False)
        require(target.stat().st_ino == BACKUP.stat().st_ino)
        target.unlink()
        _ = write_new(target, foreign)
    else:
        pin, data = checked_file(target, RECORD)
        require(data == foreign and pin["sha256"] == hashlib.sha256(foreign).hexdigest())
        saved, _ = checked_file(BACKUP, positive.MAX_IMAGE)
        require(saved["sha256"] == value["imageSha256"])
        target.unlink()
        os.link(BACKUP, target, follow_symlinks=False)
        BACKUP.unlink()
        _ = publication(value)
    return {"conflictRestored": restore, "fixtureOnly": True}


def inspect() -> dict[str, object]:
    """Check all successful/negative attempts without weakening the old positive probe."""
    value = proof()
    published = publication(value)
    destination = PUBLIC / "releases" / str(value["revision"])
    classes: dict[str, int] = {
        "complete": 0,
        "retained_artifact_differs": 0,
        "zip_checksum_mismatch": 0,
    }
    for attempt in destination.glob("download.*"):
        metadata = attempt.lstat()
        require(
            stat.S_ISDIR(metadata.st_mode)
            and metadata.st_uid == 0
            and stat.S_IMODE(metadata.st_mode) == DIRECTORY_MODE
        )
        _, data = checked_file(attempt / "fetch-outcome.json", RECORD)
        observed = record(data)
        require(
            set(observed) == set(FIELDS)
            and observed["settled"] is True
            and observed["revision"] == value["revision"]
            and type(observed["artifactId"]) is int
            and observed["artifactId"] == value["artifactId"]
            and type(observed["artifactZipBytes"]) is int
            and observed["artifactZipBytes"] == value["zipBytes"]
            and type(observed["downloadedBytes"]) is int
            and observed["downloadedBytes"] == value["zipBytes"]
        )
        kind = "complete" if observed["passed"] is True else observed["failureClass"]
        require(isinstance(kind, str) and kind in classes)
        kind = str(kind)
        classes[kind] += 1
        require(
            type(observed["schemaVersion"]) is int
            and observed["schemaVersion"] == 1
            and observed["passed"] is (kind == "complete")
            and observed["finalized"] is (kind == "complete")
            and observed["status"] == ("complete" if kind == "complete" else "failed")
            and observed["phase"]
            == {
                "complete": "complete",
                "retained_artifact_differs": "publish",
                "zip_checksum_mismatch": "download",
            }[kind]
        )
        wrong = "0" * 64 if value["zipSha256"] != "0" * 64 else "1" * 64
        require(
            observed["zipSha256"]
            == (wrong if kind == "zip_checksum_mismatch" else value["zipSha256"])
        )
        archive, _ = checked_file(attempt / "artifact.zip", positive.MAX_IMAGE)
        require(archive["sha256"] == value["zipSha256"] and archive["bytes"] == value["zipBytes"])
        names = {path.name for path in attempt.iterdir()}
        if kind == "zip_checksum_mismatch":
            require(
                observed["phase"] == "download"
                and names == {"envelope.json", "artifact.zip", "fetch-outcome.json"}
            )
        else:
            require(names == set(MEMBERS) | {"envelope.json", "artifact.zip", "fetch-outcome.json"})
            raw_pins = value["memberPins"]
            require(isinstance(raw_pins, dict))
            pins = cast("dict[str, object]", raw_pins)
            require(set(pins) == set(MEMBERS))
            for name in MEMBERS:
                expected = cast("dict[str, object]", pins[name])
                member, _ = checked_file(
                    attempt / name, positive.MAX_IMAGE if name == "image.tar" else 64 * 1024**2
                )
                require(
                    member["sha256"] == expected["sha256"] and member["bytes"] == expected["bytes"]
                )
            require(
                observed["archiveSha256"] == value["imageSha256"]
                and observed["manifestSha256"] == value["manifestSha256"]
            )
    require(classes == {"complete": 2, "retained_artifact_differs": 1, "zip_checksum_mismatch": 1})
    lock_free()
    return {
        "schemaVersion": 1,
        "scope": "SignedMuslReceiveGuestProbe",
        "runId": value["runId"],
        "revision": value["revision"],
        "classes": classes,
        "published": published,
        "lockReleased": True,
        "passed": True,
    }


class Options(argparse.Namespace):
    """Only one fixed fixture phase is selectable."""

    phase: str = ""


def main() -> int:
    """Permit only fixed fixture operations and a bounded private stdin object."""
    parser = argparse.ArgumentParser(allow_abbrev=False)
    _ = parser.add_argument(
        "phase",
        choices=(
            "install-route",
            "lock-negative",
            "journal-negative",
            "arm-conflict",
            "restore-conflict",
            "inspect",
        ),
    )
    parsed = parser.parse_args(namespace=Options())
    try:
        data = sys.stdin.buffer.read(RECORD + 1)
        require(len(data) <= RECORD)
        value = record(data) if data else {}
        if parsed.phase == "install-route":
            result = install_route(value)
        elif parsed.phase in ("lock-negative", "journal-negative"):
            result = preflight_negative(value, parsed.phase)
        elif parsed.phase in ("arm-conflict", "restore-conflict"):
            result = conflict(restore=parsed.phase == "restore-conflict")
        else:
            result = inspect()
        _ = sys.stdout.buffer.write(encoded(result))
        _ = sys.stdout.buffer.flush()
    except (OSError, ValueError, TypeError, KeyError, OverflowError):
        _ = sys.stderr.write("Signed receiver guest observation is unproven.\n")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
