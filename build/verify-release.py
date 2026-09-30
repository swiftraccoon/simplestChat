"""Verify signed release bytes locally before an Ansible play can contact its host."""

import argparse
import json
import os
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "ops/ansible/files"))

# isort: split
import bounded_process
import release_attestation as attest
import release_fetch_controller as fetch
import release_fetch_receiver as receiver
from release_json import object_value


class Options(argparse.Namespace):
    """Explicit GitHub selectors and an optional already downloaded candidate."""

    repository: str = ""
    revision: str = ""
    artifact_id: int = 0
    ci_run: int = 0
    artifact_dir: str = ""
    output_parent: Path = Path()


def main() -> int:
    """Verify current API identity and cryptographic evidence without SSH or engine calls."""
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("repository", "revision"):
        _ = parser.add_argument("--" + name, required=True)
    for name in ("artifact-id", "ci-run"):
        _ = parser.add_argument("--" + name, type=int, required=True)
    _ = parser.add_argument("--artifact-dir", default="")
    _ = parser.add_argument("--output-parent", type=Path, required=True)
    args = parser.parse_args(namespace=Options())
    _ = os.umask(0o077)
    try:
        _ = attest.selectors(args.repository, args.revision, args.ci_run, 1)
        fetch.require(fetch.positive(args.artifact_id), "invalid_artifact_id")
        selection = fetch.ReleaseSelection(
            args.repository, args.artifact_id, args.revision, args.ci_run
        )
        envelope = fetch.verified_envelope(selection)
        if args.artifact_dir:
            directory = Path(args.artifact_dir).resolve(strict=True)
            verified = attest.verify_directory(directory, envelope)
        else:
            parent = args.output_parent
            fetch.require(
                parent.is_absolute()
                and not parent.is_symlink()
                and parent.resolve() == parent
                and parent.parent.is_dir(),
                "invalid_evidence_parent",
            )
            parent.mkdir(mode=0o700, exist_ok=True)
            directory = (
                Path(tempfile.mkdtemp(prefix="release-verification.", dir=parent)) / "artifact"
            )
            verified = attest.fetch_verify(fetch.download_url(selection), directory, envelope)
        claim = object_value(object_value(verified["attestation"])["predicate"])
        files = object_value(claim["fileDigests"])
        print(  # noqa: T201 -- Public verified identities only.
            json.dumps(
                {
                    "revision": args.revision,
                    "directory": str(directory),
                    "image.tar": files["image.tar"],
                    "release.json": files["release.json"],
                }
            )
        )
    except (
        ValueError,
        OSError,
        KeyError,
        fetch.FetchError,
        receiver.FetchError,
        bounded_process.ProcessError,
    ):
        print(json.dumps({"passed": False, "failureClass": "release_attestation_failed"}))  # noqa: T201 -- Fixed CLI failure.
        return 1
    else:
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
