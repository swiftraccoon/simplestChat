"""Exercise relay certificate publication and configuration without touching a VPS."""

import hashlib
import json
import os
import shutil
import ssl
import subprocess
import tempfile
import unittest
from pathlib import Path
from typing import TYPE_CHECKING
from unittest.mock import patch

from test_public_release import FixtureRunner
from test_public_templates import environment, render
from test_support import array, at, obj, yaml_value

# isort: split
import release_public as release
import turn_public as turn

if TYPE_CHECKING:
    from release_json import JsonObject


class TurnTemplateTests(unittest.TestCase):
    """Verify the relay's destination, credential and filesystem boundaries."""

    def test_relay_is_separate_and_has_no_privileged_network_or_filesystem_access(self) -> None:
        """Only the pinned coturn executable and its private inputs are installed."""
        project = obj(yaml_value(render("turn-compose.yml.j2", scpub_turn_image="pinned-fixture")))
        self.assertEqual(project["name"], "simplestchat-turn")
        services = obj(project, "services")
        self.assertEqual(set(services), {"turn"})
        service = obj(services, "turn")
        self.assertEqual(service["user"], "10002:10002")
        self.assertEqual(service["entrypoint"], ["/usr/bin/turnserver"])
        self.assertIs(service["read_only"], expr2=True)
        self.assertEqual(service["cap_drop"], ["ALL"])
        self.assertEqual(service["cap_add"], ["NET_BIND_SERVICE"])
        self.assertEqual(service["security_opt"], ["no-new-privileges:true"])
        self.assertTrue(all(str(value).endswith(":ro") for value in array(service, "volumes")))
        self.assertEqual(at(service, "logging", "driver"), "local")

    def test_coturn_authenticates_and_only_relays_to_the_selected_media_host(self) -> None:
        """Deny all other peers, TCP peer relaying, unauthenticated STUN and unbounded use."""
        lines = render("turnserver.conf.j2", scpub_turn_secret="a" * 64).splitlines()
        for required in (
            "use-auth-secret",
            "no-stun",
            "no-tcp-relay",
            "no-multicast-peers",
            "denied-peer-ip=0.0.0.0-255.255.255.255",
            "denied-peer-ip=::-ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
            "allowed-peer-ip=192.0.2.10",
            "user-quota=4",
            "total-quota=400",
            "max-bps=2000000",
            "bps-capacity=100000000",
            "log-min-level=warning",
        ):
            self.assertIn(required, lines)
        for forbidden in (
            "no-auth",
            "allow-loopback-peers",
            "server-relay",
            "cli",
            "web-admin",
            "tlsv1",
            "tlsv1_1",
            "dtls",
        ):
            self.assertNotIn(forbidden, lines)

    def test_full_maintenance_preserves_the_same_managed_turn_values(self) -> None:
        """The full-maintenance template and app-only activation advertise identical credentials."""
        values = environment(
            "public-app.env.j2", scpub_turn_enabled=True, scpub_turn_secret="a" * 64
        )
        expected = release.TurnConfiguration(
            domain="chat.example.test", secret="a" * 64
        ).environment()
        self.assertEqual({key: values[key] for key in expected}, expected)
        self.assertNotIn("TURN_URLS", environment("public-app.env.j2"))

    def test_settings_reject_config_drift_and_a_mismatched_shared_secret(self) -> None:
        """Prepared metadata must bind the exact relay config and the app's shared secret."""
        with tempfile.TemporaryDirectory(prefix="simplestchat-turn-settings.") as directory:
            config = Path(directory)
            secret = "a" * 64
            _ = (config / "secret").write_text(secret)
            text = f"static-auth-secret={secret}\n"
            selected = {
                "domain": "relay.example.test",
                "image": "docker.io/coturn/coturn:4.18.0@sha256:" + "b" * 64,
                "configurationSha256": hashlib.sha256(text.encode()).hexdigest(),
            }
            _ = (config / "turnserver.conf").write_text(text)
            _ = (config / "settings.json").write_text(json.dumps(selected))
            with patch.object(turn, "CONFIG", config), patch.object(release, "protected"):
                self.assertEqual(turn.settings(), selected)
                _ = (config / "turnserver.conf").write_text(text + "server-relay\n")
                with self.assertRaisesRegex(release.ReleaseError, "differs from its prepared"):
                    _ = turn.settings()
                _ = (config / "turnserver.conf").write_text(text)
                _ = (config / "secret").write_text("c" * 64)
                with self.assertRaisesRegex(release.ReleaseError, "secrets differ"):
                    _ = turn.settings()

    def test_repeated_activation_is_read_only_and_rejects_runtime_drift(self) -> None:
        """Matching env files are insufficient unless the running app has that configuration."""
        with tempfile.TemporaryDirectory(prefix="simplestchat-turn-activation.") as directory:
            config = Path(directory)
            values = release.TurnConfiguration(
                domain="relay.example.test", secret="a" * 64
            ).environment()
            _ = (config / "secret").write_text("a" * 64)
            _ = (config / "app.env").write_text(
                "".join(f"{key}={value}\n" for key, value in values.items())
            )
            runner = FixtureRunner(config, config)
            with (
                patch.object(turn, "CONFIG", config),
                patch.object(release, "CONFIG", config),
                patch.object(release, "protected"),
            ):
                report: JsonObject = {}
                turn.activate(runner, "relay.example.test", report)
                self.assertTrue(report["alreadyConfigured"])
                self.assertEqual(runner.ups, 0)
                runner.config_hash_mismatch = "simplestchat"
                with self.assertRaisesRegex(release.ReleaseError, "Running configuration differs"):
                    turn.activate(runner, "relay.example.test", {})
                self.assertEqual(runner.ups, 0)


class TurnCertificateTests(unittest.TestCase):
    """Use real certificate/key parsing with only trust commands and root ownership stubbed."""

    def test_publication_selects_a_complete_private_pair_and_is_repeatable(self) -> None:
        """New pairs are readable by the relay group even under the root helper's 077 umask."""
        executable = shutil.which("openssl")
        self.assertIsNotNone(executable)
        if executable is None:
            self.fail("OpenSSL is required for the certificate regression")
        with tempfile.TemporaryDirectory(prefix="simplestchat-turn-cert.") as directory:
            root = Path(directory)
            certificate, key = root / "source.crt", root / "source.key"
            _ = subprocess.run(  # noqa: S603 - fixed openssl arguments and exclusively owned fixture paths.
                [
                    executable,
                    "req",
                    "-x509",
                    "-newkey",
                    "ec",
                    "-pkeyopt",
                    "ec_paramgen_curve:P-256",
                    "-nodes",
                    "-days",
                    "30",
                    "-subj",
                    "/CN=relay.example.test",
                    "-keyout",
                    str(key),
                    "-out",
                    str(certificate),
                ],
                check=True,
                capture_output=True,
                timeout=15,
            )
            config = root / "config"
            config.mkdir()
            (config / "tls").mkdir()
            runner = FixtureRunner(root, config)
            previous_umask = os.umask(0o077)
            self.addCleanup(os.umask, previous_umask)
            with (
                patch.object(turn, "CONFIG", config),
                patch.object(turn, "certificate_source", return_value=(certificate, key)),
                patch.object(release, "protected"),
                patch.object(os, "chown"),
                patch.object(runner, "run", return_value=b"OK") as commands,
            ):
                self.assertTrue(turn.publish_certificate(runner, "relay.example.test"))
                selected = config / "tls/current"
                self.assertTrue(selected.is_symlink())
                self.assertEqual(selected.stat().st_mode & 0o777, 0o750)
                self.assertEqual(
                    (selected / "certificate.pem").read_bytes(), certificate.read_bytes()
                )
                self.assertEqual((selected / "key.pem").read_bytes(), key.read_bytes())
                self.assertEqual((selected / "key.pem").stat().st_mode & 0o777, 0o440)
                self.assertFalse(turn.publish_certificate(runner, "relay.example.test"))
                commands.assert_any_call(
                    [
                        "/usr/bin/openssl",
                        "verify",
                        "-purpose",
                        "sslserver",
                        "-verify_hostname",
                        "relay.example.test",
                        "-CAfile",
                        "/etc/ssl/certs/ca-certificates.crt",
                        "-untrusted",
                        str(certificate),
                        str(certificate),
                    ]
                )
                selection = selected.readlink()
                with (
                    patch.object(
                        runner, "run", side_effect=release.ReleaseError("fixture trust failure")
                    ),
                    self.assertRaises(release.ReleaseError),
                ):
                    _ = turn.publish_certificate(runner, "relay.example.test")
                self.assertEqual(selected.readlink(), selection)
                _ = key.write_text("invalid fixture key")
                with self.assertRaises(ssl.SSLError):
                    _ = turn.publish_certificate(runner, "relay.example.test")
                self.assertEqual(selected.readlink(), selection)

    def test_refresh_retries_the_signal_even_when_the_pair_was_already_published(self) -> None:
        """A failed previous reload must not leave the old certificate indefinitely installed."""
        with tempfile.TemporaryDirectory(prefix="simplestchat-turn-refresh.") as directory:
            runner = FixtureRunner(Path(directory), Path(directory))
            with (
                patch.object(turn, "relay_container", return_value="b" * 64),
                patch.object(turn, "publish_certificate", return_value=False),
                patch.object(runner, "docker", return_value=b"") as docker,
                patch.object(turn, "verify_tls") as verify,
            ):
                self.assertFalse(turn.refresh_certificate(runner, "relay.example.test"))
                docker.assert_called_once_with("kill", "--signal", "SIGUSR2", "b" * 64)
                verify.assert_called_once_with("relay.example.test")


if __name__ == "__main__":
    _ = unittest.main()
