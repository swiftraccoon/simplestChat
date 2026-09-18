#!/usr/bin/env python3
"""Run the disposable release-container integration command."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "ops/ansible/files"))

from release_container_harness import main

if __name__ == "__main__":
    raise SystemExit(main())
