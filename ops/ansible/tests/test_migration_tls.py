"""Bounded same-hostname TLS handoff regressions without contacting either host."""

import base64
import json
import os
import shutil
import ssl
import subprocess
import tempfile
import unittest
from pathlib import Path
from typing import TYPE_CHECKING, override
from unittest.mock import patch

from test_public_release import FixtureRunner

# isort: split
import migration_tls as tls
import release_public as release
import turn_public as turn

if TYPE_CHECKING:
    from release_json import JsonObject

ROOT = Path(__file__).resolve().parents[3]
DOMAIN = "chat.example.test"
ISSUER = "acme-v02.api.letsencrypt.org-directory"
OPERATION = "a" * 32


class MigrationTlsTests(unittest.TestCase):
    """Exercise real certificate validation and exclusive protected subtree writes."""

    root: Path = Path()
    source: Path = Path()
    target: Path = Path()
    transaction: Path = Path()
    host: Path = Path()
    certificate: Path = Path()
    key: Path = Path()
    openssl: str = ""

    @property
    def files(self) -> dict[str, bytes]:
        """Read the exact current fixture subtree."""
        return {suffix: (self.host / f"{DOMAIN}.{suffix}").read_bytes() for suffix in tls.SUFFIXES}

    @override
    def setUp(self) -> None:
        """Keep all certificate material and disposable storage inside this checkout."""
        executable = shutil.which("openssl")
        if executable is None:
            self.fail("OpenSSL is required for the certificate regression")
        self.openssl = executable
        results = ROOT / "results"
        results.mkdir(exist_ok=True)
        temporary = tempfile.TemporaryDirectory(prefix="migration-tls-test.", dir=results)
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.source = self.root / "source"
        self.source.mkdir(mode=0o700)
        self.target = self.root / "target"
        self.target.mkdir(mode=0o700)
        self.transaction = self.root / "transaction"
        self.transaction.mkdir(mode=0o700)
        self.host = self.source / ISSUER / DOMAIN
        self.host.mkdir(parents=True, mode=0o700)
        self.certificate = self.host / f"{DOMAIN}.crt"
        self.key = self.host / f"{DOMAIN}.key"
        self.generate(self.certificate, self.key, domain=DOMAIN, days=30)
        _ = (self.host / f"{DOMAIN}.json").write_text(
            json.dumps({"sans": [DOMAIN], "issuer_data": {}})
        )
        for target, attribute, value in (
            (turn, "SOURCE_OWNERS", frozenset((0, os.getuid()))),
            (turn, "CERTIFICATES", self.source),
            (release, "ROOT_UID", os.getuid()),
            (release, "ROOT", self.target),
            (tls, "CADDY_ID", os.getuid()),
        ):
            selected = patch.object(target, attribute, value)
            _ = selected.start()
            self.addCleanup(selected.stop)
        for name in ("chown", "fchown"):
            selected = patch.object(os, name)
            _ = selected.start()
            self.addCleanup(selected.stop)

    def generate(self, certificate: Path, key: Path, *, domain: str, days: int) -> None:
        """Generate a disposable trusted fixture with an exact DNS subject alternative name."""
        _ = subprocess.run(  # noqa: S603 - fixed OpenSSL executable and owned fixture paths.
            [
                self.openssl,
                "req",
                "-x509",
                "-newkey",
                "ec",
                "-pkeyopt",
                "ec_paramgen_curve:P-256",
                "-nodes",
                "-days",
                str(days),
                "-subj",
                f"/CN={domain}",
                "-addext",
                f"subjectAltName=DNS:{domain}",
                "-keyout",
                str(key),
                "-out",
                str(certificate),
            ],
            check=True,
            capture_output=True,
            timeout=15,
        )
        key.chmod(0o600)

    def validate(self, domain: str, certificate: Path, key: Path) -> None:
        """Run the actual trust/expiry/hostname commands with only the fixture CA substituted."""
        runner = FixtureRunner(self.root, self.root)

        def command(arguments: list[str]) -> bytes:
            args = [self.openssl, *arguments[1:]]
            if "-CAfile" in args:
                args[args.index("-CAfile") + 1] = str(self.certificate)
            return subprocess.run(  # noqa: S603 - arguments come from the fixed certificate validator.
                args,
                check=True,
                capture_output=True,
                timeout=15,
            ).stdout

        with patch.object(runner, "run", side_effect=command):
            turn.validate_certificate(runner, domain, certificate, key)

    def test_real_certificate_requires_hostname_key_and_remaining_validity(self) -> None:
        """Trusted bytes pass; wrong SNI, a mismatched key and imminent expiry fail."""
        self.validate(DOMAIN, self.certificate, self.key)
        with self.assertRaises(subprocess.CalledProcessError):
            self.validate("other.example.test", self.certificate, self.key)
        other_certificate, other_key = self.root / "other.crt", self.root / "other.key"
        self.generate(other_certificate, other_key, domain=DOMAIN, days=1)
        with self.assertRaises(ssl.SSLError):
            self.validate(DOMAIN, self.certificate, other_key)
        with self.assertRaises(subprocess.CalledProcessError):
            self.validate(DOMAIN, other_certificate, other_key)

    def test_roundtrip_preserves_only_selected_subtree_and_refuses_replacement(self) -> None:
        """Receiver is repeatable for identical bytes and never overwrites differing storage."""
        unrelated = self.source / ISSUER / "other.example.test"
        unrelated.mkdir()
        _ = (unrelated / "private.key").write_text("unrelated")
        with patch.object(tls, "validate") as validated:
            tls.export_bundle(self.transaction, DOMAIN)
            bundle = self.transaction / "tls.json"
            self.assertEqual(bundle.stat().st_mode & 0o777, 0o600)
            self.assertNotIn("unrelated", bundle.read_text())
            tls.import_bundle(self.transaction, DOMAIN)
            tls.import_bundle(self.transaction, DOMAIN)
            self.assertEqual(validated.call_count, 3)
            copied = self.target / "caddy-data/caddy/certificates" / ISSUER / DOMAIN
            for suffix, expected in self.files.items():
                output = copied / f"{DOMAIN}.{suffix}"
                self.assertEqual(output.read_bytes(), expected)
                self.assertEqual(output.stat().st_mode & 0o777, 0o600)
            preserved = copied / f"{DOMAIN}.crt"
            _ = preserved.write_bytes(b"existing different certificate")
            with self.assertRaises(release.ReleaseError):
                tls.import_bundle(self.transaction, DOMAIN)
            self.assertEqual(preserved.read_bytes(), b"existing different certificate")
            with self.assertRaises(FileExistsError):
                tls.export_bundle(self.transaction, DOMAIN)

    def test_bundle_rejects_wrong_host_path_escape_extra_file_and_oversize(self) -> None:
        """Envelope contents cannot choose arbitrary paths, hosts or additional files."""
        baseline: JsonObject = {
            "domain": DOMAIN,
            "issuer": ISSUER,
            "files": {
                suffix: base64.b64encode(data).decode() for suffix, data in self.files.items()
            },
        }
        for change in (
            {"domain": "other.example.test"},
            {"issuer": "../escape"},
            {"files": {"../key": "YQ=="}},
            {"unexpected": True},
        ):
            with self.subTest(change=change), self.assertRaises(release.ReleaseError):
                release.atomic(self.transaction / "tls.json", json.dumps(baseline | change))
                _ = tls.read_bundle(self.transaction, DOMAIN)
        release.atomic(self.transaction / "tls.json", b"x" * (tls.MAX_BUNDLE + 1))
        with self.assertRaises(release.ReleaseError):
            _ = tls.read_bundle(self.transaction, DOMAIN)

    def test_source_links_and_unsafe_key_permissions_are_refused(self) -> None:
        """No-follow reads reject linked keys and publicly readable private keys."""
        self.key.chmod(0o644)
        with self.assertRaises(release.ReleaseError):
            _ = tls.snapshot(DOMAIN)
        self.key.unlink()
        self.key.symlink_to(self.certificate)
        with self.assertRaises(OSError):
            _ = tls.snapshot(DOMAIN)

    def test_rejected_validation_never_creates_destination_storage(self) -> None:
        """Even a correctly structured envelope must pass fresh certificate verification."""
        with patch.object(tls, "validate"):
            tls.export_bundle(self.transaction, DOMAIN)
        with (
            patch.object(tls, "validate", side_effect=release.ReleaseError("invalid certificate")),
            self.assertRaises(release.ReleaseError),
        ):
            tls.import_bundle(self.transaction, DOMAIN)
        self.assertFalse((self.target / "caddy-data").exists())

    def test_existing_destination_link_or_extra_file_is_not_replaced(self) -> None:
        """A destination hostname must be an ordinary exact subtree, including on retries."""
        with patch.object(tls, "validate"):
            tls.export_bundle(self.transaction, DOMAIN)
            tls.import_bundle(self.transaction, DOMAIN)
            copied = self.target / "caddy-data/caddy/certificates" / ISSUER / DOMAIN
            extra = copied / "unexpected"
            _ = extra.write_text("preserve")
            with self.assertRaises(release.ReleaseError):
                tls.import_bundle(self.transaction, DOMAIN)
            self.assertEqual(extra.read_text(), "preserve")
            extra.unlink()
            key = copied / f"{DOMAIN}.key"
            key.unlink()
            key.symlink_to(self.key)
            with self.assertRaises(OSError):
                tls.import_bundle(self.transaction, DOMAIN)
            self.assertTrue(key.is_symlink())

    def test_operation_requires_private_matching_origin_identity(self) -> None:
        """The CLI cannot import certificates for a hostname outside the prepared request."""
        migrations = self.target / "migrations"
        migrations.mkdir(mode=0o700)
        operation = migrations / OPERATION
        operation.mkdir(mode=0o700)
        request: JsonObject = {
            "operationId": OPERATION,
            "sourceOrigin": f"https://{DOMAIN}",
            "destinationOrigin": f"https://{DOMAIN}",
            "targetRevision": "b" * 40,
        }
        release.atomic(operation / "request.json", request)
        self.assertEqual(tls.operation(OPERATION), (operation, DOMAIN))
        request["destinationOrigin"] = "https://other.example.test"
        release.atomic(operation / "request.json", request)
        with self.assertRaises(release.ReleaseError):
            _ = tls.operation(OPERATION)


if __name__ == "__main__":
    _ = unittest.main()
