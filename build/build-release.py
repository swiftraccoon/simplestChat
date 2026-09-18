"""Run the typed production release builder from an ordinary checkout."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "ops/ansible/files"))

from release_build import main

if __name__ == "__main__":
    raise SystemExit(main())
