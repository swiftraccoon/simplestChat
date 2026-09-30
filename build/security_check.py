"""Run the shared blocking source gate, bounded native tier or exact-image gate.

The fast tier requires Node/npm, the repository Rust toolchain and ShellCheck.
Scanner binaries and Python wheels are installed from reviewed hash locks. Raw
reports stay in a private evidence directory; summary.json contains only check
identities, status and elapsed time and is the uploadable CI evidence contract.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
import tempfile
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING

import security_findings as findings
from security_context import ROOT, Context, executable, json_object
from security_policy import read_exceptions
from security_secrets import neutral_snapshot
from security_tools import ToolError, install, require, string, tool_path, write_private

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
    """Scan current nonignored bytes and the explicit Git change range with full redaction."""
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
    ]
    scanner = str(tool_path("gitleaks", tools))
    tree_options = list(common)
    tree_options[1] = str(translated)
    _ = context.run("gitleaks-tree", [scanner, "dir", str(neutral), *tree_options])
    if base is not None:
        require(re.fullmatch(r"[a-f0-9]{40}", base), "invalid_security_diff_base")
        _ = context.run(
            "diff-base", [executable("git"), "merge-base", "--is-ancestor", base, "HEAD"]
        )
        _ = context.run(
            "gitleaks-diff", [scanner, "git", ".", "--log-opts=" + base + "..HEAD", *common]
        )
    _ = context.run("gitleaks-working-diff", [scanner, "git", ".", "--pre-commit", *common])


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
        _ = context.run(
            "squawk",
            [
                str(tool_path("squawk", tools)),
                "--pg-version=18.6",
                "--assume-in-transaction",
                "--reporter=json",
                *migrations,
            ],
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
    source_checks(context, tools)
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


def deep(context: Context, engine: str, base: str | None) -> None:
    """Add authenticated vendor comparison and finite, isolated native sanitizer checks."""
    fast(context, base)
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


@dataclass
class Options(argparse.Namespace):
    """Declare the supported security tiers and explicit execution inputs."""

    tier: str = ""
    image: str | None = None
    base: str | None = None
    output: Path | None = None
    engine: str = "docker"
    artifact_dir: Path | None = None


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
        if args.tier == "fast":
            fast(context, args.base)
        elif args.tier == "deep":
            deep(context, args.engine, args.base)
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
