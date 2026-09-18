"""Run the typed prepared-host release orchestrator from an ordinary checkout."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "ops/ansible/files"))

from release_deploy import main

if __name__ == "__main__":
    raise SystemExit(main())
