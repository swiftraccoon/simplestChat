"""Enforce the managed server's execution profile without exposing configuration values.

These checks describe the shipped application and migration service. They do
not make claims about arbitrary programs or overrides chosen by a host owner.
The image audit separately proves the immutable filesystem and native linkage.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

from release_json import JsonObject, array_value, decode_json, object_value, string_value

MAX_CONFIGURATION = 2 * 1024 * 1024
FORBIDDEN_PREFIXES = ("LD_", "OPENSSL_")
FORBIDDEN_NAMES = frozenset({"GLIBC_TUNABLES", "GCONV_PATH", "LOCPATH"})
SERVER_COMMAND = ["/app/simplestChat"]
# Fields emitted by the current canonical Compose model. Unknown execution
# features (including lifecycle hooks) require review rather than being ignored.
SERVICE_FIELDS = frozenset(
    {
        "build",
        "cap_drop",
        "command",
        "cpus",
        "depends_on",
        "entrypoint",
        "environment",
        "healthcheck",
        "image",
        "init",
        "labels",
        "logging",
        "mem_limit",
        "network_mode",
        "networks",
        "pids_limit",
        "ports",
        "profiles",
        "read_only",
        "restart",
        "security_opt",
        "stop_grace_period",
        "tmpfs",
        "ulimits",
        "user",
        "volumes",
        "working_dir",
    }
)


class RuntimeProfileError(ValueError):
    """A fixed failure code; configuration names and values never enter diagnostics."""


class Arguments(argparse.Namespace):
    """Retain the parser's single checked operation without dynamic attribute types."""

    kind: str = ""
    image_id: str | None = None


def require(condition: object, code: str) -> None:
    """Reject an unsupported execution profile without printing private input."""
    if not condition:
        raise RuntimeProfileError(code)


def forbidden_environment(name: str) -> bool:
    """Recognize loader, crypto-provider and locale-module override names."""
    return name.startswith(FORBIDDEN_PREFIXES) or name in FORBIDDEN_NAMES


def validate_environment_names(names: list[str]) -> None:
    """Reject duplicate, malformed and forbidden names, including empty overrides."""
    require(len(names) == len(set(names)), "runtime_environment_duplicate")
    require(
        all(re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", name) for name in names)
        and not any(forbidden_environment(name) for name in names),
        "runtime_environment_override",
    )


def validate_image(config: JsonObject) -> None:
    """Require the exact image execution defaults before its managed use."""
    require(
        config.get("User") == "10001:10001"
        and config.get("WorkingDir") == "/app"
        and config.get("Cmd") == SERVER_COMMAND
        and config.get("Entrypoint") in (None, [])
        and config.get("Healthcheck") is None,
        "runtime_image_execution",
    )
    names: list[str] = []
    for item in array_value(config["Env"]):
        name, separator, _value = string_value(item).partition("=")
        require(separator == "=", "runtime_environment_assignment")
        names.append(name)
    validate_environment_names(names)


def validate_compose(
    config: JsonObject, socket_directory: Path, image_id: str, *, include_migration: bool = True
) -> None:
    """Validate resolved service settings before any application is replaced or started."""
    services = object_value(config["services"])
    require(re.fullmatch(r"sha256:[a-f0-9]{64}", image_id), "runtime_image_selection")
    names = ("simplestchat", "migrate") if include_migration else ("simplestchat",)
    for name in names:
        service = object_value(services[name])
        require(service.get("image") == image_id, "runtime_compose_image")
        require(set(service) <= SERVICE_FIELDS, "runtime_compose_fields")
        require(
            service.get("user") == "10001:10001"
            and service.get("working_dir") in (None, "/app")
            and service.get("command") in (None, SERVER_COMMAND)
            and service.get("entrypoint") in (None, [])
            and service.get("healthcheck") is None,
            "runtime_compose_execution",
        )
        require(
            service.get("read_only") is True
            and service.get("init") is True
            and service.get("cap_drop") == ["ALL"]
            and service.get("security_opt") == ["no-new-privileges:true"],
            "runtime_compose_isolation",
        )
        environment = object_value(service["environment"])
        validate_environment_names(list(environment))
        require(
            all(isinstance(value, str) for value in environment.values())
            and environment.get("RUN_MIGRATIONS") == ("true" if name == "migrate" else "false"),
            "runtime_compose_environment",
        )
        mounts = array_value(service["volumes"])
        require(len(mounts) == 1, "runtime_compose_mounts")
        mount = object_value(mounts[0])
        require(
            set(mount) <= {"type", "source", "target", "read_only", "bind"}
            and mount.get("type") == "bind"
            and mount.get("source") == str(socket_directory)
            and mount.get("target") == "/run/simplestchat-postgres"
            and mount.get("read_only") is True
            and mount.get("bind", {}) in ({}, {"create_host_path": True}),
            "runtime_compose_mounts",
        )
        size = "32m" if name == "migrate" else "64m"
        # This is the container's bounded noexec mount, not a host temporary path.
        require(
            service.get("tmpfs") == [f"/tmp:rw,nosuid,nodev,noexec,size={size},mode=1777"],  # noqa: S108
            "runtime_compose_tmpfs",
        )


def main() -> int:
    """Read bounded JSON on stdin and emit only a fixed, non-sensitive receipt."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("kind", choices=("compose", "image"))
    _ = parser.add_argument("--image-id", help="compose: the exact selected image ID")
    arguments = parser.parse_args(namespace=Arguments())
    try:
        data = sys.stdin.buffer.read(MAX_CONFIGURATION + 1)
        require(0 < len(data) <= MAX_CONFIGURATION, "runtime_configuration_size")
        document = object_value(decode_json(data))
        if arguments.kind == "image":
            require(arguments.image_id is None, "runtime_image_arguments")
            validate_image(document)
        else:
            validate_compose(
                document,
                Path("/srv/simplestchat-public/postgres-socket"),
                string_value(arguments.image_id),
            )
    except (ValueError, KeyError, OSError) as error:
        failure = str(error) if isinstance(error, RuntimeProfileError) else "runtime_configuration"
        _ = sys.stdout.write(json.dumps({"passed": False, "failure": failure}) + "\n")
        return 1
    _ = sys.stdout.write(json.dumps({"passed": True}) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
