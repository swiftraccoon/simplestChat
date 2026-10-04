"""Offline cutover observations never change DNS or recover a sealed source."""

from __future__ import annotations

import contextlib
import io
import shutil
import time
import unittest
from typing import TYPE_CHECKING
from unittest.mock import call, patch

from test_support import ROOT

# isort: split
import bounded_process
import migration_dns as dns

if TYPE_CHECKING:
    from release_json import JsonObject

SOURCE = "198.51.100.10"
TARGET = "198.51.100.20"
IPV6 = "2001:db8::20"
ORIGIN = "https://chat.example.test"
DIG = str(ROOT / "results" / "fixture-dig")


class MigrationDnsTests(unittest.TestCase):
    """Bind both families, exact target addresses and total waiting to explicit inputs."""

    def test_addresses_are_literal_distinct_and_match_media(self) -> None:
        """Mutable names, unusable addresses and alternate spellings cannot select a host."""
        source: JsonObject = {"ansible_host": SOURCE}
        target: JsonObject = {"ansible_host": TARGET, "scpub_announce_ip": TARGET}
        with patch.object(shutil, "which", return_value=DIG):
            dns.validate_addresses(source, target)
            dns.validate_addresses(source, {**target, "scpub_announce_ipv6": IPV6})
            for changed in (
                {**target, "ansible_host": "chat.example.test"},
                {**target, "ansible_host": SOURCE},
                {**target, "ansible_host": "::ffff:" + SOURCE},
                {**target, "scpub_announce_ip": SOURCE},
                {**target, "scpub_announce_ipv6": TARGET},
                {**target, "scpub_announce_ipv6": "::ffff:" + TARGET},
            ):
                with self.subTest(target=changed), self.assertRaises(dns.CutoverError):
                    dns.validate_addresses(source, changed)
        for value in (
            "localhost",
            "127.0.0.1",
            "::ffff:127.0.0.1",
            "0.0.0.0",  # noqa: S104 -- An input rejected by validation, never a listener.
            "::",
            "224.0.0.1",
            "fe80::1",
            "2001:db8::1%zone",
        ):
            with self.subTest(value=value), self.assertRaises(dns.CutoverError):
                _ = dns.address(value)
        self.assertEqual(dns.address("::ffff:" + TARGET), TARGET)
        with (
            patch.object(shutil, "which", return_value=None),
            self.assertRaisesRegex(dns.CutoverError, "migration_dns_tool_missing"),
        ):
            dns.validate_addresses(source, target)

    def test_queries_are_bounded_and_accept_cnames_before_each_family(self) -> None:
        """CNAME chains are names, while all resulting A and AAAA addresses remain visible."""
        with (
            patch.object(time, "monotonic", side_effect=[10, 11]),
            patch.object(
                bounded_process,
                "run",
                side_effect=[
                    (0, b"alias.example.test.\n" + TARGET.encode() + b"\n", b""),
                    (0, b"alias.example.test.\n" + IPV6.encode() + b"\n", b""),
                ],
            ) as command,
        ):
            self.assertEqual(dns.resolved(DIG, "chat.example.test", 14), {TARGET, IPV6})
        self.assertEqual(
            command.call_args_list,
            [
                call(
                    [DIG, "+short", "+timeout=2", "+tries=1", "chat.example.test", family],
                    limits=bounded_process.Limits(timeout=timeout, stdout=65536, stderr=4096),
                )
                for family, timeout in (("A", 4), ("AAAA", 3))
            ],
        )

    def test_unknown_response_syntax_and_wrong_family_fail_closed(self) -> None:
        """Ignoring malformed output must never turn an ambiguous answer into success."""
        for value in (
            b"PRIVATE_FIXTURE_DETAIL\n",
            b"alias.example.test\n",
            b"; parse error\n",
            b"\xff\n",
            IPV6.encode() + b"\n",
            TARGET.encode() + b"\nalias.example.test.\n",
        ):
            with (
                self.subTest(value=value),
                patch.object(time, "monotonic", return_value=0),
                patch.object(bounded_process, "run", return_value=(0, value, b"")),
                self.assertRaises(dns.CutoverError) as failed,
            ):
                _ = dns.resolved(DIG, "chat.example.test", 10)
            self.assertNotIn("PRIVATE_FIXTURE_DETAIL", str(failed.exception))
            self.assertTrue(str(failed.exception).startswith("migration_dns_"))

    def test_zero_wait_observes_both_families_once_without_sleep(self) -> None:
        """Zero wait still permits a bounded observation and refuses stale IPv6 immediately."""
        for stale_ipv6 in (False, True):
            answers = [
                (0, TARGET.encode() + b"\n", b""),
                (0, (IPV6 + "\n").encode() if stale_ipv6 else b"", b""),
            ]
            with (
                self.subTest(stale_ipv6=stale_ipv6),
                patch.object(shutil, "which", return_value=DIG),
                patch.object(time, "monotonic", return_value=0),
                patch.object(time, "sleep") as sleep,
                patch.object(bounded_process, "run", side_effect=answers) as command,
                contextlib.redirect_stdout(io.StringIO()),
            ):
                if stale_ipv6:
                    with self.assertRaisesRegex(dns.CutoverError, "timeout_source_remains_sealed"):
                        dns.wait_for_cutover(ORIGIN, TARGET, "", 0)
                else:
                    dns.wait_for_cutover(ORIGIN, TARGET, "", 0)
                self.assertEqual(
                    command.call_args_list,
                    [
                        call(
                            [DIG, "+short", "+timeout=2", "+tries=1", "chat.example.test", family],
                            limits=bounded_process.Limits(timeout=5, stdout=65536, stderr=4096),
                        )
                        for family in ("A", "AAAA")
                    ],
                )
                sleep.assert_not_called()

    def test_timeout_is_redacted_and_has_no_recovery_commands(self) -> None:
        """Resolver failure raises a sealed-source diagnostic; the sole subprocess is dig."""
        with (
            patch.object(shutil, "which", return_value=DIG),
            patch.object(time, "monotonic", side_effect=[0, 0, 2]),
            patch.object(time, "sleep") as sleep,
            patch.object(
                bounded_process,
                "run",
                side_effect=bounded_process.ProcessError("PRIVATE_FIXTURE_DETAIL"),
            ) as command,
            contextlib.redirect_stdout(io.StringIO()),
            self.assertRaises(dns.CutoverError) as failed,
        ):
            dns.wait_for_cutover(ORIGIN, TARGET, "", 1)
        self.assertEqual(
            str(failed.exception), "migration_dns_cutover_timeout_source_remains_sealed"
        )
        command.assert_called_once_with(
            [DIG, "+short", "+timeout=2", "+tries=1", "chat.example.test", "A"],
            limits=bounded_process.Limits(timeout=1, stdout=65536, stderr=4096),
        )
        sleep.assert_not_called()

    def test_polling_requires_exact_addresses_and_is_bounded_by_deadline(self) -> None:
        """A stale address in either family keeps the cutover pending until it disappears."""
        with (
            patch.object(shutil, "which", return_value=DIG),
            patch.object(time, "monotonic", side_effect=[0, 1]),
            patch.object(time, "sleep") as sleep,
            patch.object(
                dns, "resolved", side_effect=[{SOURCE, TARGET, IPV6}, {TARGET, IPV6}]
            ) as resolved,
            contextlib.redirect_stdout(io.StringIO()),
        ):
            dns.wait_for_cutover(ORIGIN, TARGET, IPV6, 5)
        sleep.assert_called_once_with(2)
        self.assertEqual(resolved.call_args_list, [call(DIG, "chat.example.test", 5)] * 2)

    def test_invalid_origin_or_wait_never_queries_dns(self) -> None:
        """The observer cannot accept a different URL transport or command-like DNS name."""
        for origin, seconds in (
            ("http://chat.example.test", 0),
            (ORIGIN + "/", 0),
            ("https://user@chat.example.test", 0),
            (ORIGIN, -1),
            (ORIGIN, 1801),
            ("https://-trace.example.test", 0),
        ):
            with (
                self.subTest(origin=origin, seconds=seconds),
                patch.object(shutil, "which", return_value=DIG),
                patch.object(bounded_process, "run") as command,
                contextlib.redirect_stdout(io.StringIO()),
                self.assertRaises(dns.CutoverError),
            ):
                dns.wait_for_cutover(origin, TARGET, "", seconds)
            command.assert_not_called()


if __name__ == "__main__":
    _ = unittest.main()
