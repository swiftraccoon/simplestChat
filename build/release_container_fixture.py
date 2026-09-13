"""Render private, loopback-only release fixtures without starting containers.

The caller owns the disposable directories, certificate generation, ownership
changes, and cleanup. Existing files are never replaced, and a partial write is
retained for inspection. Production templates supply the runtime restrictions;
only fixture identity, network exposure, and local certificate selection differ.
"""

import json
import os
from pathlib import Path
import re
import secrets
import stat

from jinja2 import Environment, StrictUndefined
import yaml


PROJECT = Path(__file__).resolve().parents[1]
TEMPLATES = PROJECT / "ops/ansible/templates"
LABEL = "simplestchat.release-test"


class QuotedImage(str):
    """Preserve the production helper's exact quoted image selections."""


class FixtureDumper(yaml.SafeDumper):
    """Keep fixture serialization local to this module."""


FixtureDumper.add_representer(
    QuotedImage,
    lambda dumper, value: dumper.represent_scalar("tag:yaml.org,2002:str", value, style='"'),
)


def _directory(path: Path) -> None:
    if not path.is_absolute() or path.resolve() != path:
        raise ValueError("Fixture directories must be absolute and contain no symlinks")
    if not stat.S_ISDIR(path.lstat().st_mode):
        raise ValueError("The caller must create each fixture directory")
    if any(character in str(path) for character in ('"', "'", "\\", "$", ":")) or any(
        ord(character) < 32 or ord(character) == 127 for character in str(path)
    ):
        raise ValueError("Fixture directory contains an unsupported configuration character")


def _replace_once(text: str, old: str, new: str) -> str:
    if text.count(old) != 1:
        raise ValueError("Production fixture anchor changed; review its rendering")
    return text.replace(old, new, 1)


def _environment(text: str, name: str, value: str) -> str:
    lines = text.splitlines()
    if sum(line.startswith(name + "=") for line in lines) != 1:
        raise ValueError("Production environment setting changed; review its rendering")
    return "\n".join(name + "=" + value if line.startswith(name + "=") else line for line in lines) + "\n"


def render_fixture(
    config: Path,
    root: Path,
    image: str,
    revision: str,
    token: str,
    postgres_image: str,
    caddy_image: str,
) -> None:
    """Exclusively create a fixture configuration in caller-created directories.

    The explicit certificate and key must subsequently be supplied as
    ``fixture.crt`` and ``fixture.key`` in ``config``. This renderer does not
    contact Docker, change host trust, create directories, or modify ownership.
    """
    for path in (config, root):
        _directory(path)
    expressions = (
        (image, r"sha256:[a-f0-9]{64}"),
        (revision, r"[a-f0-9]{40}"),
        (token, r"[a-f0-9]{32}"),
        (postgres_image, r"docker\.io/library/postgres:[a-z0-9.-]+@sha256:[a-f0-9]{64}"),
        (caddy_image, r"docker\.io/library/caddy:[a-z0-9.-]+@sha256:[a-f0-9]{64}"),
    )
    if any(not isinstance(value, str) or re.fullmatch(expression, value) is None
           for value, expression in expressions):
        raise ValueError("Fixture image identities, revision, and ownership token must be explicit")
    values = {
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
                "postgres_admin_password", "migration_password", "database_password",
                "jwt_secret", "metrics_token", "proxy_secret", "owner_password",
            )
        },
    }
    environment = Environment(undefined=StrictUndefined, keep_trailing_newline=True)
    outputs = {
        filename: environment.from_string((TEMPLATES / template).read_text()).render(values)
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
    base = yaml.safe_load((PROJECT / "docker-compose.yml").read_text())
    base["services"]["simplestchat"]["ports"] = [
        "127.0.0.1:3000:3000", "127.0.0.1:40000:40000/udp",
    ]
    base["services"]["simplestchat"].setdefault("labels", {})[LABEL] = token
    outputs["compose.base.yml"] = yaml.dump(base, Dumper=FixtureDumper, sort_keys=False)
    public = yaml.safe_load(outputs["compose.public.yml"])
    for name, service in public["services"].items():
        service.setdefault("labels", {})[LABEL] = token
        if name in ("simplestchat", "migrate"):
            service["image"] = QuotedImage(image)
    public["services"]["caddy"]["ports"] = ["127.0.0.1:443:443/tcp"]
    public["services"]["caddy"]["volumes"].extend([
        f"{config}/fixture.crt:/etc/caddy/fixture.crt:ro",
        f"{config}/fixture.key:/etc/caddy/fixture.key:ro",
    ])
    public.setdefault("networks", {}).setdefault("default", {}).setdefault("labels", {})[LABEL] = token
    outputs["compose.public.yml"] = yaml.dump(public, Dumper=FixtureDumper, sort_keys=False)
    caddy = _replace_once(
        (PROJECT / "Caddyfile").read_text(),
        "{\n\t# Keep Caddy's loopback-only admin endpoint for reloads. Do not publish its\n"
        "\t# port from a container or override it with a publicly reachable address.\n",
        "{\n\t# Explicit fixture certificate: no ACME, redirects, or admin listener.\n"
        "\tadmin off\n\tauto_https off\n",
    )
    caddy = _replace_once(
        caddy, "{$CADDY_DOMAIN:simplestchat.example.com} {",
        "https://localhost {\n\ttls /etc/caddy/fixture.crt /etc/caddy/fixture.key",
    )
    outputs["Caddyfile"] = _replace_once(
        caddy, "reverse_proxy {$CADDY_UPSTREAM:127.0.0.1:3000}", "reverse_proxy simplestchat:3000",
    )
    outputs["images.json"] = json.dumps({
        "revision": revision, "serverImage": image,
        "postgresImage": postgres_image, "caddyImage": caddy_image,
    }, indent=2) + "\n"
    for filename in outputs:
        target = config / filename
        if target.exists() or target.is_symlink():
            raise FileExistsError(f"Fixture output already exists: {filename}")
    for filename, text in outputs.items():
        descriptor = os.open(config / filename, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(descriptor, "w", encoding="utf-8") as output:
            output.write(text if text.endswith("\n") else text + "\n")
