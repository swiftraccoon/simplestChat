"""Bound the DNS handoff after a same-hostname destination has been verified."""

from __future__ import annotations

import ipaddress
import json
import re
import shutil
import subprocess
import time
from urllib.parse import urlsplit

import bounded_process
from release_json import JsonObject, string_value

MAX_WAIT_SECONDS = 1800
QUERY_SECONDS = 5
MAX_DNS_BYTES = 65536
MAX_DNS_NAME = 253
DNS_NAME = re.compile(r"(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]{2,63}", re.IGNORECASE)


class CutoverError(ValueError):
    """A fixed, non-secret diagnostic from migration address selection."""


def address(value: str) -> str:
    """Require a plain, usable IP literal rather than a mutable hostname."""
    try:
        selected = ipaddress.ip_address(value)
    except ValueError:
        message = "migration_literal_addresses_required"
        raise CutoverError(message) from None
    if isinstance(selected, ipaddress.IPv6Address) and selected.ipv4_mapped is not None:
        selected = selected.ipv4_mapped
    if (
        selected.is_loopback
        or selected.is_unspecified
        or selected.is_multicast
        or selected.is_link_local
        or "%" in value
    ):
        message = "migration_unusable_address"
        raise CutoverError(message)
    return str(selected)


def validate_addresses(source: JsonObject, target: JsonObject) -> None:
    """Keep SSH host identity and advertised media identity independent of DNS."""
    old = address(string_value(source.get("ansible_host")))
    new = address(string_value(target.get("ansible_host")))
    if old == new:
        message = "migration_distinct_hosts_required"
        raise CutoverError(message)
    if address(string_value(target.get("scpub_announce_ip"))) != new:
        message = "migration_target_media_address_mismatch"
        raise CutoverError(message)
    ipv6 = string_value(target.get("scpub_announce_ipv6", ""))
    if ipv6 and not isinstance(ipaddress.ip_address(address(ipv6)), ipaddress.IPv6Address):
        message = "migration_invalid_ipv6_address"
        raise CutoverError(message)
    if shutil.which("dig") is None:
        message = "migration_dns_tool_missing"
        raise CutoverError(message)


def resolved(dig: str, domain: str, deadline: float) -> set[str]:
    """Read both address families with bounded DNS subprocesses and no shell."""
    if len(domain) > MAX_DNS_NAME or DNS_NAME.fullmatch(domain) is None:
        message = "migration_invalid_dns_name"
        raise CutoverError(message)
    result: set[str] = set()
    for family in ("A", "AAAA"):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            message = "migration_dns_observation_timeout"
            raise bounded_process.ProcessError(message)
        status, output, _error = bounded_process.run(
            [dig, "+short", "+timeout=2", "+tries=1", domain, family],
            limits=bounded_process.Limits(
                timeout=min(QUERY_SECONDS, remaining), stdout=MAX_DNS_BYTES, stderr=4096
            ),
        )
        if status != 0:
            message = "migration_dns_query_failed"
            raise bounded_process.ProcessError(message)
        try:
            lines = output.decode("ascii").splitlines()
        except UnicodeDecodeError:
            message = "migration_dns_response_invalid"
            raise CutoverError(message) from None
        saw_address = False
        for value in lines:
            try:
                parsed = ipaddress.ip_address(value)
            except ValueError:
                if (
                    not saw_address
                    and value.endswith(".")
                    and len(value) <= MAX_DNS_NAME + 1
                    and DNS_NAME.fullmatch(value[:-1])
                ):
                    continue  # A valid CNAME may precede the resolved address records.
                message = "migration_dns_response_invalid"
                raise CutoverError(message) from None
            if (family == "A") != isinstance(parsed, ipaddress.IPv4Address):
                message = "migration_dns_address_family_mismatch"
                raise CutoverError(message)
            saw_address = True
            result.add(str(parsed))
    return result


def wait_for_cutover(origin: str, ipv4: str, ipv6: str, seconds: int) -> None:
    """Wait for only the verified destination addresses; never change DNS implicitly."""
    domain = urlsplit(origin).hostname
    dig = shutil.which("dig")
    if (
        domain is None
        or origin != "https://" + domain
        or dig is None
        or not 0 <= seconds <= MAX_WAIT_SECONDS
    ):
        message = "migration_invalid_dns_cutover"
        raise CutoverError(message)
    expected: set[str] = {address(ipv4)}
    if ipv6:
        additional = address(ipv6)
        if not isinstance(ipaddress.ip_address(additional), ipaddress.IPv6Address):
            message = "migration_invalid_ipv6_address"
            raise CutoverError(message)
        expected.add(additional)
    print(  # noqa: T201 -- Tell the operator exactly when the prepared destination is ready for DNS.
        json.dumps(
            {
                "phase": "awaiting_dns_cutover",
                "domain": domain,
                "addresses": sorted(expected),
                "waitSeconds": seconds,
            }
        ),
        flush=True,
    )
    deadline = time.monotonic() + (seconds or 2 * QUERY_SECONDS)
    while True:
        try:
            if resolved(dig, domain, deadline) == expected:
                return
        except (OSError, subprocess.SubprocessError, bounded_process.ProcessError):
            pass
        remaining = deadline - time.monotonic()
        if seconds == 0 or remaining <= 0:
            message = "migration_dns_cutover_timeout_source_remains_sealed"
            raise CutoverError(message)
        time.sleep(min(2, remaining))
