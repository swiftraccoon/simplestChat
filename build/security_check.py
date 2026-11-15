"""Run the exact-image gate.

Scanner binaries and Python wheels are installed from reviewed hash locks. Raw
reports stay in a private evidence directory; summary.json contains only check
identities, status and elapsed time and is the uploadable CI evidence contract.
The source tiers are `simplestchat-security fast` and `deep`.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING

from security_context import ROOT, Context
from security_tools import ToolError, install, require, write_private

if TYPE_CHECKING:
    from collections.abc import Sequence


@dataclass
class Options(argparse.Namespace):
    """Declare the image tier and its explicit execution inputs."""

    tier: str = ""
    image: str | None = None
    output: Path | None = None
    engine: str = "docker"
    artifact_dir: Path | None = None
    image_tools: Path | None = None
    image_database_cache: Path | None = None


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
    if args.image_tools is not None:
        require(
            args.image_tools.is_absolute()
            and args.image_tools.is_dir()
            and not args.image_tools.is_symlink(),
            "invalid_prepared_image_tools",
        )
    # Existing installations are reauthenticated against the committed tool
    # lock, including every executable hash; a prepared directory is not trust.
    scanner_directory = install(
        ["syft", "grype", "gitleaks"],
        args.image_tools if args.image_tools is not None else context.output / "image-tools",
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
            *(
                ["--database-cache", str(args.image_database_cache)]
                if args.image_database_cache is not None
                else []
            ),
        ],
        timeout=1800,
    )


def main(argv: Sequence[str] | None = None) -> int:
    """Create fresh private evidence and return failure whenever the gate is incomplete."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("tier", choices=("image",))
    _ = parser.add_argument("image")
    _ = parser.add_argument("--output", type=Path)
    _ = parser.add_argument("--engine", choices=("docker", "podman"), default="docker")
    _ = parser.add_argument("--artifact-dir", type=Path)
    _ = parser.add_argument("--image-tools", type=Path)
    _ = parser.add_argument("--image-database-cache", type=Path)
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
                        "deepCheck": None,
                        "includeVendor": False,
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
