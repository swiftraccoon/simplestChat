"""Offline public-service template checks; no Docker daemon or network access."""

from pathlib import Path
import unittest

from jinja2 import Environment, StrictUndefined
import yaml

ROOT = Path(__file__).resolve().parents[1]
VALUES = {
    "scpub_config": "/etc/simplestchat-public",
    "scpub_root": "/srv/simplestchat-public",
    "scpub_server_image": "sha256:" + "a" * 64,
    "scpub_postgres_image": "postgres:18.6-bookworm@sha256:" + "b" * 64,
    "scpub_caddy_image": "caddy:2.11.2-alpine@sha256:" + "c" * 64,
    "scpub_domain": "chat.example.test",
    "scpub_announce_ip": "192.0.2.10",
    "scpub_registration_enabled": False,
    "scpub_secrets": {
        name: f"{index:064x}"
        for index, name in enumerate([
            "postgres_admin_password", "migration_password", "database_password",
            "jwt_secret", "metrics_token", "proxy_secret", "owner_password",
        ], start=1)
    },
}


def render(name, **overrides):
    values = dict(VALUES, **overrides)
    return Environment(undefined=StrictUndefined).from_string(
        (ROOT / "templates" / name).read_text()
    ).render(values)


def environment(name, **overrides):
    return dict(
        line.split("=", 1) for line in render(name, **overrides).splitlines()
        if line and not line.startswith("#")
    )


class PublicTemplateTests(unittest.TestCase):
    def test_all_public_templates_render_without_missing_values(self):
        for path in (ROOT / "templates").glob("public-*.j2"):
            # Other public templates may use deployment-controller variables.
            if path.name in {
                "public-compose.yml.j2", "public-app.env.j2", "public-migration.env.j2",
                "public-proxy.env.j2", "public-postgres-admin-password.j2",
                "public-pg_hba.conf.j2", "public-init-database.sql.j2",
                "public-runtime-grants.sql.j2",
            }:
                with self.subTest(template=path.name):
                    self.assertNotIn("{{", render(path.name))

    def test_database_and_maintenance_have_no_network_or_ports(self):
        services = yaml.safe_load(render("public-compose.yml.j2"))["services"]
        for name in ["postgres", "migrate"]:
            with self.subTest(service=name):
                self.assertEqual(services[name]["network_mode"], "none")
                self.assertNotIn("ports", services[name])
                self.assertTrue(services[name]["read_only"])
                self.assertEqual(services[name]["cap_drop"], ["ALL"])
        database = services["postgres"]
        self.assertEqual(database["user"], "999:999")
        self.assertIn("listen_addresses=", database["command"])
        self.assertIn("password_encryption=scram-sha-256", database["command"])
        self.assertIn("unix_socket_directories=/var/run/postgresql,/run/simplestchat-postgres", database["command"])
        self.assertIn("/proc/1/comm", database["healthcheck"]["test"][1])
        self.assertIn("/srv/simplestchat-public/postgres:/var/lib/postgresql", database["volumes"])
        maintenance = services["migrate"]
        self.assertEqual(maintenance["profiles"], ["maintenance"])
        self.assertEqual(maintenance["restart"], "no")
        self.assertEqual(maintenance["image"], VALUES["scpub_server_image"])

    def test_app_extends_existing_compose_and_proxy_is_nonroot(self):
        services = yaml.safe_load(render("public-compose.yml.j2"))["services"]
        app = services["simplestchat"]
        self.assertEqual(app["extends"], {"file": "./compose.base.yml", "service": "simplestchat"})
        self.assertEqual(app["env_file"], ["./app.env"])
        self.assertEqual(app["user"], "10001:10001")
        self.assertEqual(app["image"], VALUES["scpub_server_image"])
        self.assertEqual(app["volumes"], [
            "/srv/simplestchat-public/postgres-socket:/run/simplestchat-postgres:ro",
        ])
        proxy = services["caddy"]
        self.assertEqual(proxy["user"], "10001:10001")
        self.assertEqual(proxy["cap_add"], ["NET_BIND_SERVICE"])
        self.assertEqual(proxy["ports"], ["80:80/tcp", "443:443/tcp", "443:443/udp"])
        self.assertEqual(proxy["sysctls"]["net.ipv4.ip_unprivileged_port_start"], "0")
        for service in services.values():
            self.assertEqual(service["logging"]["driver"], "local")
            self.assertEqual(service["logging"]["options"], {"max-size": "10m", "max-file": "3"})

    def test_app_configuration_is_bounded_and_secrets_are_separated(self):
        app = environment("public-app.env.j2")
        migration = environment("public-migration.env.j2")
        proxy = environment("public-proxy.env.j2")
        expected = {
            "RUN_MIGRATIONS": "false", "REGISTRATION_ENABLED": "false",
            "ALLOW_AD_HOC_ROOMS": "false", "MEDIA_WORKERS": "2", "RTC_PORT_END": "40001",
            "SIMPLESTCHAT_CPUS": "2.0", "SIMPLESTCHAT_MEMORY_LIMIT": "2g",
            "MAX_CONNECTIONS": "200", "MAX_USERS": "100", "MAX_ROOMS": "32",
            "MAX_PERSISTED_ROOMS": "100", "WEBAUTHN_RP_ID": "chat.example.test",
            "WEBAUTHN_ORIGIN": "https://chat.example.test",
        }
        for name, value in expected.items():
            self.assertEqual(app[name], value)
        self.assertEqual(environment("public-app.env.j2", scpub_registration_enabled=True)["REGISTRATION_ENABLED"], "true")
        self.assertIn("simplestchat_app:", app["DATABASE_URL"])
        self.assertIn("simplestchat_migrate:", migration["DATABASE_URL"])
        for values in [app, migration]:
            self.assertTrue(values["DATABASE_URL"].endswith("?host=/run/simplestchat-postgres&sslmode=disable"))
        self.assertEqual(migration["RUN_MIGRATIONS"], "true")
        self.assertEqual(migration["BIND_ADDR"], "127.0.0.1")
        self.assertEqual(migration["REGISTRATION_ENABLED"], "false")
        self.assertEqual(proxy["TRUSTED_PROXY_SECRET"], app["TRUSTED_PROXY_SECRET"])
        self.assertEqual(proxy["CADDY_UPSTREAM"], "simplestchat:3000")
        self.assertNotIn("JWT_SECRET", migration)
        self.assertNotIn("DATABASE_URL", proxy)
        self.assertNotIn(VALUES["scpub_secrets"]["owner_password"], "\n".join(app.values()))

    def test_database_authentication_and_grants_are_explicit(self):
        hba = [line.split() for line in render("public-pg_hba.conf.j2").splitlines()
               if line and not line.startswith("#")]
        self.assertEqual(hba, [
            ["local", "all", "postgres", "peer"],
            ["local", "simplestchat", "simplestchat_migrate", "scram-sha-256"],
            ["local", "simplestchat", "simplestchat_app", "scram-sha-256"],
            ["local", "all", "all", "reject"],
            ["host", "all", "all", "0.0.0.0/0", "reject"],
            ["host", "all", "all", "::/0", "reject"],
        ])
        bootstrap = render("public-init-database.sql.j2")
        self.assertEqual(bootstrap.count("NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"), 2)
        self.assertIn("REVOKE ALL ON DATABASE simplestchat FROM PUBLIC", bootstrap)
        self.assertIn("REVOKE ALL ON SCHEMA public FROM PUBLIC", bootstrap)
        self.assertIn("GRANT USAGE, CREATE ON SCHEMA public TO simplestchat_migrate", bootstrap)
        self.assertNotIn("GRANT USAGE, CREATE ON SCHEMA public TO simplestchat_app", bootstrap)
        self.assertIn("CREATE EXTENSION pg_trgm WITH SCHEMA public", bootstrap)
        grants = render("public-runtime-grants.sql.j2")
        self.assertNotIn("ALL TABLES", grants)
        self.assertIn("ON public.users, public.webauthn_credentials", grants)
        for table in ["sessions", "rooms", "room_roles", "room_states", "room_reports"]:
            self.assertIn(f"public.{table}", grants)
        statements = "\n".join(line for line in grants.splitlines() if not line.startswith("--"))
        self.assertNotIn("_sqlx_migrations", statements)


if __name__ == "__main__":
    unittest.main()
