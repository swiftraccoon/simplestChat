"""Check the actual one-off bridge's sealed-source and TLS ownership boundaries."""

import unittest
from pathlib import Path
from typing import TYPE_CHECKING, cast
from unittest.mock import patch

from jinja2 import Environment, StrictUndefined
from test_support import obj, objects, string

# isort: split
import migrate_public as migration
import release_public as public
from release_json import JsonObject
from test_support import yaml_value

if TYPE_CHECKING:
    from collections.abc import Callable

ROOT = Path(__file__).resolve().parents[1]
PLAY = obj(yaml_value((ROOT / "dns-handoff.yml").read_text()), 0)
SCRIPT = string(objects(PLAY, "tasks")[0], "ansible.builtin.command", "argv", 3)


class DnsHandoffTests(unittest.TestCase):
    """Execute maintained source guards while all runtime observations are mocked."""

    def test_seal_identity_and_stopped_writer_guards_fail_closed(self) -> None:
        """Wrong host, stale seal or any running/restartable source writer refuses handoff."""
        namespace: dict[str, object] = {"__name__": "handoff_test"}
        source = (
            Environment(undefined=StrictUndefined, autoescape=True).from_string(SCRIPT).render()
        )
        exec(compile(source, "dns-handoff.yml", "exec"), namespace)  # noqa: S102 -- Execute the maintained fixed source with no main invocation.
        guard = cast("Callable[[object, object, str, str], JsonObject]", namespace["verify_source"])
        request = migration.Request(
            operation_id="a" * 32,
            source_origin="https://chat.example.test",
            destination_origin="https://chat.example.test",
            target_revision="b" * 40,
        )
        prior: JsonObject = {
            "role": "source",
            "phase": "cutover-sealed",
            "before": {
                "machineId": "c" * 32,
                "containers": {
                    name: {"id": name} for name in ("simplestchat", "caddy", "postgres")
                },
            },
        }
        journal: JsonObject = {
            "operation": "server_migration",
            "request": request.json(),
            "phase": "cutover-sealed",
        }
        images: JsonObject = {"caddyImage": "exact-immutable-image"}
        for change in (
            "valid",
            "local-phase",
            "global-phase",
            "request",
            "machine",
            "running",
            "restart",
            "container",
        ):
            with self.subTest(change=change):
                local = prior | ({"phase": "frozen"} if change == "local-phase" else {})
                global_state = journal | (
                    {"phase": "frozen"}
                    if change == "global-phase"
                    else {"request": {}}
                    if change == "request"
                    else {}
                )

                def container(_runner: object, service: str, change: str = change) -> JsonObject:
                    return {
                        "id": "other" if change == "container" else service,
                        "running": change == "running",
                        "restartPolicy": "always" if change == "restart" else "no",
                    }

                with (
                    patch.object(migration, "state", return_value=local),
                    patch.object(migration, "read_object", return_value=global_state),
                    patch.object(
                        migration,
                        "machine_id",
                        return_value="other" if change == "machine" else "c" * 32,
                    ),
                    patch.object(
                        migration,
                        "configuration",
                        return_value=(
                            images,
                            {
                                "ALLOWED_ORIGINS": request.source_origin,
                                "ANNOUNCE_IP": "8.8.8.8",
                            },
                        ),
                    ),
                    patch.object(migration, "owned_container", side_effect=container),
                ):
                    if change == "valid":
                        self.assertEqual(
                            guard(request, object(), "8.8.8.8", "chat.example.test"), images
                        )
                    else:
                        with self.assertRaises(public.ReleaseError):
                            _ = guard(request, object(), "8.8.8.8", "chat.example.test")

    def test_exact_timer_precedes_start_and_never_targets_source_services(self) -> None:
        """Only the newly created immutable container ID can receive the expiry stop."""
        self.assertLess(
            SCRIPT.index("--on-active=7200"), SCRIPT.index("runner.docker('start', container")
        )
        self.assertIn("'stop', '--time', '10', container", SCRIPT)
        self.assertIn("with migration.workload(request):", SCRIPT)
        self.assertIn("'--restart=no'", SCRIPT)
        self.assertIn("'--read-only'", SCRIPT)
        self.assertIn("'10001:10001'", SCRIPT)
        self.assertIn("tls.validate(attempt, domain, files)", SCRIPT)
        self.assertNotIn("runner.compose", SCRIPT)
        self.assertNotIn("tls_insecure_skip_verify", SCRIPT)
        self.assertIn("tls_server_name ", SCRIPT)
        self.assertIn("header_up Host ", SCRIPT)
        self.assertIn("'https://' + domain + '/health'", SCRIPT)
        self.assertIn("'readonly'", SCRIPT.replace("',readonly'", "'readonly'"))


if __name__ == "__main__":
    _ = unittest.main()
