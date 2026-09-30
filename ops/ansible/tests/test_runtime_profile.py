"""Offline execution-profile regressions; no application or container is started."""

from __future__ import annotations

import json
import subprocess
import sys
import unittest
from pathlib import Path

from test_support import ROOT, obj

# isort: split
import runtime_profile as profile
from release_json import JsonObject, JsonValue, array_value

# Paths are inert container-model strings; no such filesystem resources are created.
# ruff: noqa: S108

SOCKET = Path("/srv/simplestchat-public/postgres-socket")
IMAGE_ID = "sha256:" + "a" * 64


def service(*, migration: bool = False) -> JsonObject:
    """Model only the current resolved managed service contract."""
    return {
        "image": IMAGE_ID,
        "user": "10001:10001",
        "command": None,
        "entrypoint": None,
        "environment": {"RUN_MIGRATIONS": "true" if migration else "false"},
        "read_only": True,
        "init": True,
        "cap_drop": ["ALL"],
        "security_opt": ["no-new-privileges:true"],
        "tmpfs": [
            "/tmp:rw,nosuid,nodev,noexec,size=" + ("32m" if migration else "64m") + ",mode=1777"
        ],
        "volumes": [
            {
                "type": "bind",
                "source": str(SOCKET),
                "target": "/run/simplestchat-postgres",
                "read_only": True,
                "bind": {"create_host_path": True},
            }
        ],
    }


def compose() -> JsonObject:
    """Include both services using the same application executable."""
    return {"services": {"simplestchat": service(), "migrate": service(migration=True)}}


def image() -> JsonObject:
    """Use the image defaults actually required by the managed runtime."""
    return {
        "User": "10001:10001",
        "WorkingDir": "/app",
        "Cmd": ["/app/simplestChat"],
        "Entrypoint": None,
        "Env": ["PATH=/usr/bin", "RUST_LOG=simplestChat=info"],
    }


class RuntimeProfileTests(unittest.TestCase):
    """Reject changes to executable selection, lookup paths or immutable filesystem."""

    def test_current_image_and_compose_defaults_pass(self) -> None:
        """App and migration use the audited binary; ordinary application settings remain valid."""
        profile.validate_image(image())
        value = compose()
        obj(value, "services", "simplestchat", "environment").update(
            {"JWT_SECRET": "inert-test-value", "MEDIA_WORKERS": "2", "LOCALDOMAIN": "example.test"}
        )
        profile.validate_compose(value, SOCKET, IMAGE_ID)
        del obj(value, "services")["migrate"]
        profile.validate_compose(value, SOCKET, IMAGE_ID, include_migration=False)

    def test_loader_and_crypto_names_are_rejected_even_when_empty(self) -> None:
        """A value cannot weaken the override policy, including an explicitly empty value."""
        for key in (
            "LD_PRELOAD",
            "LD_LIBRARY_PATH",
            "LD_AUDIT",
            "LD_ORIGIN_PATH",
            "LD_FAKE_FUTURE",
            "OPENSSL_CONF",
            "OPENSSL_MODULES",
            "OPENSSL_ENGINES",
            "GLIBC_TUNABLES",
            "GCONV_PATH",
            "LOCPATH",
        ):
            with self.subTest(key=key):
                value = image()
                array_value(value["Env"]).append(key + "=")
                with self.assertRaises(profile.RuntimeProfileError):
                    profile.validate_image(value)
                for name in ("simplestchat", "migrate"):
                    value = compose()
                    obj(value, "services", name, "environment")[key] = ""
                    with self.assertRaises(profile.RuntimeProfileError):
                        profile.validate_compose(value, SOCKET, IMAGE_ID)

    def test_environment_is_complete_and_unambiguous(self) -> None:
        """Duplicate, missing assignment and unresolved Compose variables fail closed."""
        for entries in (["PATH=a", "PATH=b"], ["PATH"], ["bad name=x"]):
            value = image()
            value["Env"] = list[JsonValue](entries)
            with self.assertRaises(profile.RuntimeProfileError):
                profile.validate_image(value)
        value = compose()
        obj(value, "services", "simplestchat", "environment")["PATH"] = None
        with self.assertRaises(profile.RuntimeProfileError):
            profile.validate_compose(value, SOCKET, IMAGE_ID)

    def test_arbitrary_executable_healthcheck_and_lifecycle_hooks_are_refused(self) -> None:
        """The managed contract does not authorize alternate programs, even as non-root."""
        changes: list[tuple[str, JsonValue]] = [
            ("command", ["/bin/true"]),
            ("entrypoint", ["/bin/true"]),
            ("healthcheck", {"test": ["CMD", "/bin/true"]}),
            ("post_start", [{"command": "/bin/true"}]),
            ("pre_stop", [{"command": "/bin/true"}]),
            ("user", "0:0"),
            ("working_dir", "/tmp"),
        ]
        for key, value in changes:
            for name in ("simplestchat", "migrate"):
                with self.subTest(key=key, service=name):
                    document = compose()
                    obj(document, "services", name)[key] = value
                    with self.assertRaises(profile.RuntimeProfileError):
                        profile.validate_compose(document, SOCKET, IMAGE_ID)
        image_changes: list[tuple[str, JsonValue]] = [
            ("Cmd", ["/bin/true"]),
            ("Entrypoint", ["/bin/true"]),
            ("Healthcheck", {"Test": ["CMD", "/bin/true"]}),
            ("WorkingDir", "/tmp"),
        ]
        for key, value in image_changes:
            document = image()
            document[key] = value
            with self.assertRaises(profile.RuntimeProfileError):
                profile.validate_image(document)

    def test_mount_and_isolation_changes_cannot_substitute_runtime_bytes(self) -> None:
        """Only the read-only database socket and bounded noexec /tmp are mounted."""
        changes: list[tuple[str, JsonValue]] = [
            ("read_only", False),
            ("init", False),
            ("cap_drop", []),
            ("cap_add", ["SYS_ADMIN"]),
            ("security_opt", []),
            ("privileged", True),
            ("tmpfs", ["/usr:rw"]),
            ("configs", [{"source": "foreign", "target": "/etc"}]),
        ]
        for key, value in changes:
            document = compose()
            obj(document, "services", "simplestchat")[key] = value
            with self.subTest(key=key), self.assertRaises(profile.RuntimeProfileError):
                profile.validate_compose(document, SOCKET, IMAGE_ID)
        mount_changes: list[tuple[str, JsonValue]] = [
            ("target", "/etc"),
            ("source", "/tmp/foreign"),
            ("read_only", False),
            ("bind", {"propagation": "rshared"}),
        ]
        for key, value in mount_changes:
            document = compose()
            obj(document, "services", "simplestchat", "volumes", 0)[key] = value
            with self.subTest(key=key), self.assertRaises(profile.RuntimeProfileError):
                profile.validate_compose(document, SOCKET, IMAGE_ID)

    def test_both_services_are_bound_to_the_selected_immutable_image(self) -> None:
        """Identical execution settings cannot authorize a different or mutable image."""
        for selected in ("server:latest", "sha256:" + "b" * 64):
            with self.assertRaises(profile.RuntimeProfileError):
                profile.validate_compose(compose(), SOCKET, selected)
        for name in ("simplestchat", "migrate"):
            value = compose()
            obj(value, "services", name)["image"] = "sha256:" + "b" * 64
            with self.assertRaises(profile.RuntimeProfileError):
                profile.validate_compose(value, SOCKET, IMAGE_ID)

    def test_cli_bounds_and_redacts_private_configuration(self) -> None:
        """The actual parser rejects malformed/oversized input with only fixed diagnostics."""
        source = ROOT / "ops/ansible/files/runtime_profile.py"
        value = image()
        private_marker = "inert-private-value-must-not-appear"
        array_value(value["Env"]).append("OPENSSL_CONF=" + private_marker)
        for payload in (
            json.dumps(value).encode(),
            b"{broken",
            b" " * (profile.MAX_CONFIGURATION + 1),
        ):
            result = subprocess.run(  # noqa: S603 - fixed local validator, inert JSON on stdin.
                [sys.executable, str(source), "image"],
                input=payload,
                capture_output=True,
                timeout=10,
                check=False,
            )
            self.assertEqual(result.returncode, 1)
            self.assertNotIn(private_marker.encode(), result.stdout + result.stderr)
            self.assertLess(len(result.stdout + result.stderr), 256)

    def test_compose_cli_requires_the_selected_image_without_echoing_configuration(self) -> None:
        """Exercise the launcher's actual pipe contract using the harmless service model."""
        source = ROOT / "ops/ansible/files/runtime_profile.py"
        payload = json.dumps(compose()).encode()
        for options, expected in (([], 1), (["--image-id", IMAGE_ID], 0)):
            result = subprocess.run(  # noqa: S603 - fixed local validator; no Docker or application.
                [sys.executable, str(source), "compose", *options],
                input=payload,
                capture_output=True,
                timeout=10,
                check=False,
            )
            self.assertEqual(result.returncode, expected)
            self.assertNotIn(b"RUN_MIGRATIONS", result.stdout + result.stderr)
            self.assertEqual(json.loads(result.stdout)["passed"], expected == 0)

    def test_current_nss_asset_and_docker_order_are_explicit(self) -> None:
        """The final runtime never regenerates a cache after the reviewed package installation."""
        asset = ROOT / "security/runtime/nsswitch.conf"
        tables = dict(
            line.split(": ", 1)
            for line in asset.read_text().splitlines()
            if line and not line.startswith("#")
        )
        self.assertEqual(tables["hosts"], "files dns")
        self.assertTrue(all(value == "files" for name, value in tables.items() if name != "hosts"))
        self.assertTrue({"passwd", "group", "shadow", "gshadow", "initgroups"} <= set(tables))
        docker = (ROOT / "Dockerfile").read_text().split(" AS runtime-base\n", 1)[1]
        self.assertLess(docker.index("dnf install"), docker.index("rm -f /etc/ld.so.cache"))
        self.assertLess(
            docker.index("rm -f /etc/ld.so.cache"),
            docker.index("COPY security/runtime/nsswitch.conf"),
        )
        self.assertNotIn("ldconfig", docker)


if __name__ == "__main__":
    _ = unittest.main()
