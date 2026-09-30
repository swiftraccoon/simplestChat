"""Opt-in real engine check for isolated control HTTP and safe result collection.

Set CAPACITY_CONTAINER_TEST=1 and immutable CAPACITY_SERVER_IMAGE /
CAPACITY_GENERATOR_IMAGE identities. CAPACITY_ENGINE selects docker or podman.
The test creates only random, labeled containers and an anonymous result volume;
it never publishes a host port or starts a media load. It validates the harness,
not capacity or image provenance. No images are built or pulled automatically.
"""

from __future__ import annotations

import json
import os
import re
import secrets
import tempfile
import time
import unittest
from dataclasses import replace
from pathlib import Path

from test_capacity_ownership import context

# isort: split

import capacity
import capacity_artifacts


@unittest.skipUnless(
    os.environ.get("CAPACITY_CONTAINER_TEST") == "1", "explicit container opt-in required"
)
class CapacityContainerTests(unittest.TestCase):
    """Exercise the real engine boundary without media traffic or public listeners."""

    def test_isolated_health_results_and_owned_volume_cleanup(self) -> None:
        """Readonly nonroot containers communicate over loopback and return regular results."""
        server_image = os.environ.get("CAPACITY_SERVER_IMAGE", "")
        generator_image = os.environ.get("CAPACITY_GENERATOR_IMAGE", "")
        for image in (server_image, generator_image):
            self.assertRegex(image, r"^sha256:[a-f0-9]{64}$")
        engine = capacity.find_engine(os.environ.get("CAPACITY_ENGINE", "docker"))
        run = secrets.token_hex(8)
        server, generator = f"capacity-fixture-server-{run}", f"capacity-fixture-gen-{run}"
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            fixture = replace(
                context(directory),
                engine=engine,
                run_id=run,
                server_image=server_image,
                generator_image=generator_image,
            )
            try:
                _ = engine.run(*capacity.server_command(fixture, server, "a" * 48))
                capacity.wait_ready(engine, server, time.monotonic() + 30)
                self.assertEqual(
                    engine.run(
                        "inspect", "--format", "{{.HostConfig.NetworkMode}}", server
                    ).strip(),
                    "none",
                )
                self.assertEqual(
                    engine.run(
                        "inspect", "--format", "{{.HostConfig.ReadonlyRootfs}}", server
                    ).strip(),
                    "true",
                )
                self.assertNotIn("00000000\t", engine.read(server, "/proc/net/route"))
                _ = engine.http(server, "/metrics", "a" * 48)
                script = (
                    "printf '{}' >/results/load_test_summary.json; "
                    + "printf '[]' >/results/load_test_results.json"
                )
                _ = engine.run(
                    "run",
                    "--detach",
                    "--name",
                    generator,
                    *capacity.sandbox(engine),
                    "--network",
                    f"container:{server}",
                    "--memory",
                    "128m",
                    "--memory-swap",
                    "128m",
                    "--pids-limit",
                    "64",
                    "--volume",
                    "/results",
                    "--entrypoint",
                    "/bin/sh",
                    generator_image,
                    "-c",
                    script,
                )
                self.assertEqual(engine.run("wait", generator, timeout=30).strip(), "0")
                volume = engine.run(
                    "inspect",
                    "--format",
                    '{{range .Mounts}}{{if eq .Destination "/results"}}{{.Name}}{{end}}{{end}}',
                    generator,
                ).strip()
                self.assertIsNotNone(re.fullmatch(r"[a-f0-9]{64}", volume))
                capacity.collect(engine, (server, generator), directory)
                self.assertEqual(
                    json.loads(
                        capacity_artifacts.read_regular(
                            directory / "generator/load_test_results.json"
                        )
                    ),
                    [],
                )
                self.assertTrue((directory / "collection.json").is_file())
            finally:
                try:
                    engine.remove(generator)
                finally:
                    engine.remove(server)
            self.assertEqual(
                engine.run("ps", "--all", "--quiet", "--filter", f"name={server}").strip(), ""
            )
            self.assertEqual(
                engine.run("volume", "ls", "--quiet", "--filter", f"name={volume}").strip(), ""
            )


if __name__ == "__main__":
    _ = unittest.main()
