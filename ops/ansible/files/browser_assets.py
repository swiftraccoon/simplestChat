"""Initialize or expire the managed public asset store under the release lock."""

import argparse
import json
import os
import sys
import time

import release_assets
from release_public import ROOT, ROOT_UID, ReleaseError, workload_lock


class Arguments(argparse.Namespace):
    """The only managed initialization and expiry operations."""

    action: str = ""
    revision: str | None = None


def main() -> int:
    """Serialize expiry with deployment and emit only bounded numeric evidence."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("action", choices=("initialize", "prune"))
    _ = parser.add_argument("--revision")
    arguments = parser.parse_args(namespace=Arguments())
    try:
        release_assets.require(os.geteuid() == ROOT_UID, "asset_command_owner")
        release_assets.require(
            arguments.revision is None or arguments.action == "initialize",
            "asset_command_arguments",
        )
        _ = os.umask(0o077)
        with workload_lock():
            root = ROOT / "browser-assets"
            release_assets.initialize(root)
            if arguments.action == "prune":
                release_assets.prune(root, int(time.time()))
            elif arguments.revision is not None:
                release_assets.select_version(root, arguments.revision)
        _ = sys.stdout.write('{"passed":true}\n')
    except BlockingIOError:
        # A running release already owns publication and will perform pruning.
        _ = sys.stdout.write('{"passed":true,"busy":true}\n')
    except (OSError, ValueError, ReleaseError) as error:
        failure = (
            str(error) if isinstance(error, release_assets.AssetError) else "asset_operation_failed"
        )
        _ = sys.stdout.write(json.dumps({"passed": False, "failure": failure}) + "\n")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
