"""Move one public deployment with the verified release and migration controller."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "ops/ansible/files"))

from migration_controller import main

if __name__ == "__main__":
    raise SystemExit(main())
