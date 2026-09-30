"""Acquire the exact signed main release before a disposable VM job starts.

This reuses the deployment controller's current CI selection and cryptographic
verification. It has no SSH, inventory, provider or engine operation. GitHub
credentials are needed only for this acquisition step, never inside the guest.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING

import release_attestation as attestation
import release_build
import release_deploy as deploy
import release_fetch_controller as fetch
from security_tools import require, write_private

# isort: split
import bounded_process
import release_fetch_receiver as receiver
from release_json import integer_value

if TYPE_CHECKING:
    from collections.abc import Sequence

ROOT = Path(__file__).resolve().parents[1]


@dataclass
class Options(argparse.Namespace):
    """Select only a repository identity and fresh local evidence destination."""

    repository: str = ""
    output: Path = Path()


def prepare(repository: str, output: Path) -> None:
    """Require clean source and a verified current-head archive before returning."""
    require(re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository), "vm_repository")
    revision = deploy.checkout_identity(release_build.Runner(), ROOT, repository)
    envelope = deploy.select_ci_artifact(
        deploy.DeployOptions(repository=repository, wait_seconds=900), revision
    )
    output.mkdir(mode=0o700)
    selection = fetch.ReleaseSelection(
        repository=repository,
        revision=revision,
        artifact_id=integer_value(envelope["artifactId"]),
        ci_run=integer_value(envelope["ciRunId"]),
    )
    _ = attestation.fetch_verify(fetch.download_url(selection), output / "artifact", envelope)
    write_private(
        output / "selection.json",
        (
            json.dumps(
                {
                    "revision": revision,
                    "ciRunId": envelope["ciRunId"],
                    "artifactId": envelope["artifactId"],
                }
            )
            + "\n"
        ).encode(),
        0o600,
    )


def main(argv: Sequence[str] | None = None) -> int:
    """Fail without starting guest work when selection or signature validation fails."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("--repository", required=True)
    _ = parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args(argv, namespace=Options())
    _ = os.umask(0o077)
    try:
        prepare(args.repository, args.output.resolve())
    except (
        ValueError,
        OSError,
        KeyError,
        RuntimeError,
        deploy.DeployError,
        fetch.FetchError,
        receiver.FetchError,
        bounded_process.ProcessError,
    ):
        _ = sys.stderr.write("Signed VM release acquisition failed; guest work was not started.\n")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
