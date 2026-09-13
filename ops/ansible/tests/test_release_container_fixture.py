"""Offline release fixture rendering; optional Compose checks need no daemon."""

from copy import deepcopy
import importlib.util
import json
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

from jinja2 import UndefinedError
import yaml


PROJECT = Path(__file__).resolve().parents[3]
SPEC = importlib.util.spec_from_file_location("release_container_fixture", PROJECT / "build/release_container_fixture.py")
FIXTURE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FIXTURE)
RELEASE_SPEC = importlib.util.spec_from_file_location(
    "release_container_fixture_runtime", PROJECT / "ops/ansible/files/release-public.py",
)
RELEASE = importlib.util.module_from_spec(RELEASE_SPEC)
with patch.object(sys, "path", [str(PROJECT / "ops/ansible/files"), *sys.path]):
    RELEASE_SPEC.loader.exec_module(RELEASE)
IMAGE = "sha256:" + "a" * 64
REVISION = "b" * 40
TOKEN = "c" * 32
POSTGRES = "docker.io/library/postgres:18.6-bookworm@sha256:" + "d" * 64
CADDY = "docker.io/library/caddy:2.11.4-alpine@sha256:" + "e" * 64
FILES = {
    "app.env", "migration.env", "proxy.env", "compose.base.yml", "compose.public.yml",
    "Caddyfile", "pg_hba.conf", "init-database.sql", "runtime-grants.sql",
    "postgres-admin-password", "images.json",
}


class ReleaseContainerFixtureTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="simplestchat-release-render.")
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name).resolve()
        self.config = self.directory / "config"
        self.root = self.directory / "data"
        self.config.mkdir(mode=0o700)
        self.root.mkdir(mode=0o700)

    def render(self, **overrides):
        arguments = {
            "config": self.config, "root": self.root, "image": IMAGE,
            "revision": REVISION, "token": TOKEN,
            "postgres_image": POSTGRES, "caddy_image": CADDY,
        }
        FIXTURE.render_fixture(**dict(arguments, **overrides))

    def environment(self, filename):
        return dict(line.split("=", 1) for line in (self.config / filename).read_text().splitlines()
                    if line and not line.startswith("#"))

    def test_exact_private_outputs_and_immutable_image_identity(self):
        self.render()
        self.assertEqual({path.name for path in self.config.iterdir()}, FILES)
        self.assertEqual(list(self.root.iterdir()), [])
        for path in self.config.iterdir():
            with self.subTest(filename=path.name):
                self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
                self.assertTrue(path.read_text().endswith("\n"))
                self.assertNotIn("{{", path.read_text())
        self.assertEqual(json.loads((self.config / "images.json").read_text()), {
            "revision": REVISION, "serverImage": IMAGE, "postgresImage": POSTGRES, "caddyImage": CADDY,
        })
        compose = (self.config / "compose.public.yml").read_text()
        self.assertEqual(compose.count(f'image: "{IMAGE}"'), 2)

    def test_only_loopback_ports_are_published_and_all_resources_are_owned(self):
        self.render()
        base = yaml.safe_load((self.config / "compose.base.yml").read_text())
        public = yaml.safe_load((self.config / "compose.public.yml").read_text())
        app = base["services"]["simplestchat"]
        self.assertEqual(app["ports"], ["127.0.0.1:3000:3000", "127.0.0.1:40000:40000/udp"])
        self.assertEqual(public["services"]["caddy"]["ports"], ["127.0.0.1:443:443/tcp"])
        self.assertNotIn("ports", public["services"]["simplestchat"])
        self.assertEqual(public["services"]["simplestchat"]["extends"], {
            "file": "./compose.base.yml", "service": "simplestchat",
        })
        for services in (base["services"], public["services"]):
            for name, service in services.items():
                with self.subTest(service=name):
                    self.assertEqual(service["labels"][FIXTURE.LABEL], TOKEN)
        self.assertEqual(public["networks"]["default"]["labels"][FIXTURE.LABEL], TOKEN)
        for name in ("postgres", "migrate"):
            self.assertEqual(public["services"][name]["network_mode"], "none")
            self.assertNotIn("ports", public["services"][name])

    def test_runtime_privilege_and_database_role_restrictions_are_preserved(self):
        self.render()
        public = yaml.safe_load((self.config / "compose.public.yml").read_text())
        base = yaml.safe_load((self.config / "compose.base.yml").read_text())
        self.assertEqual(public["services"]["postgres"]["user"], "999:999")
        for name in ("simplestchat", "migrate", "caddy"):
            self.assertEqual(public["services"][name]["user"], "10001:10001")
        for service in [base["services"]["simplestchat"], *[
            public["services"][name] for name in ("postgres", "migrate", "caddy")
        ]]:
            self.assertIs(service["read_only"], True)
            self.assertEqual(service["cap_drop"], ["ALL"])
            self.assertEqual(service["security_opt"], ["no-new-privileges:true"])
        self.assertEqual(public["services"]["migrate"]["profiles"], ["maintenance"])
        self.assertEqual(public["services"]["migrate"]["restart"], "no")
        for filename in ("pg_hba.conf", "runtime-grants.sql"):
            self.assertEqual((self.config / filename).read_text(),
                             (FIXTURE.TEMPLATES / f"public-{filename}.j2").read_text())
        bootstrap = (self.config / "init-database.sql").read_text()
        self.assertEqual(bootstrap.count("NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"), 2)

    def test_local_origin_minimal_media_and_distinct_ephemeral_secrets(self):
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
        selected_secrets = [app[name] for name in ("JWT_SECRET", "METRICS_TOKEN", "TRUSTED_PROXY_SECRET")]
        selected_secrets.append((self.config / "postgres-admin-password").read_text().strip())
        self.assertEqual(len(set(selected_secrets)), len(selected_secrets))
        for secret in selected_secrets:
            self.assertRegex(secret, r"^[a-f0-9]{64}$")

    def test_tls_is_explicit_without_external_resolution_or_admin_listener(self):
        self.render()
        caddy = (self.config / "Caddyfile").read_text()
        self.assertIn("\tadmin off\n", caddy)
        self.assertIn("\tauto_https off\n", caddy)
        self.assertIn("https://localhost {", caddy)
        self.assertIn("tls /etc/caddy/fixture.crt /etc/caddy/fixture.key", caddy)
        self.assertIn("reverse_proxy simplestchat:3000", caddy)
        self.assertIn("header_up X-SimplestChat-Proxy {$TRUSTED_PROXY_SECRET}", caddy)
        self.assertIn('@private path /metrics /metrics/* /diagnostics /diagnostics/*', caddy)
        self.assertIn('respond @private "Not Found" 404', caddy)
        self.assertNotIn("tls_insecure_skip_verify", caddy)
        self.assertNotIn("simplestchat.example.com", caddy)
        compose = yaml.safe_load((self.config / "compose.public.yml").read_text())
        for filename in ("fixture.crt", "fixture.key"):
            self.assertIn(f"{self.config}/{filename}:/etc/caddy/{filename}:ro",
                          compose["services"]["caddy"]["volumes"])
            self.assertFalse((self.config / filename).exists())

    def test_existing_file_or_symlink_refuses_every_output_before_writing(self):
        existing = self.config / "images.json"
        existing.write_text("user-owned evidence")
        with self.assertRaises(FileExistsError):
            self.render()
        self.assertEqual(existing.read_text(), "user-owned evidence")
        self.assertEqual(list(self.config.iterdir()), [existing])
        existing.unlink()
        existing.symlink_to(self.directory / "missing")
        with self.assertRaises(FileExistsError):
            self.render()
        self.assertTrue(existing.is_symlink())
        self.assertEqual(list(self.config.iterdir()), [existing])

    def test_invalid_identity_and_symlinked_directory_are_refused(self):
        for name, value in (
            ("image", "simplestchat:latest"), ("revision", "main"), ("token", "unknown"),
            ("postgres_image", "postgres:latest"), ("caddy_image", "caddy:latest"),
        ):
            with self.subTest(name=name), self.assertRaises(ValueError):
                self.render(**{name: value})
        link = self.directory / "linked-config"
        link.symlink_to(self.config, target_is_directory=True)
        with self.assertRaises(ValueError):
            self.render(config=link)
        self.assertEqual(list(self.config.iterdir()), [])

    def test_strict_undefined_stops_before_any_output(self):
        original = Path.read_text

        def altered(path, *args, **kwargs):
            if path == FIXTURE.TEMPLATES / "public-app.env.j2":
                return "{{ fixture_missing_required_value }}\n"
            return original(path, *args, **kwargs)

        with patch.object(Path, "read_text", altered), self.assertRaises(UndefinedError):
            self.render()
        self.assertEqual(list(self.config.iterdir()), [])

    @unittest.skipUnless(shutil.which("docker"), "Optional offline Compose renderer requires the Docker CLI")
    def test_real_compose_accepts_release_preview_and_preserves_fixture_isolation(self):
        docker = shutil.which("docker")
        version = subprocess.run([docker, "compose", "version"], env=RELEASE.ENV,
                                 capture_output=True, text=True, timeout=10, check=False)
        if version.returncode:
            self.skipTest("Docker Compose plugin is unavailable; no daemon is required")
        self.render()
        attempt = self.directory / "attempt"
        attempt.mkdir(mode=0o700)
        runner = RELEASE.Runner(attempt)
        new_image = "sha256:" + "f" * 64
        original = {filename: (self.config / filename).read_bytes() for filename in RELEASE.SELECTION}
        with patch.object(RELEASE, "CONFIG", self.config), patch.object(RELEASE, "DOCKER", [docker]):
            before = json.loads(runner.compose("--profile", "maintenance", "config", "--format", "json"))
            self.assertEqual(set(before["services"]), {"simplestchat", "migrate", "postgres", "caddy"})
            ports = []
            for name, service in before["services"].items():
                with self.subTest(service=name):
                    self.assertEqual(service["labels"][FIXTURE.LABEL], TOKEN)
                    for port in service.get("ports", []):
                        self.assertEqual(port["host_ip"], "127.0.0.1")
                        ports.append((name, str(port["published"]), port["target"], port["protocol"]))
            self.assertEqual(set(ports), {
                ("simplestchat", "3000", 3000, "tcp"),
                ("simplestchat", "40000", 40000, "udp"),
                ("caddy", "443", 443, "tcp"),
            })
            self.assertEqual(len(ports), 3)
            self.assertEqual(before["networks"]["default"]["labels"][FIXTURE.LABEL], TOKEN)
            hashes = {name: runner.compose("config", "--hash", name)
                      for name in ("simplestchat", "postgres", "caddy")}
            preview, environment = RELEASE.candidate_selection(runner, new_image, {"serverImage": IMAGE})
            self.assertEqual({filename: (self.config / filename).read_bytes() for filename in RELEASE.SELECTION}, original)
            rendered_preview = json.loads(runner.compose(
                "--profile", "maintenance", "config", "--format", "json",
                filename=attempt / "candidate-compose.yml", envfile=attempt / "candidate.env",
            ))
            expected = deepcopy(before)
            for name in ("simplestchat", "migrate"):
                expected["services"][name]["image"] = new_image
            self.assertEqual(rendered_preview, expected)
            RELEASE.atomic(self.config / "compose.public.yml", preview)
            RELEASE.atomic(self.config / "app.env", environment)
            after = json.loads(runner.compose("--profile", "maintenance", "config", "--format", "json"))
            expected["services"]["simplestchat"]["environment"]["SIMPLESTCHAT_IMAGE"] = new_image
            self.assertEqual(after, expected)
            for name in ("postgres", "caddy"):
                self.assertEqual(runner.compose("config", "--hash", name), hashes[name])
            self.assertNotEqual(runner.compose("config", "--hash", "simplestchat"), hashes["simplestchat"])


if __name__ == "__main__":
    unittest.main()
