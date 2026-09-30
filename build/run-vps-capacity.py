#!/usr/bin/env python3
"""Run the maintained private VPS capacity controller from a checkout."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "ops/ansible/files"))

from vps_capacity_controller import main

if __name__ == "__main__":
    raise SystemExit(main())
