"""Offline workflow authority and budget checks for the fixed session soak."""

import unittest
from functools import cached_property
from pathlib import Path

from test_support import at, obj, objects, string, yaml_value

# isort: split
from release_json import JsonObject

ROOT = Path(__file__).resolve().parents[3]
NAME = "Soak twenty minutes and stop PostgreSQL for eight seconds at the midpoint"


class SessionSoakCiTests(unittest.TestCase):
    """Keep the existing workload and runner-created service authority explicit."""

    @cached_property
    def job(self) -> JsonObject:
        """Read the scheduled workload without invoking GitHub or an engine."""
        text = (ROOT / ".github/workflows/performance.yml").read_text()
        return obj(at(yaml_value(text, scalars_as_strings=True), "jobs", "soak"))

    @cached_property
    def step(self) -> JsonObject:
        """Select the one supervised full-profile workload step."""
        return next(step for step in objects(at(self.job, "steps")) if step.get("name") == NAME)

    def test_same_hosted_job_deadlines_and_pinned_database_are_retained(self) -> None:
        """The port changes orchestration without reducing the public workload."""
        self.assertEqual(self.job["runs-on"], "ubuntu-24.04")
        self.assertEqual(self.job["timeout-minutes"], "60")
        self.assertEqual(self.step["timeout-minutes"], "40")
        self.assertEqual(
            string(self.job, "services", "postgres", "image"),
            "docker.io/library/postgres:18.6-bookworm@sha256:"
            + "3725f4e2499eef5134592b3b4ab79a543ed7f8e533b05b5b637af926630f6650",
        )
        self.assertNotIn("continue-on-error", self.step)
        self.assertNotIn("if", self.step)

    def test_explicit_service_context_and_captured_database_target_replace_port_discovery(
        self,
    ) -> None:
        """Only the runner-selected PostgreSQL service can receive stop/start."""
        self.assertEqual(self.step["run"], "cargo run --locked -q -p xtask -- ci session-soak")
        for key, value in {
            "SOAK_POSTGRES_ID": "${{ job.services.postgres.id }}",
            "SOAK_POSTGRES_NETWORK": "${{ job.services.postgres.network }}",
            "SOAK_POSTGRES_PORT": "${{ job.services.postgres.ports['5432'] }}",
            "SESSION_SOAK_E2E": "1",
            "E2E_ARTIFACTS": "${{ runner.temp }}/session-soak",
            "SERVER_REVISION": "${{ github.sha }}",
            "FRONTEND_REVISION": "${{ github.sha }}",
        }.items():
            self.assertEqual(string(self.step, "env", key), value)
        database = (
            "postgres://test_owner:disposable-ci-password@127.0.0.1:5432/"
            "simplestchat_test?sslmode=disable"
        )
        for key in ("DATABASE_URL", "TEST_DATABASE_URL"):
            self.assertEqual(string(self.job, "env", key), database)
        self.assertNotIn("DATABASE_URL", obj(self.step["env"]))
        self.assertNotIn("TEST_DATABASE_URL", obj(self.step["env"]))

    def test_the_existing_migration_build_browser_and_artifact_boundaries_stay_ordered(
        self,
    ) -> None:
        """The supervisor consumes built assets and does not create a second workload."""
        steps = objects(self.job["steps"])
        position = steps.index(self.step)
        for name in (
            "Build server and load generator",
            "Build UI and install pinned browser tooling",
            "Migrate disposable test database with the real server",
        ):
            self.assertLess(
                next(i for i, step in enumerate(steps) if step.get("name") == name), position
            )
        retention = next(step for step in steps if step.get("name") == "Preserve soak diagnostics")
        self.assertGreater(steps.index(retention), position)
        self.assertEqual(string(retention, "with", "path"), "${{ runner.temp }}/session-soak")


if __name__ == "__main__":
    _ = unittest.main()
