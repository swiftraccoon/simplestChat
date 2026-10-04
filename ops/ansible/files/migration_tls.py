"""Transfer one validated Caddy hostname through a private migration transaction.

Only the selected issuer/hostname certificate, key and certificate metadata are
included. ACME account keys, other hostnames and arbitrary storage paths are not
transferred. The receiver never replaces existing certificate bytes.
"""

import argparse
import base64
import json
import os
import re
import ssl
import subprocess
import sys
import tempfile
from pathlib import Path

import release_public as release
import turn_public as turn
from release_json import JsonObject, decode_json, object_value, string_value

CADDY_ID = 10001
SUFFIXES = ("crt", "key", "json")
MAX_BUNDLE = 4 * turn.MAX_CERTIFICATE + 4096


def issuer_name(value: str) -> str:
    """Accept one ordinary issuer storage component, never a path."""
    release.require(
        re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,199}", value) and value not in (".", ".."),
        "Invalid certificate issuer directory",
    )
    return value


def operation(operation_id: str) -> tuple[Path, str]:
    """Bind the only allowed hostname to the protected unchanged-origin request."""
    release.require(re.fullmatch(r"[a-f0-9]{32}", operation_id), "Invalid migration identity")
    directory = release.ROOT / "migrations" / operation_id
    for path in (release.ROOT, directory.parent, directory):
        release.protected(path, directory=True, modes=(0o700,))
    request = directory / "request.json"
    release.protected(request, limit=4096)
    value = object_value(decode_json(request.read_bytes()))
    source = string_value(value["sourceOrigin"])
    release.require(
        value["operationId"] == operation_id
        and source == value["destinationOrigin"]
        and source.startswith("https://"),
        "Certificate transfer requires the exact unchanged migration origin",
    )
    domain = source.removeprefix("https://")
    _ = release.TurnConfiguration(domain=domain, secret="0" * 64).environment()
    return directory, domain


def snapshot(domain: str) -> tuple[str, dict[str, bytes]]:
    """Read the newest exact-hostname Caddy subtree using bounded no-follow reads."""
    root = turn.certificate_directory(turn.CERTIFICATES)
    newest: tuple[int, str, dict[str, bytes]] | None = None
    try:
        with os.scandir(root) as entries:
            for index, entry in enumerate(entries):
                release.require(index < turn.MAX_CERTIFICATE_AUTHORITIES, "Too many issuers")
                issuer = issuer_name(entry.name)
                try:
                    host = turn.certificate_directory(turn.CERTIFICATES / issuer / domain)
                except FileNotFoundError:
                    continue
                try:
                    certificate, modified = turn.read_certificate_file(
                        host, f"{domain}.crt", private_key=False
                    )
                    files = {"crt": certificate}
                    for suffix in ("key", "json"):
                        files[suffix], _ = turn.read_certificate_file(
                            host, f"{domain}.{suffix}", private_key=suffix == "key"
                        )
                finally:
                    os.close(host)
                if newest is None or modified > newest[0]:
                    newest = (modified, issuer, files)
    finally:
        os.close(root)
    if newest is None:
        message = "No certificate for the unchanged migration hostname"
        raise release.ReleaseError(message)
    return newest[1], newest[2]


def validate(directory: Path, domain: str, files: dict[str, bytes]) -> None:
    """Validate private immutable snapshots before exporting or writing Caddy storage."""
    release.require(set(files) == set(SUFFIXES), "Unexpected certificate files")
    release.require(
        all(0 < len(data) <= turn.MAX_CERTIFICATE for data in files.values()),
        "Invalid certificate file size",
    )
    _ = object_value(decode_json(files["json"]))
    with tempfile.TemporaryDirectory(prefix=".tls-validation-", dir=directory) as temporary:
        private = Path(temporary)
        certificate, key = private / "certificate.pem", private / "key.pem"
        release.atomic(certificate, files["crt"])
        release.atomic(key, files["key"])
        turn.validate_certificate(release.Runner(private), domain, certificate, key)


def exclusive_file(directory: int, name: str, data: bytes, *, caddy: bool) -> None:
    """Create one durable private file without following or replacing any existing entry."""
    descriptor = os.open(
        name,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
        0o600,
        dir_fd=directory,
    )
    with os.fdopen(descriptor, "wb") as output:
        if caddy:
            os.fchown(output.fileno(), CADDY_ID, CADDY_ID)
        _ = output.write(data)
        output.flush()
        os.fsync(output.fileno())
    os.fsync(directory)


def export_bundle(directory: Path, domain: str) -> None:
    """Export just the validated selected hostname; the bundle is never printed."""
    issuer, files = snapshot(domain)
    validate(directory, domain, files)
    bundle: JsonObject = {
        "domain": domain,
        "issuer": issuer,
        "files": {name: base64.b64encode(data).decode("ascii") for name, data in files.items()},
    }
    descriptor = turn.certificate_directory(directory)
    try:
        exclusive_file(descriptor, "tls.json", json.dumps(bundle).encode(), caddy=False)
    finally:
        os.close(descriptor)


def read_bundle(directory: Path, domain: str) -> tuple[str, dict[str, bytes]]:
    """Read the bounded private envelope with exact fields and no path-bearing filenames."""
    path = directory / "tls.json"
    release.protected(path, limit=MAX_BUNDLE)
    with path.open("rb") as source:
        encoded = source.read(MAX_BUNDLE + 1)
    release.require(len(encoded) <= MAX_BUNDLE, "Oversized certificate bundle")
    bundle = object_value(decode_json(encoded))
    release.require(
        set(bundle) == {"domain", "issuer", "files"} and bundle["domain"] == domain,
        "Certificate bundle identity differs",
    )
    issuer = issuer_name(string_value(bundle["issuer"]))
    records = object_value(bundle["files"])
    release.require(set(records) == set(SUFFIXES), "Unexpected certificate files")
    files = {
        name: base64.b64decode(string_value(value), validate=True)
        for name, value in records.items()
    }
    validate(directory, domain, files)
    return issuer, files


def destination_issuer(issuer: str) -> int:
    """Create only the fixed Caddy hierarchy with checked descriptor-relative directories."""
    parent = turn.certificate_directory(release.ROOT)
    try:
        for name in ("caddy-data", "caddy", "certificates", issuer):
            try:
                os.mkdir(name, mode=0o700, dir_fd=parent)
            except FileExistsError:
                pass
            else:
                os.chown(name, CADDY_ID, CADDY_ID, dir_fd=parent, follow_symlinks=False)
            child = os.open(
                name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=parent
            )
            metadata = os.fstat(child)
            safe = metadata.st_uid in turn.SOURCE_OWNERS and not metadata.st_mode & 0o022
            if not safe:
                os.close(child)
            release.require(safe, "Unsafe destination certificate directory")
            os.close(parent)
            parent = child
    except BaseException:
        os.close(parent)
        raise
    return parent


def import_bundle(directory: Path, domain: str) -> None:
    """Install only new bytes, or accept an already identical complete hostname subtree."""
    issuer, files = read_bundle(directory, domain)
    parent = destination_issuer(issuer)
    try:
        try:
            os.mkdir(domain, mode=0o700, dir_fd=parent)
        except FileExistsError:
            created = False
        else:
            created = True
            os.chown(domain, CADDY_ID, CADDY_ID, dir_fd=parent, follow_symlinks=False)
        host = os.open(
            domain, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=parent
        )
        try:
            metadata = os.fstat(host)
            release.require(
                metadata.st_uid == CADDY_ID and not metadata.st_mode & 0o022,
                "Unsafe destination hostname directory",
            )
            if created:
                for suffix, data in files.items():
                    exclusive_file(host, f"{domain}.{suffix}", data, caddy=True)
            else:
                names: set[str] = set()
                with os.scandir(host) as entries:
                    for index, entry in enumerate(entries):
                        release.require(index < len(SUFFIXES), "Unexpected hostname files")
                        names.add(entry.name)
                release.require(
                    names == {f"{domain}.{suffix}" for suffix in SUFFIXES},
                    "Unexpected existing hostname files",
                )
                for suffix, expected in files.items():
                    actual, _ = turn.read_certificate_file(
                        host, f"{domain}.{suffix}", private_key=suffix == "key"
                    )
                    release.require(actual == expected, "Existing destination certificate differs")
        finally:
            os.close(host)
        os.fsync(parent)
    finally:
        os.close(parent)


class Arguments(argparse.Namespace):
    """Validated command selection; private material is always file based."""

    action: str = ""
    operation_id: str = ""


def main() -> None:
    """Run one bounded same-origin TLS handoff under the canonical workload lock."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("action", choices=("export", "import"))
    _ = parser.add_argument("operation_id")
    arguments = parser.parse_args(namespace=Arguments())
    release.require(os.geteuid() == 0, "Run as root on the selected migration host")
    _ = os.umask(0o077)
    with release.workload_lock():
        directory, domain = operation(arguments.operation_id)
        if arguments.action == "export":
            export_bundle(directory, domain)
        else:
            import_bundle(directory, domain)


if __name__ == "__main__":
    try:
        main()
    except (
        release.ReleaseError,
        OSError,
        ValueError,
        KeyError,
        ssl.SSLError,
        subprocess.TimeoutExpired,
    ):
        _ = sys.stderr.write(
            "Migration TLS handoff failed; private certificate state was preserved.\n"
        )
        raise SystemExit(1) from None
