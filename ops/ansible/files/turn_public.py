"""Maintain the public relay certificate and explicitly enable its app configuration.

The separate relay project never owns PostgreSQL or Caddy. Application changes
reuse the release lock, backup, readiness checks, journal and bounded rollback.
Certificate reloads use coturn SIGUSR2 and do not replace active allocations.
"""

import argparse
import hashlib
import os
import re
import signal
import socket
import ssl
import stat
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from types import FrameType
from typing import NoReturn

import release_public as release
from release_artifact import ArtifactError, validate_manifest
from release_json import JsonObject, decode_json, object_value, string_value

CONFIG = Path("/etc/simplestchat-turn")
CERTIFICATES = Path("/srv/simplestchat-public/caddy-data/caddy/certificates")
RELAY_GID = 10002
MAX_CERTIFICATE = 65536
CONTAINER_IDENTITY_FIELDS = 2


def settings() -> JsonObject:
    """Read the bounded, protected deployment identity without returning secrets."""
    release.protected(CONFIG, directory=True, modes=(0o700,))
    release.protected(CONFIG / "settings.json", limit=4096)
    result = object_value(decode_json((CONFIG / "settings.json").read_bytes()))
    domain = string_value(result["domain"])
    # Reuse the exact URL boundary used by the application configuration.
    _ = release.TurnConfiguration(domain=domain, secret="0" * 64).environment()
    release.require(
        re.fullmatch(
            r"docker.io/coturn/coturn:4.18.0@sha256:[a-f0-9]{64}", string_value(result["image"])
        ),
        "Unexpected relay image selection",
    )
    release.protected(CONFIG / "turnserver.conf", modes=(0o440,), limit=16384)
    release.require(
        hashlib.sha256((CONFIG / "turnserver.conf").read_bytes()).hexdigest()
        == result.get("configurationSha256"),
        "Relay configuration differs from its prepared identity",
    )
    release.protected(CONFIG / "secret", limit=64)
    secret = (CONFIG / "secret").read_text()
    _ = release.TurnConfiguration(domain=domain, secret=secret).environment()
    release.require(
        [
            line
            for line in (CONFIG / "turnserver.conf").read_text().splitlines()
            if line.startswith("static-auth-secret=")
        ]
        == [f"static-auth-secret={secret}"],
        "Prepared relay and application secrets differ",
    )
    return result


def certificate_source(domain: str) -> tuple[Path, Path]:
    """Select Caddy's most recently written certificate for this exact hostname."""
    choices = sorted(
        CERTIFICATES.glob(f"*/{domain}/{domain}.crt"),
        key=lambda path: path.stat().st_mtime_ns,
        reverse=True,
    )
    release.require(choices, "Caddy has not issued the relay hostname certificate")
    certificate = choices[0]
    key = certificate.with_suffix(".key")
    release.require(
        stat.S_IMODE(key.lstat().st_mode) in (0o400, 0o600), "Unsafe source key permissions"
    )
    for path in (certificate, key):
        metadata = path.lstat()
        release.require(
            stat.S_ISREG(metadata.st_mode)
            and metadata.st_uid in (0, 10001)
            and not metadata.st_mode & 0o022
            and 0 < metadata.st_size <= MAX_CERTIFICATE,
            "Unsafe certificate source",
        )
        for parent in path.parents:
            if parent == CERTIFICATES.parent:
                break
            release.require(not parent.is_symlink(), "Symlink in certificate source")
    return certificate, key


def publish_certificate(runner: release.RunnerProtocol, domain: str) -> bool:
    """Validate the matching trusted pair, then atomically select a private copy."""
    certificate, key = certificate_source(domain)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(certificate, key)
    _ = runner.run(
        ["/usr/bin/openssl", "x509", "-in", str(certificate), "-checkend", "86400", "-noout"]
    )
    _ = runner.run(
        [
            "/usr/bin/openssl",
            "verify",
            "-purpose",
            "sslserver",
            "-verify_hostname",
            domain,
            "-CAfile",
            "/etc/ssl/certs/ca-certificates.crt",
            "-untrusted",
            str(certificate),
            str(certificate),
        ]
    )
    certificate_data = certificate.read_bytes()
    key_data = key.read_bytes()
    generation = hashlib.sha256(certificate_data).hexdigest()
    tls = CONFIG / "tls"
    release.protected(tls, directory=True, modes=(0o750,))
    destination = tls / generation
    if destination.exists():
        release.protected(destination, directory=True, modes=(0o750,))
        for name, data in (("certificate.pem", certificate_data), ("key.pem", key_data)):
            release.protected(destination / name, modes=(0o440,), limit=MAX_CERTIFICATE)
            release.require((destination / name).read_bytes() == data, "Certificate copy differs")
    else:
        destination.mkdir(mode=0o750)
        os.chown(destination, 0, RELAY_GID)
        destination.chmod(0o750)
        for name, data in (("certificate.pem", certificate_data), ("key.pem", key_data)):
            release.atomic(destination / name, data)
            os.chown(destination / name, 0, RELAY_GID)
            (destination / name).chmod(0o440)
    current = tls / "current"
    if current.is_symlink() and current.readlink() == Path(generation):
        return False
    release.require(not current.exists() or current.is_symlink(), "Unexpected TLS selection")
    temporary = tls / ".next"
    release.require(
        not temporary.exists() and not temporary.is_symlink(), "Unfinished TLS selection"
    )
    temporary.symlink_to(generation)
    _ = temporary.replace(current)
    return True


def relay_container(runner: release.RunnerProtocol) -> str:
    """Select exactly the running container in the separate relay project."""
    identity = (
        runner.docker(
            "ps",
            "--quiet",
            "--no-trunc",
            "--filter",
            "label=com.docker.compose.project=simplestchat-turn",
            "--filter",
            "label=com.docker.compose.service=turn",
        )
        .decode()
        .strip()
    )
    release.require(re.fullmatch(r"[a-f0-9]{64}", identity), "Expected one running managed relay")
    selected = settings()
    image = (
        runner.docker("image", "inspect", "--format", "{{.Id}}", string_value(selected["image"]))
        .decode()
        .strip()
    )
    actual = (
        runner.docker(
            "inspect",
            "--format",
            '{{.Image}} {{index .Config.Labels "com.docker.compose.config-hash"}}',
            identity,
        )
        .decode()
        .split()
    )
    expected = (
        runner.docker(
            "compose",
            "--project-name",
            "simplestchat-turn",
            "--project-directory",
            str(CONFIG),
            "-f",
            str(CONFIG / "compose.yml"),
            "config",
            "--hash",
            "turn",
        )
        .decode()
        .split()
    )
    release.require(
        len(expected) == CONTAINER_IDENTITY_FIELDS
        and expected[0] == "turn"
        and actual == [image, expected[1]],
        "Running relay differs from its reviewed configuration",
    )
    return identity


def verify_tls(domain: str, *, seconds: float = 10) -> None:
    """Require trusted TLS and the exact currently published leaf certificate."""
    certificate = (CONFIG / "tls/current/certificate.pem").read_text()
    leaf = certificate.partition("-----END CERTIFICATE-----")[0] + "-----END CERTIFICATE-----\n"
    expected = ssl.PEM_cert_to_DER_cert(leaf)
    deadline = time.monotonic() + seconds
    while True:
        try:
            with (
                socket.create_connection((domain, 5349), timeout=3) as connection,
                ssl.create_default_context().wrap_socket(connection, server_hostname=domain) as tls,
            ):
                release.require(
                    tls.getpeercert(binary_form=True) == expected, "Relay certificate differs"
                )
        except (OSError, release.ReleaseError):
            if time.monotonic() >= deadline:
                message = "Relay TLS verification failed"
                raise release.ReleaseError(message) from None
            time.sleep(0.2)
        else:
            return


def refresh_certificate(runner: release.RunnerProtocol, domain: str) -> bool:
    """Reload the validated certificate without interrupting relayed calls."""
    identity = relay_container(runner)
    changed = publish_certificate(runner, domain)
    # Signal even when the copy already matches: a previous reload may have
    # failed after publishing the pair. Repeating SIGUSR2 is nondestructive.
    _ = runner.docker("kill", "--signal", "SIGUSR2", identity)
    verify_tls(domain)
    return changed


def activate(runner: release.RunnerProtocol, domain: str, report: JsonObject) -> None:
    """Enable the already verified relay through the bounded app replacement."""
    release.protected(CONFIG / "secret", limit=64)
    turn = release.TurnConfiguration(domain=domain, secret=(CONFIG / "secret").read_text())
    desired = turn.environment()
    current = dict(
        line.split("=", 1)
        for line in (release.CONFIG / "app.env").read_text().splitlines()
        if line and not line.startswith("#") and "=" in line
    )
    if all(current.get(key) == value for key, value in desired.items()):
        app = runner.container("simplestchat")
        release.require(
            runner.compose("config", "--hash", "simplestchat").decode().split()
            == ["simplestchat", app["configHash"]],
            "Running configuration differs from disk",
        )
        report["alreadyConfigured"] = True
        release.ready(runner, origin=f"https://{domain}", seconds=3)
        return
    selected = object_value(decode_json((release.CONFIG / "images.json").read_bytes()))
    revision = string_value(selected["revision"])
    release.require(re.fullmatch(r"[a-f0-9]{40}", revision), "Unexpected application revision")
    manifest_path = release.ROOT / "releases" / revision / "release.json"
    release.protected(manifest_path, limit=65536)
    manifest = validate_manifest(manifest_path)
    release.require(manifest["revision"] == revision, "Deployed release identity differs")
    report["revision"] = revision
    release.deploy(runner, manifest, selected, report, quiet_seconds=600, turn=turn)


class _Arguments(argparse.Namespace):
    action: str = ""


def main() -> None:
    """Perform one explicitly selected relay operation with private evidence."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument(
        "action", choices=("prepare-certificate", "refresh-certificate", "activate")
    )
    arguments = parser.parse_args(namespace=_Arguments())
    release.require(os.geteuid() == 0, "Run as root on the prepared public host")
    _ = os.umask(0o077)

    def interrupted(_signum: int, _frame: FrameType | None) -> NoReturn:
        message = "Relay operation interrupted"
        raise release.ReleaseError(message)

    for signum in (signal.SIGTERM, signal.SIGINT):
        _ = signal.signal(signum, interrupted)
    with release.workload_lock():
        domain = string_value(settings()["domain"])
        release.protected(release.ROOT / "results", directory=True, modes=(0o700,))
        attempt = Path(tempfile.mkdtemp(prefix="turn.", dir=release.ROOT / "results"))
        runner = release.Runner(attempt)
        report: JsonObject = {
            "action": arguments.action,
            "startedAt": release.timestamp(),
            "passed": False,
        }
        try:
            if arguments.action == "prepare-certificate":
                report["certificateChanged"] = publish_certificate(runner, domain)
            else:
                report["certificateChanged"] = refresh_certificate(runner, domain)
                if arguments.action == "activate":
                    activate(runner, domain, report)
            report["passed"] = True
        finally:
            report["finishedAt"] = release.timestamp()
            release.atomic(attempt / "outcome.json", report)
            _ = sys.stdout.write(f"Relay evidence: {attempt}\n")


def cli() -> int:
    """Keep native errors and private configuration out of controller output."""
    try:
        main()
    except (
        release.ReleaseError,
        ArtifactError,
        OSError,
        ValueError,
        KeyError,
        subprocess.TimeoutExpired,
    ) as error:
        _ = sys.stderr.write(
            f"Relay operation failed ({type(error).__name__}); inspect private evidence.\n"
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(cli())
