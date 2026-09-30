"""Capacity container ownership survives interrupted create responses; no engine runs."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from test_support import ROOT

# isort: split
import capacity

RUN = "a" * 16
SERVER = "sha256:" + "b" * 64
GENERATOR = "sha256:" + "c" * 64
SERVER_ID = "d" * 64
GENERATOR_ID = "e" * 64


def context(directory: Path) -> capacity.Context:
    """Return a realistic immutable execution context without any engine process."""
    return capacity.Context(
        engine=capacity.Engine("/nonexistent/docker"),
        run_id=RUN,
        server_image=SERVER,
        generator_image=GENERATOR,
        revisions=("r" * 16, "r" * 16),
        shape=capacity.Shape(1, 1, 4.8),
        worker_threshold=0.7,
        server_memory="2048m",
        generator_memory="4096m",
        browser=capacity.Browser(audio_only=True, speakers=30, chat_interval_ms=30000),
        output=directory,
        log=lambda _message: None,
    )


def entries(directory: Path) -> list[dict[str, object]]:
    """Read real atomic journal bytes through the production decoder."""
    value = capacity.read_json(directory / "ownership.json")
    return [
        capacity.as_object(item, "container")
        for item in capacity.as_list(value["containers"], "containers")
    ]


class OwnershipTests(unittest.TestCase):
    """Exercise filesystem and engine-boundary ordering, including interrupted creates."""

    def test_updates_preserve_other_identities_and_private_atomic_file(self) -> None:
        """An ID arrival replaces only its expected name and leaves no temporary file."""
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            capacity.remember_container(directory, RUN, "server", SERVER)
            capacity.remember_container(directory, RUN, "generator", GENERATOR)
            capacity.remember_container(directory, RUN, "server", SERVER, SERVER_ID)
            self.assertEqual(
                entries(directory),
                [
                    {"name": "generator", "image": GENERATOR, "id": None},
                    {"name": "server", "image": SERVER, "id": SERVER_ID},
                ],
            )
            self.assertEqual((directory / "ownership.json").stat().st_mode & 0o777, 0o600)
            self.assertFalse((directory / "ownership.tmp").exists())

    def test_different_run_cannot_reassign_a_retained_journal(self) -> None:
        """A reused output location fails before overwriting its ownership evidence."""
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            capacity.remember_container(directory, RUN, "server", SERVER, SERVER_ID)
            before = (directory / "ownership.json").read_bytes()
            with self.assertRaisesRegex(capacity.CapacityError, "another run"):
                capacity.remember_container(directory, "f" * 16, "foreign", GENERATOR)
            self.assertEqual((directory / "ownership.json").read_bytes(), before)

    def test_probe_identity_precedes_even_an_interrupted_engine_response(self) -> None:
        """The otherwise ephemeral host-facts probe has an exact recovery identity."""
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)

            def run(*args: str, **_kwargs: object) -> str:
                self.assertIn(f"simplestchat.capacity.run={RUN}", args)
                self.assertIn(f"capacity-probe-{RUN}", args)
                self.assertEqual(
                    entries(directory),
                    [{"name": f"capacity-probe-{RUN}", "image": SERVER, "id": None}],
                )
                message = "Interrupted probe response"
                raise capacity.CapacityError(message)

            with (
                patch.object(capacity.Engine, "run", side_effect=run),
                self.assertRaisesRegex(capacity.CapacityError, "Interrupted probe"),
            ):
                _ = capacity.host_facts(
                    capacity.Engine("unused"), SERVER, ownership=(directory, RUN)
                )
            self.assertEqual(len(entries(directory)), 1)

    def run_step(self, directory: Path, *, interrupt_generator: bool) -> list[list[str]]:
        """Drive real create/finally ordering while all media/engine work stays mocked."""
        created: list[list[str]] = []

        def run(*args: str, **_kwargs: object) -> str:
            created.append(list(args))
            self.assertEqual(args[0], "run")
            self.assertIn(f"simplestchat.capacity.run={RUN}", args)
            name = args[args.index("--name") + 1]
            prior = next(item for item in entries(directory) if item["name"] == name)
            self.assertIsNone(prior["id"], "expected ownership must be durable before docker run")
            if name.startswith("capacity-gen-"):
                if interrupt_generator:
                    message = "Lost generator create response"
                    raise capacity.CapacityError(message)
                return GENERATOR_ID
            return SERVER_ID

        with (
            patch.object(capacity, "wait_ready"),
            patch.object(capacity.Engine, "run", side_effect=run),
            patch.object(capacity.Engine, "read", return_value="1024"),
            patch.object(capacity, "parse_udp", return_value=capacity.UdpCounters(0, 0, 0)),
            patch.object(capacity.Engine, "running", return_value=True),
            patch.object(capacity.Engine, "oom_killed", return_value=False),
            patch.object(capacity.Engine, "remove") as remove,
            patch.object(capacity, "monitor", return_value=(None, None, None)),
            patch.object(capacity, "scrape", return_value={}),
            patch.object(capacity, "collect"),
        ):
            _ = capacity.run_step(
                context(directory), capacity.StepPlan("meetings", 30, 30, 30, 60), 1
            )
        self.assertEqual(
            [call.args for call in remove.call_args_list],
            [(f"capacity-gen-{RUN}-1",), (f"capacity-server-{RUN}-1",)],
        )
        return created

    def test_both_created_containers_retain_exact_ids_and_shared_run_label(self) -> None:
        """Create responses fill the journal and normal cleanup retains its evidence."""
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            created = self.run_step(directory, interrupt_generator=False)
            self.assertEqual(len(created), 2)
            self.assertEqual(
                entries(directory),
                [
                    {"name": f"capacity-server-{RUN}-1", "image": SERVER, "id": SERVER_ID},
                    {"name": f"capacity-gen-{RUN}-1", "image": GENERATOR, "id": GENERATOR_ID},
                ],
            )

    def test_missing_generator_response_retains_expected_identity_and_prior_server_id(self) -> None:
        """A lost response keeps the label/name/image proof needed for exact-ID inspection."""
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            _ = self.run_step(directory, interrupt_generator=True)
            self.assertEqual(
                entries(directory),
                [
                    {"name": f"capacity-server-{RUN}-1", "image": SERVER, "id": SERVER_ID},
                    {"name": f"capacity-gen-{RUN}-1", "image": GENERATOR, "id": None},
                ],
            )

    def test_controller_entrypoint_is_executable(self) -> None:
        """The documented maintained CLI runs directly from an ordinary checkout."""
        self.assertTrue((ROOT / "build/run-vps-capacity.py").stat().st_mode & 0o111)


if __name__ == "__main__":
    _ = unittest.main()
