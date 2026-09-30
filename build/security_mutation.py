#!/usr/bin/env python3
"""Measure selected pure-policy tests against bounded mutations of a private full-crate copy."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import sys
import time
import tomllib
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING

from security_context import Context, executable
from security_tools import (
    ToolError,
    bounded_file,
    current_platform,
    require,
    tool_path,
    write_private,
)

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "ops/ansible/files"))

# isort: split
import bounded_process
from release_json import (
    JsonObject,
    JsonValue,
    array_value,
    decode_json,
    integer_value,
    object_value,
    string_value,
)

if TYPE_CHECKING:
    from collections.abc import Sequence

ROOT = Path(__file__).resolve().parents[1]
POLICY = ROOT / "security/mutation/policy.json"
MAX_REPORT = 16 * 1024**2
MAX_EVIDENCE = 512 * 1024**2
MAX_WORKSPACE = 20 * 1024**3
MAX_WORKSPACE_ENTRIES = 200000
DISK_RESERVE = 1024**3
MAX_POLICY_FILES, MAX_TEST_FILTERS = 8, 16


def read(path: Path) -> JsonObject:
    """Accept one bounded metadata object, never an artifact-provided success marker."""
    return object_value(decode_json(bounded_file(path, MAX_REPORT)))


def write(path: Path, value: JsonValue) -> None:
    """Keep compact result evidence private and exclusive."""
    write_private(path, (json.dumps(value, indent=2, sort_keys=True) + "\n").encode(), 0o600)


def digest(path: Path) -> str:
    """Hash an already bounded policy, source manifest or tool receipt."""
    return hashlib.sha256(bounded_file(path, MAX_REPORT)).hexdigest()


@dataclass(frozen=True)
class Policy:
    """Reviewed finite function inventory, tests and independent runtime ceilings."""

    version: str
    maximum: int
    minimum_tests: int
    build_seconds: int
    test_seconds: int
    total_seconds: int
    files: dict[str, tuple[str, ...]]
    filters: tuple[str, ...]


def policy_at(path: Path) -> Policy:
    """Reject malformed or open-ended selection before invoking Cargo."""
    value = read(path)
    require(
        set(value)
        == {
            "schemaVersion",
            "toolVersion",
            "maximumMutants",
            "minimumTests",
            "buildTimeoutSeconds",
            "testTimeoutSeconds",
            "totalTimeoutSeconds",
            "sourceFiles",
            "testFilters",
        },
        "mutation_policy_fields",
    )
    require(
        value["schemaVersion"] == 1 and value["toolVersion"] == "27.1.0", "mutation_policy_version"
    )
    bounds = (
        ("maximumMutants", 1, 128),
        ("minimumTests", 1, 200),
        ("buildTimeoutSeconds", 1, 1800),
        ("testTimeoutSeconds", 1, 120),
        ("totalTimeoutSeconds", 1, 7200),
    )
    numbers: dict[str, int] = {}
    for name, minimum, maximum in bounds:
        number = integer_value(value[name])
        require(minimum <= number <= maximum, "mutation_policy_bound")
        numbers[name] = number
    files: dict[str, tuple[str, ...]] = {}
    for name, functions in object_value(value["sourceFiles"]).items():
        require(re.fullmatch(r"src/(?:[a-z_]+/)*[a-z_]+\.rs", name), "mutation_source_path")
        names = tuple(string_value(item) for item in array_value(functions))
        require(
            bool(names)
            and len(names) == len(set(names))
            and all(
                re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*", item)
                for item in names
            ),
            "mutation_function_selection",
        )
        files[name] = names
    filters = tuple(string_value(item) for item in array_value(value["testFilters"]))
    require(
        0 < len(files) <= MAX_POLICY_FILES
        and 0 < len(filters) <= MAX_TEST_FILTERS
        and all(re.fullmatch(r"[A-Za-z_][A-Za-z0-9_:]*", item) for item in filters),
        "mutation_test_selection",
    )
    return Policy(
        string_value(value["toolVersion"]),
        numbers["maximumMutants"],
        numbers["minimumTests"],
        numbers["buildTimeoutSeconds"],
        numbers["testTimeoutSeconds"],
        numbers["totalTimeoutSeconds"],
        files,
        filters,
    )


def selection_arguments(policy: Policy) -> list[str]:
    """Select every mutation of the exact reviewed functions; never sample a subset."""
    names = [name for functions in policy.files.values() for name in functions]
    expression = (
        r"(?:replace |in )(?:" + "|".join(re.escape(name) for name in names) + r")(?: ->|$)"
    )
    return [item for file in policy.files for item in ("--file", file)] + ["--re", expression]


def mutant_inventory(values: JsonValue, policy: Policy) -> dict[str, JsonObject]:
    """Require a nonempty bounded inventory and at least one mutant for every selected function."""
    result: dict[str, JsonObject] = {}
    observed: set[tuple[str, str]] = set()
    for raw in array_value(values):
        item = object_value(raw)
        name, file = string_value(item["name"]), string_value(item["file"])
        function = string_value(object_value(item["function"])["function_name"])
        require(
            file in policy.files and function in policy.files[file], "mutation_unselected_function"
        )
        require(name not in result, "mutation_duplicate")
        result[name] = {key: value for key, value in item.items() if key != "diff"}
        observed.add((file, function))
    expected = {
        (file, function) for file, functions in policy.files.items() for function in functions
    }
    require(
        0 < len(result) <= policy.maximum and observed == expected, "mutation_inventory_coverage"
    )
    return result


def cargo_environment(context: Context, source: Path, openssl: Path) -> None:
    """Use the pinned compiler, private target and canonical static-native prerequisites."""
    toolchain = object_value(
        decode_json(json.dumps(tomllib.loads((source / "rust-toolchain.toml").read_text())))
    )
    channel = string_value(object_value(toolchain["toolchain"])["channel"])
    require(re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", channel), "mutation_rust_toolchain")
    _, found = context.run(
        "rust-toolchain", [executable("rustup"), "which", "--toolchain", channel, "cargo"]
    )
    cargo = Path(found.decode().strip()).resolve(strict=True)
    prefix = openssl.resolve(strict=True)
    require(
        all((prefix / "lib" / name).is_file() for name in ("libssl.a", "libcrypto.a")),
        "mutation_static_openssl_missing",
    )
    context.env.update(
        {
            "PATH": str(cargo.parent) + os.pathsep + context.env["PATH"],
            "RUSTC": str(cargo.with_name("rustc")),
            "RUSTDOC": str(cargo.with_name("rustdoc")),
            "CARGO_NET_OFFLINE": "true",
            "CARGO_BUILD_JOBS": "2",
            "CARGO_PROFILE_TEST_DEBUG": "0",
            "MEDIASOUP_BUILD_JOBS": "2",
            "CARGO_TARGET_DIR": str(source / "target"),
            "CARGO_TERM_COLOR": "never",
            "OPENSSL_DIR": str(prefix),
            "OPENSSL_STATIC": "1",
            "PKG_CONFIG_PATH": str(prefix / "lib/pkgconfig"),
            "PIP_CONSTRAINT": str(source / "build/pip-constraints.txt"),
        }
    )


def command(tool: Path, source: Path, output: Path, policy: Policy, *, listing: bool) -> list[str]:
    """In-place mutation is confined to our private snapshot; it implies one mutation worker."""
    argv = [
        str(tool),
        "mutants",
        "--dir",
        str(source),
        "--no-config",
        "--no-shuffle",
        "--colors=never",
        "--annotations=none",
        "-C=--locked",
        "-C=--offline",
        "-C=--lib",
        *selection_arguments(policy),
    ]
    if listing:
        return [*argv, "--list", "--json"]
    return [
        *argv,
        "--in-place",
        "--baseline=run",
        "--cap-lints=true",
        "--jobserver-tasks=2",
        f"--build-timeout={policy.build_seconds}",
        f"--timeout={policy.test_seconds}",
        "--output",
        str(output),
        "--",
        "--",
        *policy.filters,
        "--test-threads=1",
    ]


@dataclass
class DiskBudget:
    """Actively bound tool-written logs and private compiler outputs without rereading bytes."""

    source: Path
    output: Path
    checked: float = 0
    valid: bool = True
    failure: str | None = None
    observed: dict[str, JsonValue] = field(default_factory=dict)

    def healthy(self, *, force: bool = False) -> bool:
        """Throttle running checks; completion must force a fresh filesystem inventory."""
        if not self.valid:
            return False
        now = time.monotonic()
        if not force and now - self.checked < 1:
            return self.valid
        self.checked = now
        for root, label, limit in (
            (self.source, "workspace", MAX_WORKSPACE),
            (self.output, "evidence", MAX_EVIDENCE),
        ):
            total = count = 0
            for directory, directories, names in os.walk(root, followlinks=False):
                count += len(directories) + len(names)
                self.observed[label + "Entries"] = count
                if count > MAX_WORKSPACE_ENTRIES:
                    self.valid = False
                    self.failure = label + "_entry_limit"
                    return False
                for name in names:
                    try:
                        size = (Path(directory) / name).lstat().st_size
                    except FileNotFoundError:
                        continue  # Compiler temporary files can disappear between walk and stat.
                    total += size
                    self.observed[label + "Bytes"] = total
                    if total > limit:
                        self.valid = False
                        self.failure = label + "_byte_limit"
                        return False
        available = shutil.disk_usage(self.source).free
        self.observed["availableBytes"] = available
        self.valid = available >= DISK_RESERVE
        if not self.valid:
            self.failure = "free_disk_reserve"
        return self.valid


def execute_mutants(context: Context, argv: Sequence[str], source: Path, timeout: int) -> int:
    """Bound stdout, stderr, tool-written files and the overall process-group lifetime."""
    budget = DiskBudget(source, context.output / "results")
    try:
        with (
            (context.output / "mutation.stdout").open("xb") as output,
            (context.output / "mutation.stderr").open("xb") as error,
        ):
            status, _, _ = bounded_process.run(
                argv,
                cwd=source,
                env=context.env,
                limits=bounded_process.Limits(
                    timeout=timeout, stdout=MAX_REPORT, stderr=MAX_REPORT, healthy=budget.healthy
                ),
                output=output,
                error=error,
            )
    finally:
        _ = budget.healthy(force=True)
        write(
            context.output / "budget.json",
            {"passed": budget.valid, "failure": budget.failure, **budget.observed},
        )
    require(budget.valid, "mutation_disk_budget")
    return status


def test_counts(row: JsonObject, directory: Path) -> tuple[int, int]:
    """Distinguish Rust assertion failures from zero tests, compiler errors, crashes or OOM."""
    name = string_value(row["log_path"])
    require(re.fullmatch(r"log/[A-Za-z0-9_.-]+\.log", name), "mutation_log_path")
    text = bounded_file(directory / name, MAX_REPORT).decode("utf-8", errors="replace")
    matches = list(
        re.finditer(
            r"test result: (?:ok|FAILED)\. ([0-9]+) passed; ([0-9]+) failed; "
            + r"0 ignored; 0 measured; [0-9]+ filtered out",
            text,
        )
    )
    require(len(matches) == 1, "mutation_test_result_missing")
    return int(matches[0].group(1)), int(matches[0].group(2))


def assess(directory: Path, expected: dict[str, JsonObject], policy: Policy) -> JsonObject:
    """Require one successful baseline and every enumerated viable mutant killed by tests."""
    value = read(directory / "outcomes.json")
    require(
        value.get("cargo_mutants_version") == policy.version
        and isinstance(value.get("end_time"), str),
        "mutation_result_incomplete",
    )
    rows = [object_value(row) for row in array_value(value["outcomes"])]
    baseline = [row for row in rows if row["scenario"] == "Baseline"]
    require(len(baseline) == 1 and baseline[0]["summary"] == "Success", "mutation_baseline_failed")
    passed, failed = test_counts(baseline[0], directory)
    require(passed >= policy.minimum_tests and failed == 0, "mutation_baseline_test_count")
    observed: dict[str, str] = {}
    for row in rows:
        if row["scenario"] == "Baseline":
            continue
        mutant = object_value(object_value(row["scenario"])["Mutant"])
        name = string_value(mutant["name"])
        require(
            name in expected and name not in observed and mutant == expected[name],
            "mutation_result_identity",
        )
        summary = string_value(row["summary"])
        require(
            summary in ("CaughtMutant", "MissedMutant", "Unviable", "Timeout", "Failure"),
            "mutation_result_summary",
        )
        observed[name] = summary
        if summary == "CaughtMutant":
            phases = [object_value(item) for item in array_value(row["phase_results"])]
            require(
                [item["phase"] for item in phases] == ["Build", "Test"]
                and phases[0]["process_status"] == "Success"
                and phases[1]["process_status"] == {"Failure": 101},
                "mutation_caught_process",
            )
            ok, failures = test_counts(row, directory)
            require(
                failures > 0 and ok + failures >= policy.minimum_tests, "mutation_caught_assertion"
            )
    require(
        set(observed) == set(expected) and value["total_mutants"] == len(expected),
        "mutation_result_coverage",
    )
    counts: JsonObject = {
        name: sum(result == kind for result in observed.values())
        for name, kind in (
            ("caught", "CaughtMutant"),
            ("missed", "MissedMutant"),
            ("unviable", "Unviable"),
            ("timeout", "Timeout"),
            ("failure", "Failure"),
        )
    }
    require(
        all(value[key] == counts[key] for key in ("caught", "missed", "unviable", "timeout"))
        and value["success"] == 0,
        "mutation_result_totals",
    )
    return {
        "passed": counts["caught"] == len(expected),
        "baselineTests": passed,
        "total": len(expected),
        "counts": counts,
        "mutants": dict[str, JsonValue](observed),
    }


@dataclass
class Options(argparse.Namespace):
    """Explicit tool/platform selection and fresh local evidence; no target-host option."""

    output: Path = Path()
    tools_directory: Path | None = None
    tool_platform: str | None = None
    openssl_prefix: Path = Path()


def run(args: Options) -> bool:
    """Never mutate the user's source; retain compact evidence and remove only our private copy."""
    policy = policy_at(POLICY)
    args.output.mkdir(mode=0o700)
    output = args.output.resolve(strict=True)
    context = Context(ROOT, output)
    result: JsonObject = {"passed": False, "policySha256": digest(POLICY)}
    source: Path | None = None
    started = time.monotonic()
    try:
        platform = args.tool_platform or current_platform()
        require(
            platform in ("linux-x86_64", "darwin-x86_64")
            and platform.startswith("darwin-" if sys.platform == "darwin" else "linux-"),
            "mutation_tool_platform",
        )
        tool = tool_path("cargo-mutants", args.tools_directory, target_platform=platform)
        result.update(
            toolPlatform=platform,
            toolSha256=digest(tool),
            toolLockSha256=digest(ROOT / "build/security-tools.lock.json"),
        )
        source = output / "source"
        _ = context.snapshot()
        result["sourceManifestSha256"] = digest(output / "source-manifest.json")
        # Build scripts are trusted source inputs; preserve their executable bits in our copy.
        for name in read(output / "source-manifest.json"):
            if (ROOT / name).stat().st_mode & 0o111:
                (source / name).chmod(0o700)
        cargo_environment(context, source, args.openssl_prefix)
        _, listing = context.run(
            "mutation-inventory",
            command(tool, source, output / "results", policy, listing=True),
            cwd=source,
            timeout=120,
        )
        expected = mutant_inventory(decode_json(listing), policy)
        write(output / "inventory.json", list[JsonValue](expected.values()))
        result["enumeratedMutants"] = len(expected)
        remaining = policy.total_seconds - int(time.monotonic() - started)
        require(remaining > 0, "mutation_deadline")
        status = execute_mutants(
            context,
            command(tool, source, output / "results", policy, listing=False),
            source,
            remaining,
        )
        summary = assess(output / "results/mutants.out", expected, policy)
        write(output / "summary.json", summary)
        result.update(
            summary=summary, exitStatus=status, elapsedSeconds=round(time.monotonic() - started, 3)
        )
        result["passed"] = status == 0 and summary["passed"] is True
    except (ValueError, OSError, KeyError, ToolError, bounded_process.ProcessError) as error:
        result["error"] = (
            str(error)
            if isinstance(error, (ToolError, bounded_process.ProcessError))
            else type(error).__name__
        )
    finally:
        if source is not None and source.exists():
            try:
                shutil.rmtree(source)
            except OSError:
                result["passed"] = False
                result["error"] = "mutation_snapshot_cleanup"
        write(output / "outcome.json", result)
    return result["passed"] is True


def main() -> int:
    """Run optional local/scheduled test-quality checks independently of ordinary security gates."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("--output", type=Path, required=True)
    _ = parser.add_argument("--openssl-prefix", type=Path, required=True)
    _ = parser.add_argument("--tools-directory", type=Path)
    _ = parser.add_argument("--tool-platform", choices=("linux-x86_64", "darwin-x86_64"))
    args = parser.parse_args(namespace=Options())
    _ = os.umask(0o077)
    try:
        passed = run(args)
    except (ValueError, OSError, ToolError):
        passed = False
    print(  # noqa: T201 -- Fixed CLI summary.
        "Mutation policy checks "
        + ("passed" if passed else "failed")
        + "; evidence: "
        + str(args.output)
    )
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
