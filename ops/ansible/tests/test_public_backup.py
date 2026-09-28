"""The nightly backup playbook, script and units."""

import subprocess
import tempfile
import unittest
from pathlib import Path

from test_public_templates import render
from test_support import ROOT, yaml_value


class NightlyBackupTests(unittest.TestCase):
    """Backups validate before they count, and the units stay bounded."""

    def test_script_dumps_validates_receipts_and_prunes(self) -> None:
        """The rendered script parses, and its stages appear in order."""
        script = render("backup-nightly.sh.j2")
        with tempfile.NamedTemporaryFile("w", suffix=".sh", delete=False) as handle:
            _ = handle.write(script)
            path = handle.name
        try:
            _ = subprocess.run(  # noqa: S603 - fixed local argv; parses the rendered script only.
                ["/bin/bash", "-n", path], check=True, capture_output=True
            )
        finally:
            Path(path).unlink()
        order = [
            "set -euo pipefail",
            "pg_dump --host /run/simplestchat-postgres --username postgres"
            + " --dbname simplestchat --format custom",
            "pg_restore --list",
            '"$stamp.dump" "$bytes" "$sha256"',
            "i = keep_at_least",
            'rm -f -- "$target"/*.partial',
        ]
        positions = [script.index(item) for item in order]
        self.assertEqual(positions, sorted(positions))
        self.assertIn("keep_days=14", script)
        self.assertIn("keep_at_least=3", script)

    def test_units_are_a_persistent_nightly_timer_and_a_hardened_oneshot(self) -> None:
        """A missed night runs at the next boot; the service can write only its backups."""
        timer = render("backup-nightly.timer.j2")
        self.assertIn("OnCalendar=*-*-* 04:00:00 UTC", timer)
        self.assertIn("Persistent=true", timer)
        service = render("backup-nightly.service.j2")
        self.assertIn("Type=oneshot", service)
        self.assertIn(
            "ExecStart=/usr/bin/bash /usr/local/libexec/simplestchat-public/backup-nightly.sh",
            service,
        )
        self.assertIn("ReadWritePaths=/srv/simplestchat-public/backups", service)
        self.assertIn("ProtectSystem=strict", service)

    def test_playbook_requires_the_opt_in_and_installs_the_timer(self) -> None:
        """The playbook is explicit about its opt-in and enables the timer it installs."""
        plays = yaml_value((ROOT / "ops/ansible/backup.yml").read_text())
        text = (ROOT / "ops/ansible/backup.yml").read_text()
        self.assertIsInstance(plays, list)
        self.assertIn("scpub_backup_enabled | bool", text)
        self.assertIn("name: simplestchat-backup.timer", text)
        self.assertIn("validate: /usr/bin/bash -n %s", text)


if __name__ == "__main__":
    _ = unittest.main()
