"""Exercise relay certificate publication and configuration without touching a VPS."""

import hashlib
import json
import os
import shlex
import shutil
import ssl
import subprocess
import tempfile
import unittest
from pathlib import Path
from typing import TYPE_CHECKING, Unpack, cast
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

    def test_descriptor_snapshot_has_hard_size_and_regular_file_bounds(self) -> None:
        """Opened source validation rejects empty, oversized and growing file contents."""
        with (
            tempfile.TemporaryDirectory() as directory,
            patch.object(turn, "SOURCE_OWNERS", {os.getuid()}),
        ):
            root = Path(directory)
            key = root / "key.pem"
            descriptor = os.open(root, os.O_RDONLY | os.O_DIRECTORY)
            self.addCleanup(os.close, descriptor)
            _ = key.write_bytes(b"fixture")
            key.chmod(0o600)
            data, _ = turn.read_certificate_file(descriptor, "key.pem", private_key=True)
            self.assertEqual(data, b"fixture")
            for content in (b"", b"x" * (turn.MAX_CERTIFICATE + 1)):
                _ = key.write_bytes(content)
                with self.assertRaises(release.ReleaseError):
                    _ = turn.read_certificate_file(descriptor, "key.pem", private_key=True)
            _ = key.write_bytes(b"fixture")
            with (
                patch.object(os, "read", return_value=b"x" * (turn.MAX_CERTIFICATE + 1)),
                self.assertRaises(release.ReleaseError),
            ):
                _ = turn.read_certificate_file(descriptor, "key.pem", private_key=True)

    def test_metrics_support_probe_handles_pinned_help_stderr_and_exit_status(self) -> None:
        """The exact probe accepts stderr help with 255, but rejects failed or incomplete help."""
        identity = "a" * 64
        cases = (
            (0, True, True),
            (255, True, True),
            (255, False, False),
            (0, False, False),
            (1, True, False),
            (127, True, False),
        )
        for status, has_options, accepted in cases:
            with (
                self.subTest(status=status, has_options=has_options),
                tempfile.TemporaryDirectory() as directory,
            ):
                root = Path(directory)
                executable = root / "fixture turnserver"
                help_text = (
                    " ".join(option.decode() for option in turn.METRICS_HELP_OPTIONS)
                    if has_options
                    else "fixture version help"
                )
                _ = executable.write_text(
                    f"#!/bin/sh\nprintf '%s\\n' '{help_text}' >&2\nexit {status}\n"
                )
                executable.chmod(0o700)
                runner = release.Runner(root)

                def docker(
                    *arguments: str,
                    fixture_executable: Path = executable,
                    fixture_runner: release.Runner = runner,
                    **options: Unpack[release.CommandOptions],
                ) -> bytes:
                    self.assertEqual(
                        arguments, ("exec", identity, "/bin/sh", "-c", turn.METRICS_HELP_PROBE)
                    )
                    # Only replace the pinned executable with this owned fixture.
                    # Stream redirection and status handling execute unchanged.
                    command = arguments[-1].replace(
                        "/usr/bin/turnserver", shlex.quote(str(fixture_executable))
                    )
                    return fixture_runner.run(["/bin/sh", "-c", command], **options)

                with patch.object(runner, "docker", side_effect=docker):
                    if accepted:
                        turn.require_metrics_support(runner, identity)
                    else:
                        with self.assertRaises(release.ReleaseError):
                            turn.require_metrics_support(runner, identity)
                self.assertEqual((root / "001.stderr").read_bytes(), b"")
                self.assertEqual((root / "001.stdout").read_text(), help_text + "\n")

    def test_metrics_transition_preserves_configuration_and_rolls_back(self) -> None:
        """Only fixed metrics settings change; failed readiness restores previous bytes."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config = root / "config"
            config.mkdir()
            source = config / "turnserver.conf"
            original = b"static-auth-secret=fixture-secret\nlistening-port=3478\n"
            _ = source.write_bytes(original)
            metadata = config / "settings.json"
            before = json.dumps(
                {"configurationSha256": hashlib.sha256(original).hexdigest()}
            ).encode()
            _ = metadata.write_bytes(before)
            runner = FixtureRunner(root, config)
            with (
                patch.object(turn, "CONFIG", config),
                patch.object(turn, "relay_container", return_value="a" * 64),
                patch.object(runner, "docker", return_value=b" ".join(turn.METRICS_HELP_OPTIONS)),
                patch.object(turn, "verify_tls"),
                patch.object(turn, "verify_metrics"),
                patch.object(os, "chown"),
            ):
                self.assertTrue(turn.enable_metrics(runner, "relay.example.test"))
                self.assertEqual(source.read_bytes(), original + turn.METRICS_OPTIONS.encode())
                self.assertFalse(turn.enable_metrics(runner, "relay.example.test"))
                release.atomic(source, original)
                _ = metadata.write_bytes(before)
                with (
                    patch.object(
                        turn, "verify_metrics", side_effect=release.ReleaseError("fixture")
                    ),
                    self.assertRaises(release.ReleaseError),
                ):
                    _ = turn.enable_metrics(runner, "relay.example.test")
                self.assertEqual(source.read_bytes(), original)
                self.assertEqual(metadata.read_bytes(), before)

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

            def source_bytes(_domain: str) -> tuple[bytes, bytes]:
                return certificate.read_bytes(), key.read_bytes()

            with (
                patch.object(turn, "CONFIG", config),
                patch.object(
                    turn,
                    "certificate_source",
                    side_effect=source_bytes,
                ),
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
                # Verification consumes root-owned snapshots, never mutable source paths.
                verify_call = cast("list[str]", commands.call_args_list[1].args[0])
                self.assertNotEqual(verify_call[-1], str(certificate))
                self.assertTrue(str(verify_call[-1]).startswith(str(config / ".certificate-")))
                self.assertEqual(verify_call[-1], verify_call[-2])
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
