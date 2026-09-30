"""Expose the checkout's capacity model to managed deployment templates."""

import sys
from collections.abc import Callable
from pathlib import Path

# Filters run on the controller, where the complete checkout is available.
# Sharing the adviser avoids drifting copies of memory and network formulas.
sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "build"))

from capacity import managed_limits


class FilterModule:
    """Ansible's filter discovery contract."""

    @staticmethod
    def filters() -> dict[str, Callable[[int, int, float, int], dict[str, int]]]:
        """Return the pure, offline sizing calculation."""
        return {"simplestchat_capacity": managed_limits}
