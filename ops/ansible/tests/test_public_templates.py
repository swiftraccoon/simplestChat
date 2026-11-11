"""Offline public-service template checks; no Docker daemon or network access."""

import re
import unittest
from pathlib import Path

from jinja2 import Environment, StrictUndefined
from test_support import array, at, obj, string, yaml_value

# isort: split
from release_json import JsonObject, JsonValue
from release_public import IDENTITY_KEYS

ROOT = Path(__file__).resolve().parents[1]
VALUES: JsonObject = {
    "scpub_config": "/etc/simplestchat-public",
    "scpub_root": "/srv/simplestchat-public",
    "scpub_server_image": "sha256:" + "a" * 64,
    "scpub_postgres_image": "postgres:18.6-bookworm@sha256:" + "b" * 64,
    "scpub_caddy_image": "caddy:2.11.2-alpine@sha256:" + "c" * 64,
    "scpub_domain": "chat.example.test",
    "scpub_webauthn_rp_id": "chat.example.test",
    "scpub_announce_ip": "192.0.2.10",
    "scpub_announce_ipv6": "2001:db8::10",
    "scpub_ipv6_network": "fd5c:5c68:a7d1::/64",
    "scpub_registration_enabled": False,
    # Sizing as group_vars derives it for a 4-vCPU host with 11967 MiB.
    "scpub_app_cpus": 3,
    "scpub_media_workers": 3,
    "scpub_app_memory_mib": 8976,
    "scpub_max_connections": 525,
    "scpub_max_rooms": 525,
    "scpub_max_participants_per_room": 525,
    "scpub_max_broadcasters_per_room": 30,
    "scpub_max_users": 100,
    "scpub_max_persisted_rooms": 100,
    "scpub_port_mbps": 1000,
    "scpub_reserved_cpus": 1,
    "scpub_reserved_memory_mib": 2991,
    "scpub_postgres_memory_mib": 1495,
    "scpub_postgres_shared_buffers_mib": 373,
    "scpub_postgres_cpus": 1,
    "scpub_caddy_memory_mib": 373,
    "scpub_caddy_cpus": 0.5,
    "scpub_turn_memory_mib": 512,
    "scpub_turn_cpus": 1,
    "scpub_turn_bps_capacity": 62500000,
    "scpub_turn_max_bps": 500000,
    "scpub_turn_user_quota": 4,
    "scpub_turn_total_quota": 525,
    "scpub_turn_relay_threads": 1,
    "scpub_turn_relay_port_min": 49160,
    "scpub_turn_relay_port_max": 50209,
    "scpub_secrets": {
        name: f"{index:064x}"
        for index, name in enumerate(
            [
                "postgres_admin_password",
                "migration_password",
                "database_password",
                "metrics_token",
                "proxy_secret",
                "owner_password",
            ],
            start=1,
        )
    },
}


def render(name: str, **overrides: JsonValue) -> str:
    """Render one configuration template with explicit fixture values."""
    values = dict(VALUES, **overrides)
    return (
        Environment(undefined=StrictUndefined, autoescape=False)  # noqa: S701 - config, not HTML.
        .from_string((ROOT / "templates" / name).read_text())
        .render(values)
    )


def environment(name: str, **overrides: JsonValue) -> dict[str, str]:
    """Read the rendered dotenv assignments without interpreting their values."""
    return dict(
        line.split("=", 1)
        for line in render(name, **overrides).splitlines()
        if line and not line.startswith("#")
    )


class PublicTemplateTests(unittest.TestCase):
    """Verify the public templates contract offline."""

    def test_database_logs_omit_bound_values_in_slow_and_error_paths(self) -> None:
        """Both PostgreSQL parameter logging controls are explicit, independent of defaults."""
        project = obj(yaml_value(render("public-compose.yml.j2")))
        command = array(obj(obj(project, "services"), "postgres"), "command")
        self.assertIn("log_min_duration_statement=500ms", command)
        self.assertIn("log_parameter_max_length=0", command)
        self.assertIn("log_parameter_max_length_on_error=0", command)

    def test_all_public_templates_render_without_missing_values(self) -> None:
        """Verify all public templates render without missing values."""
        for path in (ROOT / "templates").glob("public-*.j2"):
            # Other public templates may use deployment-controller variables.
            if path.name in {
                "public-compose.yml.j2",
                "public-app.env.j2",
                "public-migration.env.j2",
                "public-proxy.env.j2",
                "public-postgres-admin-password.j2",
                "public-pg_hba.conf.j2",
                "public-init-database.sql.j2",
                "public-runtime-grants.sql.j2",
            }:
                with self.subTest(template=path.name):
                    self.assertNotIn("{{", render(path.name))

    def test_database_and_maintenance_have_no_network_or_ports(self) -> None:
        """Verify database and maintenance have no network or ports."""
        services = obj(yaml_value(render("public-compose.yml.j2")), "services")
        for name in ["postgres", "migrate"]:
            with self.subTest(service=name):
                self.assertEqual(at(services, name, "network_mode"), "none")
                self.assertNotIn("ports", obj(services, name))
                self.assertTrue(at(services, name, "read_only"))
                self.assertEqual(at(services, name, "cap_drop"), ["ALL"])
        database = obj(services, "postgres")
        self.assertEqual(database["user"], "999:999")
        self.assertIn("listen_addresses=", array(database, "command"))
        self.assertIn("password_encryption=scram-sha-256", array(database, "command"))
        self.assertIn(
            "unix_socket_directories=/var/run/postgresql,/run/simplestchat-postgres",
            array(database, "command"),
        )
        self.assertIn("/proc/1/comm", string(database, "healthcheck", "test", 1))
        self.assertIn(
            "/srv/simplestchat-public/postgres:/var/lib/postgresql", array(database, "volumes")
        )
        maintenance = obj(services, "migrate")
        self.assertEqual(maintenance["profiles"], ["maintenance"])
        self.assertEqual(maintenance["restart"], "no")
        self.assertEqual(maintenance["image"], VALUES["scpub_server_image"])

    def test_the_base_compose_file_carries_the_servers_limits(self) -> None:
        """Compose's `environment` outranks `app.env`, so its defaults must be the server's."""
        compose = yaml_value((ROOT.parent.parent / "docker-compose.yml").read_text())
        base = obj(compose, "services", "simplestchat", "environment")
        # The server's own default; 16 blanked a browser's tiles beyond eight peers.
        self.assertEqual(
            base["MAX_CONSUMERS_PER_PARTICIPANT"], "${MAX_CONSUMERS_PER_PARTICIPANT:-64}"
        )
        # Forwarded, and empty when unset, which the server reads as no ceiling.
        self.assertEqual(base["MAX_PARTICIPANTS_PER_ROOM"], "${MAX_PARTICIPANTS_PER_ROOM:-}")
        self.assertEqual(base["MAX_BROADCASTERS_PER_ROOM"], "${MAX_BROADCASTERS_PER_ROOM:-}")
        # The managed host interpolates the base from app.env, which sets the ceiling.
        self.assertEqual(environment("public-app.env.j2")["MAX_PARTICIPANTS_PER_ROOM"], "525")
        self.assertEqual(environment("public-app.env.j2")["MAX_BROADCASTERS_PER_ROOM"], "30")

    def test_a_dual_stack_host_gets_an_ipv6_network_and_listeners(self) -> None:
        """IPv6 is announced, carried natively by the project network and offered by TURN."""
        rendered = obj(yaml_value(render("public-compose.yml.j2")))
        self.assertIs(at(rendered, "networks", "default", "enable_ipv6"), expr2=True)
        self.assertEqual(
            at(rendered, "networks", "default", "ipam", "config", 0, "subnet"),
            "fd5c:5c68:a7d1::/64",
        )
        self.assertNotIn(
            "networks", obj(yaml_value(render("public-compose.yml.j2", scpub_announce_ipv6="")))
        )
        self.assertEqual(environment("public-app.env.j2")["ANNOUNCE_IPV6"], "2001:db8::10")
        self.assertEqual(
            environment("public-app.env.j2", scpub_announce_ipv6="")["ANNOUNCE_IPV6"], ""
        )
        turn = render("turnserver.conf.j2", scpub_turn_secret="a" * 64).splitlines()
        self.assertEqual(turn.count("listening-ip=192.0.2.10"), 1)
        self.assertEqual(turn.count("listening-ip=2001:db8::10"), 1)
        without = render("turnserver.conf.j2", scpub_turn_secret="a" * 64, scpub_announce_ipv6="")
        self.assertNotIn("listening-ip=2001", without)

    def test_app_extends_existing_compose_and_proxy_is_nonroot(self) -> None:
        """Verify app extends existing compose and proxy is nonroot."""
        services = obj(yaml_value(render("public-compose.yml.j2")), "services")
        app = obj(services, "simplestchat")
        self.assertEqual(app["extends"], {"file": "./compose.base.yml", "service": "simplestchat"})
        self.assertEqual(app["env_file"], ["./app.env"])
        self.assertEqual(app["user"], "10001:10001")
        self.assertEqual(app["image"], VALUES["scpub_server_image"])
        self.assertEqual(
            app["volumes"],
            [
                "/srv/simplestchat-public/postgres-socket:/run/simplestchat-postgres:ro",
            ],
        )
        proxy = obj(services, "caddy")
        self.assertEqual(proxy["user"], "10001:10001")
        self.assertEqual(proxy["cap_add"], ["NET_BIND_SERVICE"])
        self.assertEqual(proxy["ports"], ["80:80/tcp", "443:443/tcp", "443:443/udp"])
        self.assertEqual(at(proxy, "sysctls", "net.ipv4.ip_unprivileged_port_start"), "0")
        app_logging = obj(services, "simplestchat", "logging")
        self.assertEqual(
            app_logging,
            {
                "driver": "journald",
                "options": {
                    "tag": "simplestchat.app",
                    "mode": "non-blocking",
                    "max-buffer-size": "4m",
                },
            },
        )
        for name, service in services.items():
            if name == "simplestchat":
                continue
            self.assertEqual(at(service, "logging", "driver"), "local")
            self.assertEqual(
                at(service, "logging", "options"), {"max-size": "10m", "max-file": "3"}
            )

    def test_app_configuration_is_bounded_and_secrets_are_separated(self) -> None:
        """Verify app configuration is bounded and secrets are separated."""
        app = environment("public-app.env.j2")
        migration = environment("public-migration.env.j2")
        proxy = environment("public-proxy.env.j2")
        expected = {
            "RUN_MIGRATIONS": "false",
            "REGISTRATION_ENABLED": "false",
            "ALLOW_AD_HOC_ROOMS": "false",
            "MEDIA_WORKERS": "3",
            "RTC_PORT_END": "40002",
            "SIMPLESTCHAT_CPUS": "3",
            "SIMPLESTCHAT_MEMORY_LIMIT": "8976m",
            "MAX_CONNECTIONS": "525",
            "MAX_USERS": "100",
            "MAX_ROOMS": "525",
            "MAX_PERSISTED_ROOMS": "100",
            "WEBAUTHN_RP_ID": "chat.example.test",
            "WEBAUTHN_ORIGIN": "https://chat.example.test",
        }
        for name, value in expected.items():
            self.assertEqual(app[name], value)
        self.assertEqual(
            environment("public-app.env.j2", scpub_registration_enabled=True)[
                "REGISTRATION_ENABLED"
            ],
            "true",
        )
        self.assertIn("simplestchat_app:", app["DATABASE_URL"])
        self.assertIn("simplestchat_migrate:", migration["DATABASE_URL"])
        for values in [app, migration]:
            self.assertTrue(
                values["DATABASE_URL"].endswith("?host=/run/simplestchat-postgres&sslmode=disable")
            )
        self.assertEqual(migration["RUN_MIGRATIONS"], "true")
        self.assertEqual(migration["BIND_ADDR"], "127.0.0.1")
        self.assertEqual(migration["REGISTRATION_ENABLED"], "false")
        self.assertEqual(proxy["TRUSTED_PROXY_SECRET"], app["TRUSTED_PROXY_SECRET"])
        self.assertEqual(proxy["CADDY_UPSTREAM"], "simplestchat:3000")
        self.assertNotIn("METRICS_TOKEN", migration)
        self.assertNotIn("DATABASE_URL", proxy)
        self.assertNotIn(string(VALUES, "scpub_secrets", "owner_password"), "\n".join(app.values()))

    def test_passkey_identity_can_remain_at_the_parent_without_widening_origins(self) -> None:
        """Inventory retains an existing RP ID while HTTP and WebAuthn use one new origin."""
        domain = "the.research.clinic"
        for identity in (domain, "research.clinic"):
            app = environment(
                "public-app.env.j2", scpub_domain=domain, scpub_webauthn_rp_id=identity
            )
            self.assertEqual(app["WEBAUTHN_RP_ID"], identity)
            self.assertEqual(app["WEBAUTHN_ORIGIN"], f"https://{domain}")
            self.assertEqual(app["ALLOWED_ORIGINS"], f"https://{domain}")

    def test_a_pinned_worker_count_sets_the_media_port_range(self) -> None:
        """Each media worker takes one port from 40000 upwards."""
        rendered = environment("public-app.env.j2", scpub_media_workers=2)
        self.assertEqual(rendered["RTC_PORT_END"], "40001")
        self.assertEqual(rendered["MEDIA_WORKERS"], "2")

    def test_ipv6_only_network_carries_the_primary_announced_address(self) -> None:
        """An IPv6 primary needs a native Compose network even without a second address."""
        compose = yaml_value(
            render(
                "public-compose.yml.j2", scpub_announce_ip="2001:db8::10", scpub_announce_ipv6=""
            )
        )
        self.assertIs(at(compose, "networks", "default", "enable_ipv6"), expr2=True)

    def test_the_maintenance_helper_protects_every_secret_and_identity_line(self) -> None:
        """A candidate app.env may change sizing; the helper refuses secret or identity changes."""
        other_secrets: JsonObject = {
            name: f"{index:064x}"
            for index, name in enumerate(obj(VALUES["scpub_secrets"]), start=101)
        }
        first = environment(
            "public-app.env.j2", scpub_turn_enabled=True, scpub_turn_secret="a" * 64
        )
        second = environment(
            "public-app.env.j2",
            scpub_turn_enabled=True,
            scpub_turn_secret="b" * 64,
            scpub_secrets=other_secrets,
            scpub_domain="other.example.test",
            scpub_webauthn_rp_id="other.example.test",
            scpub_announce_ip="192.0.2.99",
            scpub_announce_ipv6="2001:db8::99",
            scpub_server_image="sha256:" + "d" * 64,
        )
        identity = {key for key in first if first[key] != second[key]}
        self.assertTrue(identity, "the fixture changed nothing")
        self.assertIn("ANNOUNCE_IPV6", identity)
        self.assertLessEqual(identity, IDENTITY_KEYS)
        self.assertLessEqual({"RUN_MIGRATIONS", "BIND_ADDR", "PORT"}, IDENTITY_KEYS)
        # Sizing lines stay free for the candidate to follow the host.
        self.assertFalse(
            IDENTITY_KEYS
            & {
                "MEDIA_WORKERS",
                "RTC_PORT_END",
                "SIMPLESTCHAT_CPUS",
                "SIMPLESTCHAT_MEMORY_LIMIT",
                "MAX_CONNECTIONS",
                "MAX_ROOMS",
                "MAX_PARTICIPANTS_PER_ROOM",
                "MAX_BROADCASTERS_PER_ROOM",
            }
        )

    def test_database_authentication_and_grants_are_explicit(self) -> None:
        """Verify database authentication and grants are explicit."""
        hba = [
            line.split()
            for line in render("public-pg_hba.conf.j2").splitlines()
            if line and not line.startswith("#")
        ]
        self.assertEqual(
            hba,
            [
                ["local", "all", "postgres", "peer"],
                ["local", "simplestchat", "simplestchat_migrate", "scram-sha-256"],
                ["local", "simplestchat", "simplestchat_app", "scram-sha-256"],
                ["local", "all", "all", "reject"],
                ["host", "all", "all", "0.0.0.0/0", "reject"],
                ["host", "all", "all", "::/0", "reject"],
            ],
        )
        bootstrap = render("public-init-database.sql.j2")
        self.assertEqual(
            bootstrap.count("NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"), 2
        )
        self.assertIn("REVOKE ALL ON DATABASE simplestchat FROM PUBLIC", bootstrap)
        self.assertIn("REVOKE ALL ON SCHEMA public FROM PUBLIC", bootstrap)
        self.assertIn("GRANT USAGE, CREATE ON SCHEMA public TO simplestchat_migrate", bootstrap)
        self.assertNotIn("GRANT USAGE, CREATE ON SCHEMA public TO simplestchat_app", bootstrap)
        self.assertIn("CREATE EXTENSION pg_trgm WITH SCHEMA public", bootstrap)
        grants = render("public-runtime-grants.sql.j2")
        self.assertNotIn("ALL TABLES", grants)
        self.assertIn("GRANT SELECT, INSERT, UPDATE\n  ON public.users\n", grants)
        for table in ["sessions", "rooms", "room_roles", "room_states", "room_reports"]:
            self.assertIn(f"public.{table}", grants)
        statements = "\n".join(line for line in grants.splitlines() if not line.startswith("--"))
        self.assertNotIn("_sqlx_migrations", statements)

    @staticmethod
    def production_rust(source: str) -> str:
        """Omit only cfg(test) items, retaining production declarations that follow them."""
        tokens = list(
            re.finditer(
                r'(?s)//[^\n]*|/\*.*?\*/|(?:br|r)(?P<hash>\#{0,32})".*?"(?P=hash)'
                + r"""|b?"(?:\\.|[^"\\])*"|b?'(?:\\.|[^'\\])'"""
                + r"|[A-Za-z_][A-Za-z0-9_]*|[^\s]",
                source,
            )
        )
        expected = ["#", "[", "cfg", "(", "test", ")", "]"]
        result: list[str] = []
        cursor = 0
        index = 0
        while index < len(tokens):
            if [token.group() for token in tokens[index : index + 7]] != expected:
                index += 1
                continue
            result.append(source[cursor : tokens[index].start()])
            index += 7
            depth = 0
            while index < len(tokens):
                token = tokens[index]
                index += 1
                value = token.group()
                if value == "{":
                    depth += 1
                elif value == "}":
                    depth -= 1
                    if depth == 0:
                        break
                elif value == ";" and depth == 0:
                    break
            cursor = tokens[index - 1].end()
        result.append(source[cursor:])
        return "".join(result)

    def test_runtime_grant_scanner_keeps_production_after_test_items(self) -> None:
        """Test-only imports/functions/modules cannot hide later runtime SQL statements."""
        source = """
#[cfg(test)] use fixture::owner;
fn first() { query("INSERT INTO rooms VALUES (1)"); }
#[cfg(test)]
mod tests { fn fixture() { query(r#"DELETE FROM users; }"#); } }
#[cfg(test)]
fn helper() { query("UPDATE users SET name = 'fixture'"); }
fn runtime() { query("UPDATE rooms SET name = 'current'"); }
"""
        production = self.production_rust(source)
        self.assertIn("INSERT INTO rooms", production)
        self.assertIn("UPDATE rooms", production)
        self.assertNotIn("DELETE FROM users", production)
        self.assertNotIn("UPDATE users", production)
        self.assertNotIn("fixture::owner", production)

    def test_runtime_grants_cover_every_statement_the_server_runs(self) -> None:
        """Every table the server inserts into, updates or deletes from is granted that right."""
        granted: dict[str, set[str]] = {}
        grants = render("public-runtime-grants.sql.j2")
        for grant in re.finditer(
            r"GRANT ([A-Z, ]+)\n\s+ON ([^;]+?)\n\s+TO simplestchat_app;", grants
        ):
            rights = {right.strip() for right in str(grant.group(1)).split(",")}
            for table in str(grant.group(2)).split(","):
                granted[table.strip().removeprefix("public.")] = rights
        tables = (
            "users|webauthn_credentials|sessions|rooms|room_roles|room_states|room_reports"
            + "|moderation_events|invites|invite_redemptions|session_access_tokens"
        )
        statement = re.compile(r"\b(INSERT INTO|UPDATE|DELETE FROM)\s+(" + tables + r")\b")
        needed: dict[str, set[str]] = {}
        repository = ROOT.parents[1]
        sources = list((repository / "src").rglob("*.rs"))
        self.assertTrue(sources, f"no Rust sources under {repository}")
        for path in sources:
            if path.name.endswith("_tests.rs"):
                continue
            production = self.production_rust(path.read_text())
            for found in statement.finditer(production):
                needed.setdefault(str(found.group(2)), set()).add(str(found.group(1)).split()[0])
        self.assertEqual(needed["webauthn_credentials"], {"INSERT", "UPDATE", "DELETE"})
        # Access tokens are issued and pruned, never rewritten.
        self.assertEqual(needed["session_access_tokens"], {"INSERT", "DELETE"})
        self.assertNotIn("DELETE", needed.get("users", set()))
        for table, rights in needed.items():
            self.assertLessEqual(rights, granted.get(table, set()), table)


if __name__ == "__main__":
    _ = unittest.main()
