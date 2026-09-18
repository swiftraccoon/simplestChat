"""Offline release fixture rendering; optional Compose checks need no daemon."""

from __future__ import annotations

import shutil
import stat
import subprocess
import tempfile
import unittest
from copy import deepcopy
from dataclasses import replace
from pathlib import Path
from unittest.mock import patch

from jinja2 import UndefinedError
from test_support import ROOT as PROJECT

# isort: split

import release_container_fixture as fixture
import release_public as release
from release_json import (
    JsonObject,
    JsonValue,
    array_value,
    decode_json,
    integer_value,
    object_value,
    string_value,
)

IMAGE = "sha256:" + "a" * 64
REVISION = "b" * 40
TOKEN = "c" * 32
POSTGRES = "docker.io/library/postgres:18.6-bookworm@sha256:" + "d" * 64
CADDY = "docker.io/library/caddy:2.11.4-alpine@sha256:" + "e" * 64
FILES = {
    "app.env",
    "migration.env",
    "proxy.env",
    "compose.base.yml",
    "compose.public.yml",
    "Caddyfile",
    "pg_hba.conf",
    "init-database.sql",
    "runtime-grants.sql",
    "postgres-admin-password",
    "images.json",
}
IDENTITY = fixture.FixtureIdentity(IMAGE, REVISION, TOKEN, POSTGRES, CADDY)


def field(value: JsonValue, *keys: str) -> JsonValue:
    """Traverse a test configuration only after validating each object boundary."""
    for key in keys:
        value = object_value(value)[key]
    return value


def mapping(value: JsonValue, *keys: str) -> JsonObject:
    """Select a mutable configuration object with a checked recursive path."""
    return object_value(field(value, *keys))


class ReleaseContainerFixtureTests(unittest.TestCase):
    """Verify that production restrictions survive private fixture rendering."""

    def __init__(self, method_name: str = "runTest") -> None:
        """Create each case's exclusively owned temporary fixture paths."""
        super().__init__(method_name)
        self.temporary: tempfile.TemporaryDirectory[str] = tempfile.TemporaryDirectory(
            prefix="simplestchat-release-render."
        )
        self.addCleanup(self.temporary.cleanup)
        self.directory: Path = Path(self.temporary.name).resolve()
        self.config: Path = self.directory / "config"
        self.root: Path = self.directory / "data"
        self.config.mkdir(mode=0o700)
        self.root.mkdir(mode=0o700)

    def render(
        self, *, config: Path | None = None, identity: fixture.FixtureIdentity = IDENTITY
    ) -> None:
        """Render only this case's explicit identity into its owned directories."""
        fixture.render_fixture(self.config if config is None else config, self.root, identity)

    def environment(self, filename: str) -> dict[str, str]:
        """Parse literal fixture environment assignments without executing them."""
        return dict(
            line.split("=", 1)
            for line in (self.config / filename).read_text(encoding="utf-8").splitlines()
            if line and not line.startswith("#")
        )

    def test_exact_private_outputs_and_immutable_image_identity(self) -> None:
        """Every fixture file is private and both app selections remain quoted immutable IDs."""
        self.assertEqual(
            Path(fixture.__file__).resolve(), PROJECT / "build/release_container_fixture.py"
        )
        self.render()
        self.assertEqual({path.name for path in self.config.iterdir()}, FILES)
        self.assertEqual(list(self.root.iterdir()), [])
        for path in self.config.iterdir():
            with self.subTest(filename=path.name):
                self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
                self.assertTrue(path.read_text(encoding="utf-8").endswith("\n"))
                self.assertNotIn("{{", path.read_text(encoding="utf-8"))
        self.assertEqual(
            decode_json((self.config / "images.json").read_text(encoding="utf-8")),
            {
                "revision": REVISION,
                "serverImage": IMAGE,
                "postgresImage": POSTGRES,
                "caddyImage": CADDY,
            },
        )
        compose = (self.config / "compose.public.yml").read_text(encoding="utf-8")
        self.assertEqual(compose.count(f'image: "{IMAGE}"'), 2)

    def test_only_loopback_ports_are_published_and_all_resources_are_owned(self) -> None:
        """Only app/proxy loopback ports are exposed and every resource carries its owner label."""
        self.render()
        base = fixture.load_yaml((self.config / "compose.base.yml").read_text(encoding="utf-8"))
        public = fixture.load_yaml((self.config / "compose.public.yml").read_text(encoding="utf-8"))
        app = mapping(base, "services", "simplestchat")
        self.assertEqual(app["ports"], ["127.0.0.1:3000:3000", "127.0.0.1:40000:40000/udp"])
        self.assertEqual(field(public, "services", "caddy", "ports"), ["127.0.0.1:443:443/tcp"])
        self.assertNotIn("ports", mapping(public, "services", "simplestchat"))
        self.assertEqual(
            field(public, "services", "simplestchat", "extends"),
            {
                "file": "./compose.base.yml",
                "service": "simplestchat",
            },
        )
        for services in (mapping(base, "services"), mapping(public, "services")):
            for name, service in services.items():
                with self.subTest(service=name):
                    self.assertEqual(field(service, "labels", fixture.LABEL), TOKEN)
        self.assertEqual(field(public, "networks", "default", "labels", fixture.LABEL), TOKEN)
        for name in ("postgres", "migrate"):
            self.assertEqual(field(public, "services", name, "network_mode"), "none")
            self.assertNotIn("ports", mapping(public, "services", name))

    def test_runtime_privilege_and_database_role_restrictions_are_preserved(self) -> None:
        """Runtime users, capability limits, migration behavior, and DB grants remain unchanged."""
        self.render()
        public = fixture.load_yaml((self.config / "compose.public.yml").read_text(encoding="utf-8"))
        base = fixture.load_yaml((self.config / "compose.base.yml").read_text(encoding="utf-8"))
        self.assertEqual(field(public, "services", "postgres", "user"), "999:999")
        for name in ("simplestchat", "migrate", "caddy"):
            self.assertEqual(field(public, "services", name, "user"), "10001:10001")
        for service in [
            mapping(base, "services", "simplestchat"),
            *[mapping(public, "services", name) for name in ("postgres", "migrate", "caddy")],
        ]:
            self.assertTrue(service["read_only"])
            self.assertEqual(service["cap_drop"], ["ALL"])
            self.assertEqual(service["security_opt"], ["no-new-privileges:true"])
        self.assertEqual(field(public, "services", "migrate", "profiles"), ["maintenance"])
        self.assertEqual(field(public, "services", "migrate", "restart"), "no")
        for filename in ("pg_hba.conf", "runtime-grants.sql"):
            self.assertEqual(
                (self.config / filename).read_text(encoding="utf-8"),
                (fixture.TEMPLATES / f"public-{filename}.j2").read_text(encoding="utf-8"),
            )
        bootstrap = (self.config / "init-database.sql").read_text(encoding="utf-8")
        self.assertEqual(
            bootstrap.count("NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"), 2
        )

    def test_local_origin_minimal_media_and_distinct_ephemeral_secrets(self) -> None:
        """Fixture origin/media bounds stay local and each independent secret is ephemeral."""
        self.render()
        app = self.environment("app.env")
        migration = self.environment("migration.env")
        proxy = self.environment("proxy.env")
        for name in ("ALLOWED_ORIGINS", "WEBAUTHN_ORIGIN"):
            self.assertEqual(app[name], "https://localhost")
        self.assertEqual(app["WEBAUTHN_RP_ID"], "localhost")
        self.assertEqual(app["ANNOUNCE_IP"], "127.0.0.1")
        self.assertEqual(app["RUN_MIGRATIONS"], "false")
        self.assertEqual(app["REGISTRATION_ENABLED"], "false")
        self.assertEqual(app["MEDIA_WORKERS"], "1")
        self.assertEqual(app["RTC_PORT_END"], "40000")
        self.assertEqual(migration["RUN_MIGRATIONS"], "true")
        self.assertEqual(proxy["CADDY_DOMAIN"], "localhost")
        self.assertEqual(proxy["CADDY_UPSTREAM"], "simplestchat:3000")
        self.assertEqual(proxy["TRUSTED_PROXY_SECRET"], app["TRUSTED_PROXY_SECRET"])
        self.assertIn("postgres://simplestchat_app:", app["DATABASE_URL"])
        self.assertIn("postgres://simplestchat_migrate:", migration["DATABASE_URL"])
        self.assertNotEqual(app["DATABASE_URL"], migration["DATABASE_URL"])
        selected_secrets = [
            app[name] for name in ("JWT_SECRET", "METRICS_TOKEN", "TRUSTED_PROXY_SECRET")
        ]
        selected_secrets.append(
            (self.config / "postgres-admin-password").read_text(encoding="utf-8").strip()
        )
        self.assertEqual(len(set(selected_secrets)), len(selected_secrets))
        for secret in selected_secrets:
            self.assertRegex(secret, r"^[a-f0-9]{64}$")

    def test_tls_is_explicit_without_external_resolution_or_admin_listener(self) -> None:
        """The local certificate disables ACME/admin exposure without weakening HTTPS checks."""
        self.render()
        caddy = (self.config / "Caddyfile").read_text(encoding="utf-8")
        self.assertIn("\tadmin off\n", caddy)
        self.assertIn("\tauto_https off\n", caddy)
        self.assertIn("https://localhost {", caddy)
        self.assertIn("tls /etc/caddy/fixture.crt /etc/caddy/fixture.key", caddy)
        self.assertIn("reverse_proxy simplestchat:3000", caddy)
        self.assertIn("header_up X-SimplestChat-Proxy {$TRUSTED_PROXY_SECRET}", caddy)
        self.assertIn("@private path /metrics /metrics/* /diagnostics /diagnostics/*", caddy)
        self.assertIn('respond @private "Not Found" 404', caddy)
        self.assertNotIn("tls_insecure_skip_verify", caddy)
        self.assertNotIn("simplestchat.example.com", caddy)
        compose = fixture.load_yaml(
            (self.config / "compose.public.yml").read_text(encoding="utf-8")
        )
        for filename in ("fixture.crt", "fixture.key"):
            self.assertIn(
                f"{self.config}/{filename}:/etc/caddy/{filename}:ro",
                array_value(field(compose, "services", "caddy", "volumes")),
            )
            self.assertFalse((self.config / filename).exists())

    def test_existing_file_or_symlink_refuses_every_output_before_writing(self) -> None:
        """An existing file or dangling symlink prevents any partial replacement."""
        existing = self.config / "images.json"
        _ = existing.write_text("user-owned evidence", encoding="utf-8")
        with self.assertRaises(FileExistsError):
            self.render()
        self.assertEqual(existing.read_text(encoding="utf-8"), "user-owned evidence")
        self.assertEqual(list(self.config.iterdir()), [existing])
        existing.unlink()
        existing.symlink_to(self.directory / "missing")
        with self.assertRaises(FileExistsError):
            self.render()
        self.assertTrue(existing.is_symlink())
        self.assertEqual(list(self.config.iterdir()), [existing])

    def test_invalid_identity_and_symlinked_directory_are_refused(self) -> None:
        """Reject moving image selectors, invalid ownership tokens, and symlinked outputs."""
        for name, value in (
            ("image", "simplestchat:latest"),
            ("revision", "main"),
            ("token", "unknown"),
            ("postgres_image", "postgres:latest"),
            ("caddy_image", "caddy:latest"),
        ):
            with self.subTest(name=name), self.assertRaises(ValueError):
                self.render(identity=replace(IDENTITY, **{name: value}))
        link = self.directory / "linked-config"
        link.symlink_to(self.config, target_is_directory=True)
        with self.assertRaises(ValueError):
            self.render(config=link)
        self.assertEqual(list(self.config.iterdir()), [])

    def test_strict_undefined_stops_before_any_output(self) -> None:
        """Missing template inputs stop before any output file is published."""
        original = Path.read_text

        def altered(path: Path, encoding: str | None = None, errors: str | None = None) -> str:
            if path == fixture.TEMPLATES / "public-app.env.j2":
                return "{{ fixture_missing_required_value }}\n"
            return original(path, encoding=encoding, errors=errors)

        with patch.object(Path, "read_text", altered), self.assertRaises(UndefinedError):
            self.render()
        self.assertEqual(list(self.config.iterdir()), [])

    def test_real_compose_accepts_release_preview_and_preserves_fixture_isolation(self) -> None:
        """The optional offline renderer accepts the fixture and preserves release isolation."""
        docker = shutil.which("docker")
        if docker is None:
            self.skipTest("Optional offline Compose renderer requires the Docker CLI")
        # Only the resolved local Docker executable's offline version command is invoked.
        version = subprocess.run(  # noqa: S603
            [docker, "compose", "version"],
            env=release.ENV,
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )
        if version.returncode:
            self.skipTest("Docker Compose plugin is unavailable; no daemon is required")
        self.render()
        attempt = self.directory / "attempt"
        attempt.mkdir(mode=0o700)
        runner = release.Runner(attempt)
        new_image = "sha256:" + "f" * 64
        original = {
            filename: (self.config / filename).read_bytes() for filename in release.SELECTION
        }
        with (
            patch.object(release, "CONFIG", self.config),
            patch.object(release, "DOCKER", [docker]),
        ):
            before = decode_json(
                runner.compose("--profile", "maintenance", "config", "--format", "json")
            )
            self.assertEqual(
                set(mapping(before, "services")), {"simplestchat", "migrate", "postgres", "caddy"}
            )
            ports: list[tuple[str, str, int, str]] = []
            for name, raw_service in mapping(before, "services").items():
                service = object_value(raw_service)
                with self.subTest(service=name):
                    self.assertEqual(field(service, "labels", fixture.LABEL), TOKEN)
                    for raw_port in array_value(service.get("ports", [])):
                        port = object_value(raw_port)
                        self.assertEqual(port["host_ip"], "127.0.0.1")
                        ports.append(
                            (
                                name,
                                str(port["published"]),
                                integer_value(port["target"]),
                                string_value(port["protocol"]),
                            )
                        )
            self.assertEqual(
                set(ports),
                {
                    ("simplestchat", "3000", 3000, "tcp"),
                    ("simplestchat", "40000", 40000, "udp"),
                    ("caddy", "443", 443, "tcp"),
                },
            )
            self.assertEqual(len(ports), 3)
            self.assertEqual(field(before, "networks", "default", "labels", fixture.LABEL), TOKEN)
            hashes = {
                name: runner.compose("config", "--hash", name)
                for name in ("simplestchat", "postgres", "caddy")
            }
            preview, environment = release.candidate_selection(
                runner, new_image, {"serverImage": IMAGE}
            )
            self.assertEqual(
                {filename: (self.config / filename).read_bytes() for filename in release.SELECTION},
                original,
            )
            rendered_preview = decode_json(
                runner.compose(
                    "--profile",
                    "maintenance",
                    "config",
                    "--format",
                    "json",
                    filename=attempt / "candidate-compose.yml",
                    envfile=attempt / "candidate.env",
                )
            )
            expected = deepcopy(before)
            for name in ("simplestchat", "migrate"):
                mapping(expected, "services", name)["image"] = new_image
            self.assertEqual(rendered_preview, expected)
            release.atomic(self.config / "compose.public.yml", preview)
            release.atomic(self.config / "app.env", environment)
            after = decode_json(
                runner.compose("--profile", "maintenance", "config", "--format", "json")
            )
            mapping(expected, "services", "simplestchat", "environment")["SIMPLESTCHAT_IMAGE"] = (
                new_image
            )
            self.assertEqual(after, expected)
            for name in ("postgres", "caddy"):
                self.assertEqual(runner.compose("config", "--hash", name), hashes[name])
            self.assertNotEqual(
                runner.compose("config", "--hash", "simplestchat"), hashes["simplestchat"]
            )


if __name__ == "__main__":
    _ = unittest.main()
