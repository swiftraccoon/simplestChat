"""Respect container CPU and memory ceilings during fresh traced compilation."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path
from typing import override

from test_support import ROOT

# isort: split
import security_codeql_resources as resources
from security_tools import ToolError


class CodeqlResourceTests(unittest.TestCase):
    """Model hosted runners, smaller local containers, and kernel quota formats."""

    directory: Path = Path()

    @override
    def setUp(self) -> None:
        """Keep synthetic controller trees private and inside the checkout."""
        temporary = tempfile.TemporaryDirectory(prefix="codeql-resources-", dir=ROOT / "results")
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)

    def test_hosted_and_local_resources_bound_both_compile_and_queries(self) -> None:
        """Four hosted workers fit; a five-GiB container gets two and three-GiB RAM."""
        self.assertEqual(resources.budget(4, 16 * 1024**3), resources.Budget(4, 6144))
        self.assertEqual(resources.budget(3, 5 * 1024**3), resources.Budget(2, 3072))
        self.assertEqual(resources.budget(3, 8 * 1024**3), resources.Budget(3, 4915))
        self.assertEqual(resources.budget(1, 16 * 1024**3).workers, 1)
        with self.assertRaisesRegex(ToolError, "codeql_insufficient_resources"):
            _ = resources.budget(4, 1024**3)

    def test_explicit_worker_limit_cannot_exceed_cpu_memory_or_fixed_cap(self) -> None:
        """A caller may reduce concurrency but cannot bypass the detected budget."""
        self.assertEqual(resources.budget(3, 5 * 1024**3, "1").workers, 1)
        for requested in ("0", "5", "01", "-1", "", "2.5"):
            with (
                self.subTest(requested=requested),
                self.assertRaisesRegex(ToolError, "codeql_build_jobs_range"),
            ):
                _ = resources.budget(4, 16 * 1024**3, requested)
        with self.assertRaisesRegex(ToolError, "codeql_build_jobs_budget"):
            _ = resources.budget(3, 5 * 1024**3, "3")

    def test_v2_quotas_include_ancestors_and_fractional_cpu_budget(self) -> None:
        """Host-visible counts must not override a parent's stricter container limit."""
        root = self.directory / "controllers"
        leaf = root / "group/runner"
        leaf.mkdir(parents=True)
        _ = (root / "cpu.max").write_text("max 100000\n")
        _ = (leaf.parent / "cpu.max").write_text("250000 100000\n")
        _ = (leaf / "cpu.max").write_text("300000 100000\n")
        _ = (root / "memory.max").write_text("max\n")
        _ = (leaf / "memory.max").write_text(str(5 * 1024**3))
        membership = self.directory / "membership"
        _ = membership.write_text("0::/group/runner\n")
        cpus, memory = resources.cgroup_limits(root, membership)
        self.assertEqual(min(cpus), 2)
        self.assertEqual(memory, [5 * 1024**3])

    def test_v1_limits_apply_with_host_relative_membership(self) -> None:
        """Containers may expose their own controller root and a nonresolvable host path."""
        root = self.directory / "controllers"
        cpu = root / "cpu"
        memory = root / "memory"
        cpu.mkdir(parents=True)
        memory.mkdir()
        _ = (cpu / "cpu.cfs_quota_us").write_text("300000")
        _ = (cpu / "cpu.cfs_period_us").write_text("100000")
        _ = (memory / "memory.limit_in_bytes").write_text(str(5 * 1024**3))
        membership = self.directory / "membership"
        _ = membership.write_text("1:cpu,cpuacct:/../../host/runner\n2:memory:/runner\n")
        self.assertEqual(resources.cgroup_limits(root, membership), ([3], [5 * 1024**3]))


if __name__ == "__main__":
    _ = unittest.main()
