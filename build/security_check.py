"""Run the shared blocking source gate, bounded native tier or exact-image gate.

The fast tier requires Node/npm, the repository Rust toolchain and ShellCheck.
Scanner binaries and Python wheels are installed from reviewed hash locks. Raw
reports stay in a private evidence directory; summary.json contains only check
identities, status and elapsed time and is the uploadable CI evidence contract.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import re
import sys
import tempfile
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, cast

import security_actions
import security_dependency_licenses
import security_findings as findings
import security_openssl
from security_context import MAX_REPORT, ROOT, Context, executable, json_object
from security_policy import read_exceptions
from security_secret_projection import project
from security_secrets import neutral_snapshot
from security_tools import (
    ToolError,
    bounded_file,
    current_platform,
    install,
    require,
    string,
    tool_path,
    write_private,
)

if TYPE_CHECKING:
    from collections.abc import Sequence

    from security_policy import ExceptionRecord


def workflow_checks(context: Context, tools: Path, reviews: Sequence[ExceptionRecord]) -> None:
    """Check workflow syntax and every offline/pedantic action-security finding."""
    _ = context.run("actionlint", [str(tool_path("actionlint", tools))])
    status, output = context.run(
        "zizmor",
        [
            str(tool_path("zizmor", tools)),
            "--offline",
            "--pedantic",
            "--no-ignores",
            "--no-config",
            "--strict-collection",
            "--collect=all",
            "--format=json-v1",
            ".github",
        ],
        accepted=(0, 11, 12, 13, 14),
    )
    reviewed = findings.zizmor_findings(output, reviews)
    require(status == 0 or reviewed > 0, "zizmor_unsuccessful_empty_report")
    context.checks[-1]["reviewedFindings"] = reviewed


def secret_checks(
    context: Context,
    tools: Path,
    reviews: Sequence[ExceptionRecord],
    base: str | None,
    snapshot: Path,
) -> None:
    """Scan current ASCII projections and textual Git changes with full redaction."""
    config = context.root / "security/gitleaks.toml"
    findings.gitleaks_configuration(config, reviews)
    neutral, translated = neutral_snapshot(context, snapshot, config)
    common = [
        "--config",
        str(config),
        "--redact=100",
        "--ignore-gitleaks-allow",
        "--gitleaks-ignore-path",
        os.devnull,
        "--log-level=warn",
        "--no-banner",
        "--report-format=json",
        "--report-path=-",
        # Source snapshots already bound each original file and total bytes.
        # A scanner-specific decimal-MB limit must not silently skip projections.
        "--max-target-megabytes=0",
        "--max-decode-depth=3",
    ]
    scanner = str(tool_path("gitleaks", tools))
    secret_selftest(context, scanner, common)
    tree_options = list(common)
    tree_options[1] = str(translated)
    clean_secret_scan(context, "gitleaks-tree", [scanner, "dir", str(neutral), *tree_options])
    coverage_bytes = bounded_file(context.output / "secret-coverage.json", MAX_REPORT)
    coverage = json_object(coverage_bytes)
    context.checks[-1].update(
        sourceFiles=coverage["files"],
        sourceBytes=coverage["sourceBytes"],
        projectionBytes=coverage["projectionBytes"],
        projectionCoverageSha256=hashlib.sha256(coverage_bytes).hexdigest(),
    )
    if base is not None:
        require(re.fullmatch(r"[a-f0-9]{40}", base), "invalid_security_diff_base")
        _ = context.run(
            "diff-base", [executable("git"), "merge-base", "--is-ancestor", base, "HEAD"]
        )
        clean_secret_scan(
            context,
            "gitleaks-diff",
            [scanner, "git", ".", "--log-opts=" + base + "..HEAD", *common],
        )
    clean_secret_scan(
        context, "gitleaks-working-diff", [scanner, "git", ".", "--pre-commit", *common]
    )


def clean_secret_scan(context: Context, name: str, arguments: Sequence[str]) -> None:
    """Require a successful scanner to produce an explicit empty findings array."""
    _, output = context.run(name, arguments)
    require(
        findings.list_value(cast("object", json.loads(output))) == [], "secret_findings_present"
    )


def secret_selftest(context: Context, scanner: str, options: Sequence[str]) -> None:
    """Prove the actual detector finds a redacted inert credential in projected binary bytes."""
    canary = (
        "ghp_"
        + base64.b64encode(hashlib.sha512(b"inert source scanner coverage only").digest())
        .decode()
        .replace("+", "x")
        .replace("/", "y")[:36]
    )
    source = context.output / "secret-canary.original"
    write_private(
        source,
        b"\x7fELF\x02\x01\x01\x00" + ("TOKEN=" + canary + " # gitleaks:allow\n").encode(),
        0o600,
    )
    inputs = context.output / "secret-canary-inputs"
    inputs.mkdir(mode=0o700)
    target = inputs / "content000000"
    projection = project(source, target, max_bytes=1024)
    configuration = context.output / "secret-canary.toml"
    write_private(configuration, b"[extend]\nuseDefault = true\n", 0o600)
    arguments = list(options)
    arguments[1] = str(configuration)
    _, output = context.run(
        "gitleaks-selftest",
        [scanner, "dir", str(inputs), *arguments, "--exit-code=10"],
        accepted=(10,),
    )
    entries = findings.list_value(cast("object", json.loads(output)))
    require(len(entries) == 1, "secret_detector_selftest_count")
    entry = findings.object_value(entries[0])
    require(
        entry.get("RuleID") == "github-pat"
        and entry.get("Secret") == "REDACTED"
        and Path(string(entry["File"])).resolve() == target.resolve()
        and canary.encode() not in output,
        "secret_detector_selftest_identity",
    )
    context.checks[-1].update(
        expectedFinding="github-pat",
        sourceSha256=projection.source_sha256,
        projectionSha256=projection.projection_sha256,
    )


def dependency_checks(context: Context, tools: Path, reviews: Sequence[ExceptionRecord]) -> None:
    """Audit current vulnerability databases and enforce the explicit source/license budget."""
    _, output = context.run(
        "cargo-audit",
        [
            str(tool_path("cargo-audit", tools)),
            "audit",
            "--json",
            "--file",
            str(context.root / "Cargo.lock"),
            "--db",
            str(context.root / "target/security-rustsec-db"),
        ],
        accepted=(0, 1),
        timeout=300,
        cwd=context.output / "home",
        env_updates={"CARGO_HOME": str(context.output / "cargo-audit-home")},
    )
    lock = findings.object_value(tomllib.loads((context.root / "Cargo.lock").read_text()))
    reviewed = findings.cargo_findings(output, reviews, len(findings.list_value(lock["package"])))
    require(
        context.checks[-1]["exitStatus"] == 0 or reviewed > 0, "cargo_unsuccessful_empty_report"
    )
    context.checks[-1]["reviewedFindings"] = reviewed
    _ = context.run(
        "cargo-deny",
        [str(tool_path("cargo-deny", tools)), "--locked", "check", "bans", "licenses", "sources"],
        timeout=300,
    )
    _, output = context.run(
        "cargo-metadata",
        [executable("cargo"), "metadata", "--locked", "--all-features", "--format-version=1"],
        timeout=300,
    )
    findings.dependency_budget(output, context.root / "security/dependency-budget.json")
    for index, requirements in enumerate(
        (
            "build/python-requirements.txt",
            "ops/ansible/requirements.txt",
            "security/requirements.txt",
            "vendor/mediasoup-sys-0.17.0/python-invoke-requirements.txt",
            "vendor/mediasoup-sys-0.17.0/python-tools-requirements.txt",
        )
    ):
        _, output = context.run(
            f"pip-audit-{index}",
            [
                sys.executable,
                "-m",
                "pip_audit",
                "--require-hashes",
                "--disable-pip",
                "-r",
                requirements,
                "--format=json",
            ],
            timeout=300,
        )
        dependencies = findings.list_value(json_object(output)["dependencies"])
        require(
            all(
                not findings.list_value(findings.object_value(item)["vulns"])
                for item in dependencies
            ),
            "python_dependency_advisory",
        )
    for index, package in enumerate(("web", "web/e2e")):
        _, output = context.run(
            f"npm-audit-{index}",
            [executable("npm"), "audit", "--package-lock-only", "--ignore-scripts", "--json"],
            cwd=context.root / package,
            timeout=300,
        )
        metadata = findings.object_value(json_object(output)["metadata"])
        vulnerabilities = findings.object_value(metadata["vulnerabilities"])
        require(vulnerabilities["total"] == 0, "npm_dependency_advisory")


def source_checks(context: Context, tools: Path) -> None:
    """Run tested repository rules and inspect new SQL plus rendered runtime restrictions."""
    _ = context.run(
        "semgrep",
        [
            sys.executable,
            "build/security_semgrep.py",
            "check",
            "--root",
            str(context.root),
            "--output",
            str(context.output / "semgrep"),
        ],
        timeout=300,
    )
    migrations = findings.new_migrations(context.root)
    if migrations:
        config = context.output / "squawk.toml"
        write_private(
            config,
            b"excluded_rules = []\nincluded_rules = []\nexcluded_paths = []\n"
            + b'assume_in_transaction = true\npg_version = "18.6"\n',
            0o600,
        )
        _, output = context.run(
            "squawk",
            [
                str(tool_path("squawk", tools)),
                "--config",
                str(config),
                "--pg-version=18.6",
                "--assume-in-transaction",
                "--reporter=json",
                *(str(context.root / name) for name in migrations),
            ],
            cwd=context.output / "home",
        )
        require(
            findings.list_value(cast("object", json.loads(output))) == [],
            "migration_policy_findings",
        )
    context.checks.append(
        {"name": "migration-review", "newFiles": len(migrations), "exitStatus": 0}
    )
    _ = context.run(
        "runtime-configuration",
        [
            sys.executable,
            "-m",
            "unittest",
            "discover",
            "-s",
            "ops/ansible/tests",
            "-p",
            "test_public_templates.py",
        ],
        timeout=180,
    )


def fast(context: Context, base: str | None) -> None:
    """Run the same blocking checks for a local checkout and GitHub Actions."""
    _, revision = context.run("source-revision", [executable("git"), "rev-parse", "HEAD"])
    snapshot = context.snapshot()
    reviews = read_exceptions(snapshot / "security/exceptions.json")
    tools = install([])
    workflow_checks(context, tools, reviews)
    secret_checks(context, tools, reviews, base, snapshot)
    dependency_checks(context, tools, reviews)
    security_dependency_licenses.check(context, snapshot, base)
    security_actions.check(context, snapshot, base)
    source_checks(context, tools)
    security_openssl.check(context, snapshot)
    _ = context.snapshot("final-source")
    original = (context.output / "source-manifest.json").read_bytes()
    require(
        original == (context.output / "final-source-manifest.json").read_bytes(),
        "security_source_changed_during_checks",
    )
    _, final_revision = context.run("final-revision", [executable("git"), "rev-parse", "HEAD"])
    require(revision == final_revision, "security_revision_changed_during_checks")
    context.checks.append(
        {
            "name": "source-identity",
            "exitStatus": 0,
            "revision": revision.decode().strip(),
            "sourceSha256": hashlib.sha256(original).hexdigest(),
        }
    )


def deep(context: Context, args: Options) -> None:
    """Run the requested native and test-quality checks after the complete source gate."""
    fast(context, args.base)
    _ = context.run(
        "vendor-integrity",
        [
            sys.executable,
            "build/security_vendor.py",
            "verify",
            "--root",
            str(context.root),
            "--cache",
            str(context.output / "vendor-cache"),
            "--output",
            str(context.output / "vendor"),
        ],
        timeout=900,
    )
    if args.deep_check in ("all", "native"):
        native_checks(context, args.engine)
    if args.deep_check in ("all", "mutation"):
        mutation_checks(context, args)


def native_checks(context: Context, engine: str) -> None:
    """Compile and execute the finite suites in the owned offline native sandbox."""
    preparation = context.output / "native-preparation"
    _ = context.run(
        "native-prepare",
        [
            sys.executable,
            "build/native_security.py",
            "prepare",
            "--engine",
            engine,
            "--output",
            str(preparation),
        ],
        timeout=1800,
    )
    result = json_object((preparation / "report.json").read_bytes())
    image_id = string(result["imageId"])
    for mode in ("asan", "ubsan", "replay"):
        _ = context.run(
            "native-" + mode,
            [
                sys.executable,
                "build/native_security.py",
                "run",
                "--engine",
                engine,
                "--image",
                image_id,
                "--mode",
                mode,
                "--output",
                str(context.output / ("native-" + mode)),
            ],
            timeout=1800,
        )


def mutation_checks(context: Context, args: Options) -> None:
    """Use authenticated tooling and a private full-source copy for pure policy mutations."""
    platform = args.mutation_tool_platform or current_platform()
    tools = install(["cargo-mutants"], context.output / "mutation-tools", target_platform=platform)
    prefix = args.openssl_prefix or Path(
        os.environ.get("OPENSSL_DIR", str(context.root / "target/openssl-3.5.9"))
    )
    _ = context.run(
        "mutation-policy",
        [
            sys.executable,
            "build/security_mutation.py",
            "--tools-directory",
            str(tools),
            "--tool-platform",
            platform,
            "--openssl-prefix",
            str(prefix.resolve(strict=True)),
            "--output",
            str(context.output / "mutation"),
        ],
        timeout=3660,
    )


@dataclass
class Options(argparse.Namespace):
    """Declare the supported security tiers and explicit execution inputs."""

    tier: str = ""
    image: str | None = None
    base: str | None = None
    output: Path | None = None
    engine: str = "docker"
    artifact_dir: Path | None = None
    deep_check: str = "all"
    openssl_prefix: Path | None = None
    mutation_tool_platform: str | None = None


def image(context: Context, args: Options) -> None:
    """Export exactly once when needed and scan the authenticated production archive."""
    artifact_dir = args.artifact_dir
    if artifact_dir is None:
        require(args.engine == "docker", "canonical_release_export_requires_docker")
        artifact_dir = context.output / "export"
        _ = context.run(
            "image-export",
            [
                sys.executable,
                "build/build-release.py",
                "--image-id",
                args.image or "",
                "--output",
                str(artifact_dir),
            ],
            timeout=1800,
        )
    scanner_directory = install(
        ["syft", "grype", "gitleaks"],
        context.output / "image-tools",
        target_platform="linux-x86_64",
    )
    _ = context.run(
        "image-security",
        [
            sys.executable,
            "build/security_image.py",
            args.image or "",
            "--artifact-dir",
            str(artifact_dir),
            "--output",
            str(context.output / "image"),
            "--engine",
            args.engine,
            "--tools-directory",
            str(scanner_directory),
        ],
        timeout=1800,
    )


def main(argv: Sequence[str] | None = None) -> int:
    """Create fresh private evidence and return failure whenever any gate is incomplete."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("tier", choices=("fast", "deep", "image"))
    _ = parser.add_argument("image", nargs="?")
    _ = parser.add_argument("--base")
    _ = parser.add_argument("--output", type=Path)
    _ = parser.add_argument("--engine", choices=("docker", "podman"), default="docker")
    _ = parser.add_argument("--artifact-dir", type=Path)
    _ = parser.add_argument("--deep-check", choices=("all", "native", "mutation"), default="all")
    _ = parser.add_argument("--openssl-prefix", type=Path)
    _ = parser.add_argument("--mutation-tool-platform", choices=("linux-x86_64", "darwin-x86_64"))
    args = parser.parse_args(argv, namespace=Options())
    _ = os.umask(0o077)
    (ROOT / "results").mkdir(exist_ok=True)
    output = (
        args.output.resolve()
        if args.output is not None
        else Path(tempfile.mkdtemp(prefix="security." + args.tier + ".", dir=ROOT / "results"))
    )
    if args.output is not None:
        output.mkdir(mode=0o700)
    context = Context(ROOT, output)
    passed = False
    failure: str | None = None
    try:
        require((args.tier == "image") == (args.image is not None), "security_image_argument")
        require(
            args.tier == "deep"
            or (
                args.deep_check == "all"
                and args.openssl_prefix is None
                and args.mutation_tool_platform is None
            ),
            "security_deep_arguments",
        )
        if args.tier == "fast":
            fast(context, args.base)
        elif args.tier == "deep":
            deep(context, args)
        else:
            image(context, args)
        passed = True
    except (ToolError, OSError, ValueError, KeyError, RuntimeError) as error:
        failure = str(error) if isinstance(error, ToolError) else type(error).__name__
    finally:
        write_private(
            output / "summary.json",
            (
                json.dumps(
                    {
                        "schemaVersion": 1,
                        "tier": args.tier,
                        "deepCheck": args.deep_check if args.tier == "deep" else None,
                        "passed": passed,
                        "failure": failure,
                        "checks": context.checks,
                    },
                    indent=2,
                )
                + "\n"
            ).encode(),
            0o600,
        )
    _ = sys.stdout.write(
        f"Security {args.tier}: {'passed' if passed else 'failed'}; evidence: {output}\n"
    )
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
