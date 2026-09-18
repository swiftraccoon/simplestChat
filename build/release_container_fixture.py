"""Render private, loopback-only release fixtures without starting containers.

The caller owns the disposable directories, certificate generation, ownership
changes, and cleanup. Existing files are never replaced, and a partial write is
retained for inspection. Production templates supply the runtime restrictions;
only fixture identity, network exposure, and local certificate selection differ.
"""

from __future__ import annotations

import json
import os
import re
import secrets
import stat
from dataclasses import dataclass
from pathlib import Path
from typing import cast, final

import yaml
from jinja2 import Environment, StrictUndefined
from release_json import JsonObject, array_value, json_value, object_value

PROJECT = Path(__file__).resolve().parents[1]
TEMPLATES = PROJECT / "ops/ansible/templates"
LABEL = "simplestchat.release-test"
MIN_PRINTABLE = 32
DELETE_CHARACTER = 127


@final
class QuotedImage(str):
    """Preserve the production helper's exact quoted image selections."""

    __slots__ = ()


class FixtureDumper(yaml.SafeDumper):
    """Keep fixture serialization local to this module."""


def _quoted_image(_dumper: yaml.SafeDumper, value: QuotedImage) -> yaml.nodes.ScalarNode:
    return yaml.nodes.ScalarNode("tag:yaml.org,2002:str", value, style='"')


FixtureDumper.add_representer(QuotedImage, _quoted_image)


@dataclass(frozen=True)
class FixtureIdentity:
    """The explicit immutable image identities and ownership token for one fixture."""

    image: str
    revision: str
    token: str
    postgres_image: str
    caddy_image: str


def load_yaml(text: str) -> JsonObject:
    """Validate configuration YAML before using its recursive object structure."""
    return object_value(json_value(cast("object", yaml.safe_load(text))))


def _directory(path: Path) -> None:
    if not path.is_absolute() or path.resolve() != path:
        message = "Fixture directories must be absolute and contain no symlinks"
        raise ValueError(message)
    if not stat.S_ISDIR(path.lstat().st_mode):
        message = "The caller must create each fixture directory"
        raise ValueError(message)
    if any(character in str(path) for character in ('"', "'", "\\", "$", ":")) or any(
        ord(character) < MIN_PRINTABLE or ord(character) == DELETE_CHARACTER
        for character in str(path)
    ):
        message = "Fixture directory contains an unsupported configuration character"
        raise ValueError(message)


def _replace_once(text: str, old: str, new: str) -> str:
    if text.count(old) != 1:
        message = "Production fixture anchor changed; review its rendering"
        raise ValueError(message)
    return text.replace(old, new, 1)


def _environment(text: str, name: str, value: str) -> str:
    lines = text.splitlines()
    if sum(line.startswith(name + "=") for line in lines) != 1:
        message = "Production environment setting changed; review its rendering"
        raise ValueError(message)
    return (
        "\n".join(name + "=" + value if line.startswith(name + "=") else line for line in lines)
        + "\n"
    )


def render_fixture(
    config: Path,
    root: Path,
    identity: FixtureIdentity,
) -> None:
    """Exclusively create a fixture configuration in caller-created directories.

    The explicit certificate and key must subsequently be supplied as
    ``fixture.crt`` and ``fixture.key`` in ``config``. This renderer does not
    contact Docker, change host trust, create directories, or modify ownership.
    """
    for path in (config, root):
        _directory(path)
    image, revision, token = identity.image, identity.revision, identity.token
    postgres_image, caddy_image = identity.postgres_image, identity.caddy_image
    expressions = (
        (image, r"sha256:[a-f0-9]{64}"),
        (revision, r"[a-f0-9]{40}"),
        (token, r"[a-f0-9]{32}"),
        (postgres_image, r"docker\.io/library/postgres:[a-z0-9.-]+@sha256:[a-f0-9]{64}"),
        (caddy_image, r"docker\.io/library/caddy:[a-z0-9.-]+@sha256:[a-f0-9]{64}"),
    )
    if any(re.fullmatch(expression, value) is None for value, expression in expressions):
        message = "Fixture image identities, revision, and ownership token must be explicit"
        raise ValueError(message)
    values: JsonObject = {
        "scpub_config": str(config),
        "scpub_root": str(root),
        "scpub_server_image": image,
        "scpub_postgres_image": postgres_image,
        "scpub_caddy_image": caddy_image,
        "scpub_domain": "localhost",
        "scpub_announce_ip": "127.0.0.1",
        "scpub_registration_enabled": False,
        "scpub_secrets": {
            name: secrets.token_hex(32)
            for name in (
                "postgres_admin_password",
                "migration_password",
                "database_password",
                "jwt_secret",
                "metrics_token",
                "proxy_secret",
                "owner_password",
            )
        },
    }
    # These are shell/SQL/Compose configuration templates, never HTML content.
    environment = Environment(
        undefined=StrictUndefined,
        keep_trailing_newline=True,
        autoescape=False,  # noqa: S701
    )
    outputs = {
        filename: environment.from_string(
            (TEMPLATES / template).read_text(encoding="utf-8")
        ).render(values)
        for filename, template in (
            ("app.env", "public-app.env.j2"),
            ("migration.env", "public-migration.env.j2"),
            ("proxy.env", "public-proxy.env.j2"),
            ("compose.public.yml", "public-compose.yml.j2"),
            ("pg_hba.conf", "public-pg_hba.conf.j2"),
            ("init-database.sql", "public-init-database.sql.j2"),
            ("runtime-grants.sql", "public-runtime-grants.sql.j2"),
            ("postgres-admin-password", "public-postgres-admin-password.j2"),
        )
    }
    outputs["app.env"] = _environment(outputs["app.env"], "MEDIA_WORKERS", "1")
    outputs["app.env"] = _environment(outputs["app.env"], "RTC_PORT_END", "40000")
    base = load_yaml((PROJECT / "docker-compose.yml").read_text(encoding="utf-8"))
    base_services = object_value(base["services"])
    base_app = object_value(base_services["simplestchat"])
    base_app["ports"] = [
        "127.0.0.1:3000:3000",
        "127.0.0.1:40000:40000/udp",
    ]
    object_value(base_app.setdefault("labels", {}))[LABEL] = token
    outputs["compose.base.yml"] = yaml.dump(base, Dumper=FixtureDumper, sort_keys=False)
    public = load_yaml(outputs["compose.public.yml"])
    services = object_value(public["services"])
    for name, value in services.items():
        service = object_value(value)
        object_value(service.setdefault("labels", {}))[LABEL] = token
        if name in ("simplestchat", "migrate"):
            service["image"] = QuotedImage(image)
    caddy_service = object_value(services["caddy"])
    caddy_service["ports"] = ["127.0.0.1:443:443/tcp"]
    array_value(caddy_service["volumes"]).extend(
        [
            f"{config}/fixture.crt:/etc/caddy/fixture.crt:ro",
            f"{config}/fixture.key:/etc/caddy/fixture.key:ro",
        ]
    )
    network = object_value(
        object_value(public.setdefault("networks", {})).setdefault("default", {})
    )
    object_value(network.setdefault("labels", {}))[LABEL] = token
    outputs["compose.public.yml"] = yaml.dump(public, Dumper=FixtureDumper, sort_keys=False)
    caddy = _replace_once(
        (PROJECT / "Caddyfile").read_text(encoding="utf-8"),
        "{\n\t# Keep Caddy's loopback-only admin endpoint for reloads. Do not publish its\n"
        + "\t# port from a container or override it with a publicly reachable address.\n",
        "{\n\t# Explicit fixture certificate: no ACME, redirects, or admin listener.\n"
        + "\tadmin off\n\tauto_https off\n",
    )
    caddy = _replace_once(
        caddy,
        "{$CADDY_DOMAIN:simplestchat.example.com} {",
        "https://localhost {\n\ttls /etc/caddy/fixture.crt /etc/caddy/fixture.key",
    )
    outputs["Caddyfile"] = _replace_once(
        caddy,
        "reverse_proxy {$CADDY_UPSTREAM:127.0.0.1:3000}",
        "reverse_proxy simplestchat:3000",
    )
    outputs["images.json"] = (
        json.dumps(
            {
                "revision": revision,
                "serverImage": image,
                "postgresImage": postgres_image,
                "caddyImage": caddy_image,
            },
            indent=2,
        )
        + "\n"
    )
    for filename in outputs:
        target = config / filename
        if target.exists() or target.is_symlink():
            message = f"Fixture output already exists: {filename}"
            raise FileExistsError(message)
    for filename, text in outputs.items():
        descriptor = os.open(config / filename, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(descriptor, "w", encoding="utf-8") as output:
            _ = output.write(text if text.endswith("\n") else text + "\n")
