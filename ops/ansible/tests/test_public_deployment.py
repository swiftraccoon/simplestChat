"""Offline deployment ordering and syntax checks; never execute the launcher."""

from pathlib import Path
import subprocess
import unittest

import yaml

ROOT = Path(__file__).resolve().parents[1]


class PublicDeploymentTests(unittest.TestCase):
    def test_installed_shell_commands_parse_and_pass_shellcheck(self):
        scripts = [str(ROOT / 'files' / name) for name in ['deploy-public.sh', 'public-compose.sh']]
        for script in scripts:
            result = subprocess.run(['bash', '-n', script], capture_output=True, text=True, timeout=15)
            self.assertEqual(result.returncode, 0, result.stderr)
        result = subprocess.run(['shellcheck', *scripts], capture_output=True, text=True, timeout=15)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_enrollment_is_private_and_inventory_policy_precedes_public_start(self):
        script = (ROOT / 'files/deploy-public.sh').read_text()
        self.assertLess(script.index('flock -n 9'), script.index('phase=proxy_validation'))
        self.assertIn('Stop the public application and proxy before maintenance.', script)
        self.assertLess(script.index('REGISTRATION_ENABLED=true compose up'), script.index('phase=lobby'))
        self.assertLess(script.index('phase=lobby'), script.index('phase=runtime_policy'))
        self.assertLess(script.index('phase=runtime_policy'), script.index('phase=public_proxy'))
        self.assertIn('--profile maintenance stop caddy simplestchat migrate', script)
        self.assertNotIn('down --volumes', script)
        self.assertNotIn('docker system prune', script)

    def test_verified_artifacts_and_migrations_precede_runtime_privileges(self):
        script = (ROOT / 'files/deploy-public.sh').read_text()
        self.assertIn('git -C "$source_root" status --porcelain --untracked-files=all', script)
        self.assertIn('org.opencontainers.image.revision', script)
        self.assertIn('--cap-drop ALL --cap-add NET_BIND_SERVICE', script)
        self.assertIn('hashlib.sha384', script)
        self.assertIn('actual == expected', script)
        self.assertLess(script.index('actual == expected'), script.index('"$config/runtime-grants.sql"'))
        self.assertLess(script.index('.ExitCode == 0'), script.index('docker_owned rm "$migration_id"'))
        self.assertIn("has_table_privilege('simplestchat_app','public._sqlx_migrations'", script)

    def test_preparation_requires_opt_in_and_does_not_start_services(self):
        play = yaml.safe_load((ROOT / 'public.yml').read_text())[0]
        self.assertIn('scpub_enabled | bool', play['pre_tasks'][0]['ansible.builtin.assert']['that'])
        for task in play['tasks']:
            self.assertNotIn('ansible.builtin.systemd_service', task)
            command = task.get('ansible.builtin.command', {}).get('argv', [])
            self.assertNotIn('up', command)
            self.assertNotIn('start', command)
        text = (ROOT / 'public.yml').read_text()
        self.assertIn('os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600', text)
        self.assertIn('cacheable: false', text)
        self.assertIn('no_log: true', text)

    def test_unfinished_release_preflight_precedes_public_configuration(self):
        play = yaml.safe_load((ROOT / 'public.yml').read_text())[0]
        guards = play['pre_tasks']
        release = next(task for task in guards if 'release-state.json' in str(task))
        stopped = next(task for task in guards if 'scpub_active_containers' == task.get('register'))
        self.assertLess(guards.index(release), guards.index(stopped))
        self.assertEqual(release['tags'], ['always'])
        self.assertIs(release['changed_when'], False)
        self.assertIs(release['check_mode'], False)
        script = (ROOT / 'files/deploy-public.sh').read_text()
        journal = script.index("record = Path('/srv/simplestchat-public/release-state.json')")
        self.assertLess(script.index('flock -n 9'), journal)
        self.assertLess(journal, script.index('compose()'))


if __name__ == '__main__':
    unittest.main()
