"""Offline checks of the host hardening and proxy playbooks; nothing is contacted."""

import re
import unittest

# Fixture assertions and literal expected limits document the tested contract.
# ruff: noqa: S101
import test_support

# isort: split
from release_json import JsonObject
from test_support import at, obj, objects, string, strings, yaml_value

ANSIBLE = test_support.ROOT / "ops/ansible"


def play(name: str) -> JsonObject:
    """Load the single play of a maintained playbook."""
    return obj(yaml_value((ANSIBLE / name).read_text()), 0)


def names(tasks: list[JsonObject]) -> list[str]:
    """Task names in order."""
    return [string(task, "name") for task in tasks]


class HostHardeningPlaybook(unittest.TestCase):
    """Passwords are refused only after a proved key login and verified effective settings."""

    def test_sshd_drop_in_sorts_first_and_is_verified_before_the_reload(self) -> None:
        """The drop-in precedes provider files; sshd -T is asserted before any reload."""
        hardening = play("host-hardening.yml")
        self.assertEqual(hardening["hosts"], "benchmark_hosts")
        self.assertIs(hardening["become"], expr2=True)
        self.assertEqual(hardening["serial"], 1)
        variables = obj(hardening, "vars")
        self.assertTrue(
            string(variables, "schost_sshd_dropin").startswith("/etc/ssh/sshd_config.d/00-")
        )
        guards = objects(hardening, "pre_tasks")
        self.assertIn("scpub_enabled | bool", strings(guards[0], "ansible.builtin.assert", "that"))
        proof = guards[-1]
        self.assertEqual(proof["delegate_to"], "localhost")
        self.assertIs(proof["become"], expr2=False)
        self.assertIs(proof["changed_when"], expr2=False)
        login = string(guards[-2], "ansible.builtin.set_fact", "schost_login_proof")
        for option in (
            "BatchMode=yes",
            "PasswordAuthentication=no",
            "KbdInteractiveAuthentication=no",
        ):
            self.assertIn(option, login)
        tasks = objects(hardening, "tasks")
        order = names(tasks)
        dropin = tasks[0]
        content = string(dropin, "ansible.builtin.copy", "content")
        self.assertEqual(at(dropin, "ansible.builtin.copy", "mode"), "0600")
        for line in (
            "PasswordAuthentication no",
            "KbdInteractiveAuthentication no",
            "PermitRootLogin {{ schost_permit_root_login }}",
            "PubkeyAuthentication yes",
        ):
            self.assertIn(line, content.splitlines())
        self.assertEqual(
            strings(tasks[1], "ansible.builtin.command", "argv"), ["/usr/sbin/sshd", "-T"]
        )
        effective = strings(tasks[2], "ansible.builtin.assert", "that")
        self.assertTrue(any("'passwordauthentication no' in" in rule for rule in effective))
        self.assertTrue(any("permitrootlogin" in rule for rule in effective))
        self.assertEqual(dropin["notify"], "Reload the SSH daemon")
        self.assertEqual(
            tasks[3]["ansible.builtin.meta"],
            "flush_handlers",
            "the reload runs right after the assertion",
        )
        self.assertEqual(
            tasks[4]["delegate_to"], "localhost", "a fresh login is proved after the reload"
        )
        self.assertLess(
            order.index("Reload the SSH daemon now; sessions already open stay connected"),
            order.index("Prove that key-only login still works after the reload"),
        )
        handlers = objects(hardening, "handlers")
        reload = next(
            handler for handler in handlers if string(handler, "name") == "Reload the SSH daemon"
        )
        self.assertEqual(at(reload, "ansible.builtin.systemd_service", "state"), "reloaded")
        self.assertEqual(at(reload, "ansible.builtin.systemd_service", "name"), "ssh.service")
        self.assertFalse(
            any(
                "ansible.builtin.apt" in task or "ansible.builtin.lineinfile" in task
                for task in tasks
            )
        )

    def test_resolver_drop_in_closes_link_local_listeners_and_is_verified(self) -> None:
        """LLMNR and mDNS are disabled through a drop-in, applied by a restart and verified."""
        tasks = objects(play("host-hardening.yml"), "tasks")
        resolved = next(
            task for task in tasks if string(task, "name").startswith("Stop answering LLMNR")
        )
        content = string(resolved, "ansible.builtin.copy", "content")
        self.assertIn("LLMNR=no", content.splitlines())
        self.assertIn("MulticastDNS=no", content.splitlines())
        self.assertEqual(resolved["notify"], "Restart the stub resolver")
        self.assertEqual(tasks[tasks.index(resolved) + 1]["ansible.builtin.meta"], "flush_handlers")
        handlers = objects(play("host-hardening.yml"), "handlers")
        restart = next(
            handler
            for handler in handlers
            if string(handler, "name") == "Restart the stub resolver"
        )
        self.assertEqual(
            at(restart, "ansible.builtin.systemd_service", "name"), "systemd-resolved.service"
        )
        self.assertEqual(at(restart, "ansible.builtin.systemd_service", "state"), "restarted")
        verification = strings(tasks[-1], "ansible.builtin.assert", "that")
        self.assertIn("'+LLMNR' not in schost_resolver.stdout", verification)
        self.assertIn("'+mDNS' not in schost_resolver.stdout", verification)


class ProxyConfigPlaybook(unittest.TestCase):
    """The Caddyfile is validated offline, written in place and reloaded, then probed."""

    def test_candidate_is_validated_before_the_in_place_write_and_reload(self) -> None:
        """Validation precedes the in-place write; a stale mount restarts; reload before probe."""
        proxy = play("proxy-config.yml")
        self.assertEqual(proxy["hosts"], "benchmark_hosts")
        self.assertIs(proxy["become"], expr2=True)
        guards = objects(proxy, "pre_tasks")
        rules = strings(guards[0], "ansible.builtin.assert", "that")
        self.assertIn("scpub_enabled | bool", rules)
        self.assertTrue(any("scpub_caddy_image is match" in rule for rule in rules))
        tasks = objects(proxy, "tasks")
        order = names(tasks)
        validate = tasks[1]
        argv = strings(validate, "ansible.builtin.command", "argv")
        self.assertEqual(
            argv[-6:],
            ["caddy", "validate", "--config", "/etc/caddy/Caddyfile", "--adapter", "caddyfile"],
        )
        self.assertIn("none", argv[argv.index("--network") + 1])
        self.assertEqual(argv[argv.index("--cap-add") + 1], "NET_BIND_SERVICE")
        self.assertIn("{{ scproxy_candidate }}:/etc/caddy/Caddyfile:ro", argv)
        self.assertIs(validate["changed_when"], expr2=False)
        write = tasks[3]
        write_argv = strings(write, "ansible.builtin.command", "argv")
        self.assertEqual(
            write_argv[:2],
            ["cp", "--no-preserve=mode,ownership,timestamps"],
            "an in-place write keeps the mounted inode",
        )
        self.assertEqual(write_argv[-1], "{{ scpub_config }}/Caddyfile")
        self.assertEqual(write["notify"], "Reload the running proxy")
        restart = next(
            task for task in tasks if string(task, "name").startswith("Restart the proxy once")
        )
        self.assertIn("restart", strings(restart, "ansible.builtin.command", "argv"))
        self.assertIn("scproxy_mounted.stdout.split() | first !=", string(restart, "when"))
        self.assertLess(tasks.index(restart), order.index("Reload the running proxy now"))
        self.assertLess(
            order.index("Reload the running proxy now"),
            order.index("Read a response the proxy answers itself"),
        )
        reload = obj(proxy, "handlers", 0)
        self.assertEqual(string(reload, "name"), "Reload the running proxy")
        self.assertIn("reload", strings(reload, "ansible.builtin.command", "argv"))
        self.assertLess(
            order.index("Validate the candidate with the exact pinned proxy image, offline"),
            order.index("Write the Caddyfile in place so the container's bind mount sees it"),
        )
        probe = tasks[-2]
        self.assertEqual(
            at(probe, "ansible.builtin.uri", "url"), "https://{{ scpub_domain }}/metrics"
        )
        self.assertEqual(at(probe, "ansible.builtin.uri", "status_code"), [404])
        checks = strings(tasks[-1], "ansible.builtin.assert", "that")
        self.assertIn(
            "scproxy_probe.content_security_policy | default('') == scproxy_policy", checks
        )

    def test_policy_extraction_matches_the_single_default_only_line(self) -> None:
        """The playbook's regex finds exactly the Caddyfile's one default-only policy."""
        caddyfile = (test_support.ROOT / "Caddyfile").read_text()
        lines = [
            line.strip()
            for line in caddyfile.splitlines()
            if "Content-Security-Policy" in line and not line.strip().startswith("#")
        ]
        self.assertEqual(len(lines), 1)
        match = re.search(r'\?Content-Security-Policy "([^"]+)"', caddyfile)
        self.assertIsNotNone(match)
        assert match is not None
        self.assertTrue(match.group(1).startswith("default-src 'self'; "))
        self.assertNotIn("upgrade-insecure-requests", match.group(1))
