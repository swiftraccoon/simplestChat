"""Offline public-service template checks; no Docker daemon or network access."""

import re
import unittest
from pathlib import Path
from typing import TYPE_CHECKING, cast

from jinja2 import Environment, StrictUndefined
from jinja2.nativetypes import NativeEnvironment
from test_support import array, at, obj, string, yaml_value

# isort: split
import capacity
from release_json import JsonObject, JsonValue
from release_public import IDENTITY_KEYS
from sizing import FilterModule

if TYPE_CHECKING:
    from collections.abc import MutableMapping

ROOT = Path(__file__).resolve().parents[1]
VALUES: JsonObject = {
    "scpub_config": "/etc/simplestchat-public",
    "scpub_root": "/srv/simplestchat-public",
    "scpub_server_image": "sha256:" + "a" * 64,
    "scpub_postgres_image": "postgres:18.6-bookworm@sha256:" + "b" * 64,
    "scpub_caddy_image": "caddy:2.11.2-alpine@sha256:" + "c" * 64,
    "scpub_domain": "chat.example.test",
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
                "jwt_secret",
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


def derive_group_vars(facts: JsonObject, overrides: JsonObject) -> dict[str, str]:
    """Render every templated sizing variable of group_vars from the given facts, in order."""
    group_vars = obj(yaml_value((ROOT / "group_vars/benchmark_hosts.yml").read_text()))
    jinja = NativeEnvironment(undefined=StrictUndefined, autoescape=False)
    # Jinja types only its built-in filters, although this is its public extension API.
    cast("MutableMapping[str, object]", jinja.filters).update(FilterModule.filters())
    derived = [
        name
        for name, source in group_vars.items()
        if name.startswith("scpub_")
        and isinstance(source, (str, int, float))
        and name != "scpub_registration_enabled"
        and (isinstance(source, (int, float)) or source.startswith("{{"))
    ]
    values: dict[str, object] = dict(facts)
    for name in derived:
        source = overrides.get(name, group_vars[name])
        values[name] = (
            cast("object", jinja.from_string(source).render(values))
            if isinstance(source, str) and source.startswith("{{")
            else source
        )
    return {name: str(values[name]) for name in derived}


# Ansible always defines the default-route facts, as empty objects without a route.
DUAL_STACK: JsonObject = {
    "ansible_default_ipv4": {"address": "192.0.2.10", "interface": "eth0"},
    "ansible_default_ipv6": {"address": "2001:db8::10", "interface": "eth0"},
}
FOUR_ONLY: JsonObject = {
    "ansible_default_ipv4": {"address": "192.0.2.10", "interface": "ens3"},
    "ansible_default_ipv6": {},
}
SIX_ONLY: JsonObject = {
    "ansible_default_ipv4": {},
    "ansible_default_ipv6": {"address": "2001:db8::10", "interface": "eth0"},
}


class PublicTemplateTests(unittest.TestCase):
    """Verify the public templates contract offline."""

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
        self.assertNotIn("JWT_SECRET", migration)
        self.assertNotIn("DATABASE_URL", proxy)
        self.assertNotIn(string(VALUES, "scpub_secrets", "owner_password"), "\n".join(app.values()))

    def test_sizing_defaults_follow_the_host_and_yield_to_the_inventory(self) -> None:
        """The group_vars sizing expressions size a host the way build/capacity.py does."""
        vps = derive_group_vars(
            {"ansible_processor_vcpus": 4, "ansible_memtotal_mb": 11967, **DUAL_STACK}, {}
        )
        self.assertEqual(vps["scpub_app_cpus"], "3")
        self.assertEqual(vps["scpub_media_workers"], "3")
        self.assertEqual(int(vps["scpub_app_memory_mib"]), 11967 - 2991)
        self.assertEqual(vps["scpub_max_connections"], "525")
        self.assertEqual(vps["scpub_max_rooms"], "525")
        self.assertEqual(vps["scpub_max_participants_per_room"], "525")
        self.assertEqual(vps["scpub_reserved_memory_mib"], "2991")
        self.assertEqual(
            (vps["scpub_postgres_memory_mib"], vps["scpub_postgres_cpus"]), ("1495", "1")
        )
        self.assertEqual(vps["scpub_postgres_shared_buffers_mib"], "373")
        self.assertEqual((vps["scpub_caddy_memory_mib"], vps["scpub_caddy_cpus"]), ("373", "0.5"))
        self.assertEqual(vps["scpub_turn_bps_capacity"], "62500000")
        self.assertEqual(vps["scpub_turn_total_quota"], "525")
        self.assertEqual(vps["scpub_turn_relay_port_max"], "50209")
        large = derive_group_vars(
            {"ansible_processor_vcpus": 16, "ansible_memtotal_mb": 65536, **SIX_ONLY}, {}
        )
        self.assertEqual((large["scpub_app_cpus"], large["scpub_media_workers"]), ("15", "15"))
        self.assertEqual(int(large["scpub_app_memory_mib"]), 65536 - 16384)
        self.assertEqual(large["scpub_max_connections"], "800")
        self.assertEqual(
            (large["scpub_postgres_memory_mib"], large["scpub_caddy_memory_mib"]), ("4096", "2048")
        )
        self.assertEqual(large["scpub_turn_relay_port_max"], "50759")
        small = derive_group_vars(
            {"ansible_processor_vcpus": 1, "ansible_memtotal_mb": 1024, **FOUR_ONLY}, {}
        )
        self.assertEqual((small["scpub_app_cpus"], small["scpub_media_workers"]), ("1", "1"))
        self.assertEqual(small["scpub_app_memory_mib"], "512")
        self.assertEqual((small["scpub_max_connections"], small["scpub_max_rooms"]), ("175", "175"))
        self.assertEqual(
            (small["scpub_postgres_memory_mib"], small["scpub_caddy_memory_mib"]), ("1024", "256")
        )
        # Overrides carry through: a pinned worker count, a bigger reserve, a slower port.
        pinned = derive_group_vars(
            {"ansible_processor_vcpus": 4, "ansible_memtotal_mb": 11967, **DUAL_STACK},
            {"scpub_media_workers": 2, "scpub_reserved_cpus": 2, "scpub_port_mbps": 100},
        )
        self.assertEqual((pinned["scpub_app_cpus"], pinned["scpub_media_workers"]), ("2", "2"))
        self.assertEqual(pinned["scpub_max_connections"], "80")
        self.assertEqual((pinned["scpub_postgres_cpus"], pinned["scpub_caddy_cpus"]), ("2", "1.0"))
        self.assertEqual(pinned["scpub_turn_bps_capacity"], "6250000")
        rendered = environment("public-app.env.j2", scpub_media_workers=2)
        self.assertEqual(rendered["RTC_PORT_END"], "40001")
        self.assertEqual(rendered["MEDIA_WORKERS"], "2")

    def test_managed_and_adviser_limits_agree_across_host_shapes(self) -> None:
        """Both setup paths obey memory, bandwidth and runtime limits using one model."""
        for cpus, memory, port in [
            (1, 1024, 1000),
            (8, 2048, 1000),
            (8, 16384, 1000),
            (64, 131072, 1000),
            (128, 262144, 40000),
            (16, 65536, 100),
        ]:
            with self.subTest(cpus=cpus, memory=memory, port=port):
                managed = derive_group_vars(
                    {"ansible_processor_vcpus": cpus, "ansible_memtotal_mb": memory, **FOUR_ONLY},
                    {"scpub_port_mbps": port},
                )
                suggested = capacity.suggest_for(
                    capacity.Host(cpus, memory, port), capacity.REFERENCE_CEILINGS
                )
                settings = dict(line.split("=", 1) for line in suggested.settings)
                for managed_key, runtime_key in [
                    ("scpub_media_workers", "MEDIA_WORKERS"),
                    ("scpub_max_connections", "MAX_CONNECTIONS"),
                    ("scpub_max_participants_per_room", "MAX_PARTICIPANTS_PER_ROOM"),
                ]:
                    self.assertEqual(managed[managed_key], settings[runtime_key])
                self.assertLessEqual(int(settings["MEDIA_WORKERS"]), 64)
                self.assertLessEqual(int(settings["MAX_PARTICIPANTS_PER_ROOM"]), 10000)
                # coturn accepts bytes/second; its aggregate cap is half the sold port.
                self.assertEqual(int(managed["scpub_turn_bps_capacity"]) * 8, port * 500000)
                self.assertEqual(int(managed["scpub_turn_max_bps"]) * 8, 4000000)

    def test_ipv6_only_network_carries_the_primary_announced_address(self) -> None:
        """An IPv6 primary needs a native Compose network even without a second address."""
        compose = yaml_value(
            render(
                "public-compose.yml.j2", scpub_announce_ip="2001:db8::10", scpub_announce_ipv6=""
            )
        )
        self.assertIs(at(compose, "networks", "default", "enable_ipv6"), expr2=True)

    def test_announced_addresses_follow_the_default_routes(self) -> None:
        """A dual-stack host announces both families, a single-stack host its one; overrides win."""
        facts = {"ansible_processor_vcpus": 4, "ansible_memtotal_mb": 11967}
        dual = derive_group_vars({**facts, **DUAL_STACK}, {})
        self.assertEqual(dual["scpub_announce_ip"], "192.0.2.10")
        self.assertEqual(dual["scpub_announce_ipv6"], "2001:db8::10")
        self.assertEqual(dual["scpub_transfer_interface"], "eth0")
        six = derive_group_vars({**facts, **SIX_ONLY}, {})
        self.assertEqual(
            (six["scpub_announce_ip"], six["scpub_announce_ipv6"]), ("2001:db8::10", "")
        )
        four = derive_group_vars({**facts, **FOUR_ONLY}, {})
        self.assertEqual(
            (four["scpub_announce_ipv6"], four["scpub_transfer_interface"]), ("", "ens3")
        )
        kept_four = derive_group_vars({**facts, **DUAL_STACK}, {"scpub_announce_ipv6": ""})
        self.assertEqual(kept_four["scpub_announce_ipv6"], "")

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
            + "|moderation_events|invites"
        )
        statement = re.compile(r"\b(INSERT INTO|UPDATE|DELETE FROM)\s+(" + tables + r")\b")
        needed: dict[str, set[str]] = {}
        repository = ROOT.parents[1]
        sources = list((repository / "src").rglob("*.rs"))
        self.assertTrue(sources, f"no Rust sources under {repository}")
        for path in sources:
            if path.name.endswith("_tests.rs"):
                continue
            # Tests sit after the first cfg(test) in a file and run as the owner.
            production = path.read_text().split("#[cfg(test)]", maxsplit=1)[0]
            for found in statement.finditer(production):
                needed.setdefault(str(found.group(2)), set()).add(str(found.group(1)).split()[0])
        self.assertEqual(needed["webauthn_credentials"], {"INSERT", "UPDATE", "DELETE"})
        self.assertNotIn("DELETE", needed.get("users", set()))
        for table, rights in needed.items():
            self.assertLessEqual(rights, granted.get(table, set()), table)


if __name__ == "__main__":
    _ = unittest.main()
