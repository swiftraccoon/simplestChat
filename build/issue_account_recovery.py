"""Initialize absent recovery keys for explicitly authorized account owners.

This is a privileged administrator operation on an explicitly prepared host;
see docs/deployment.md. It is never a public authentication or key replacement
endpoint. Keep the private output on every failure: a lost reply may follow a
successful commit. Never automatically retry an uncertain result.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import ipaddress
import json
import os
import re
import resource
import secrets
import stat
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, cast

if TYPE_CHECKING:
    from collections.abc import Sequence

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "ops/ansible/files"))

# isort: split
import bounded_process

SUCCESS = b"account_recovery_issued\n"
MAX_ACCOUNTS = 20
MAX_HOST_LENGTH = 253
MAX_HOST_LABEL = 63
MAX_EMAIL_BYTES = 255


class RecoveryError(RuntimeError):
    """A fixed diagnostic that contains no account or credential data."""


@dataclass(frozen=True)
class Options:
    """Explicit operator-selected destination and exact account identities."""

    host: str
    identity: Path
    container: str
    emails: tuple[str, ...]
    output: Path


def require(condition: object, code: str) -> None:
    """Reject an unsafe operation without including caller-controlled values."""
    if not condition:
        raise RecoveryError(code)


def private_owner(metadata: os.stat_result) -> bool:
    """Accept only objects owned by this caller with no group or world access."""
    return metadata.st_uid == os.geteuid() and metadata.st_mode & 0o077 == 0


def validate(options: Options) -> None:
    """Reject ambiguous targets and unsafe private destinations before generating keys."""
    try:
        _ = ipaddress.ip_address(options.host)
    except ValueError:
        require(
            len(options.host) <= MAX_HOST_LENGTH
            and re.fullmatch(r"[A-Za-z0-9](?:[A-Za-z0-9.-]*[A-Za-z0-9])?", options.host)
            and all(
                1 <= len(label) <= MAX_HOST_LABEL
                and not label.startswith("-")
                and not label.endswith("-")
                for label in options.host.split(".")
            ),
            "invalid_host",
        )
    require(re.fullmatch(r"[0-9a-f]{64}", options.container), "invalid_container")
    require(1 <= len(options.emails) <= MAX_ACCOUNTS, "invalid_account_count")
    require(len(set(options.emails)) == len(options.emails), "duplicate_account")
    for email in options.emails:
        require(
            len(email.encode("utf-8")) <= MAX_EMAIL_BYTES
            and email == email.lower()
            and re.fullmatch(r"[^\s@\x00-\x1f\x7f]+@[^\s@\x00-\x1f\x7f]+", email),
            "invalid_email",
        )
    require(options.identity.is_absolute(), "identity_must_be_absolute")
    identity = options.identity.lstat()
    require(stat.S_ISREG(identity.st_mode) and private_owner(identity), "unsafe_identity")
    require(options.output.is_absolute(), "output_must_be_absolute")
    parent = options.output.parent.lstat()
    require(stat.S_ISDIR(parent.st_mode) and private_owner(parent), "unsafe_output_parent")
    require(not os.path.lexists(options.output), "output_already_exists")


def recovery_pair() -> tuple[str, str]:
    """Use the application's sole recovery format and SHA-256 hash contract."""
    key = "sc-recovery-" + base64.urlsafe_b64encode(secrets.token_bytes(32)).decode().rstrip("=")
    return key, hashlib.sha256(key.encode("ascii")).hexdigest()


def save_keys(output: Path, accounts: list[dict[str, str]]) -> None:
    """Durably create a private file and directory entry before contacting the host."""
    directory = os.open(output.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        require(private_owner(os.fstat(directory)), "unsafe_output_parent")
        descriptor = os.open(
            output.name,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
            0o600,
            dir_fd=directory,
        )
        with os.fdopen(descriptor, "w", encoding="utf-8") as destination:
            os.fchmod(destination.fileno(), 0o600)
            json.dump({"accounts": accounts}, destination, ensure_ascii=True, indent=2)
            _ = destination.write("\n")
            destination.flush()
            os.fsync(destination.fileno())
        os.fsync(directory)
    finally:
        os.close(directory)


def transaction(entries: Sequence[tuple[str, str]]) -> bytes:
    """Lock exact accounts by stable ID and enforce all guards before committing."""
    payload = (
        json.dumps([{"email": email, "recovery_key_hash": digest} for email, digest in entries])
        .encode()
        .hex()
    )
    # Hex-encoded JSON keeps account text outside SQL and psql syntax. Only
    # digests enter the remote process; raw recovery keys stay on this machine.
    return (
        "BEGIN;\n"  # noqa: S608 -- The sole interpolated value is bytes.hex(), never SQL text.
        "SET LOCAL lock_timeout = '5s';\n"
        "SET LOCAL statement_timeout = '15s';\n"
        "SET LOCAL idle_in_transaction_session_timeout = '15s';\n"
        "CREATE TEMP TABLE recovery_requests (email text PRIMARY KEY, "
        "recovery_key_hash text NOT NULL) ON COMMIT DROP;\n"
        "INSERT INTO pg_temp.recovery_requests "
        "SELECT email, recovery_key_hash FROM jsonb_to_recordset("
        f"convert_from(decode('{payload}', 'hex'), 'UTF8')::jsonb) "
        "AS requested(email text, recovery_key_hash text);\n"
        "DO $recovery$\n"
        "DECLARE expected integer; locked integer; changed integer;\n"
        "BEGIN\n"
        "  SELECT count(*) INTO expected FROM pg_temp.recovery_requests;\n"
        "  IF expected = 0 THEN RAISE EXCEPTION 'recovery_guard_failed'; END IF;\n"
        "  PERFORM u.id FROM public.users AS u JOIN pg_temp.recovery_requests AS r "
        "ON u.email = r.email ORDER BY u.id FOR UPDATE OF u;\n"
        "  GET DIAGNOSTICS locked = ROW_COUNT;\n"
        "  IF locked <> expected THEN RAISE EXCEPTION 'recovery_guard_failed'; END IF;\n"
        "  IF EXISTS (SELECT 1 FROM public.users AS u JOIN pg_temp.recovery_requests AS r "
        "ON u.email = r.email WHERE u.recovery_key_hash IS NOT NULL) THEN\n"
        "    RAISE EXCEPTION 'recovery_guard_failed';\n"
        "  END IF;\n"
        "  UPDATE public.users AS u SET recovery_key_hash = r.recovery_key_hash, "
        "updated_at = now() FROM pg_temp.recovery_requests AS r "
        "WHERE u.email = r.email AND u.recovery_key_hash IS NULL;\n"
        "  GET DIAGNOSTICS changed = ROW_COUNT;\n"
        "  IF changed <> expected THEN RAISE EXCEPTION 'recovery_guard_failed'; END IF;\n"
        "END;\n"
        "$recovery$;\n"
        "COMMIT;\n"
        "SELECT 'account_recovery_issued';\n"
    ).encode("ascii")


def ssh_arguments(options: Options) -> list[str]:
    """Use one explicit identity, verified host keys and a fixed administrator command."""
    settings = (
        "BatchMode=yes",
        "StrictHostKeyChecking=yes",
        "IdentitiesOnly=yes",
        "ConnectTimeout=10",
        "ConnectionAttempts=1",
        "ControlMaster=no",
        "ControlPath=none",
        "ForwardAgent=no",
        "ForwardX11=no",
        "ClearAllForwardings=yes",
        "PermitLocalCommand=no",
        "ProxyCommand=none",
        "ProxyJump=none",
        "PasswordAuthentication=no",
        "KbdInteractiveAuthentication=no",
        "SendEnv=-*",
        "ServerAliveInterval=5",
        "ServerAliveCountMax=2",
    )
    return [
        "/usr/bin/ssh",
        "-F",
        "/dev/null",
        "-T",
        *[item for value in settings for item in ("-o", value)],
        "-i",
        str(options.identity),
        "-l",
        "root",
        options.host,
        "ulimit -c 0; exec /usr/bin/docker exec -i -u postgres "
        + options.container
        + " psql -X -qAt --set=ON_ERROR_STOP=1 -d simplestchat",
    ]


def issue(options: Options) -> None:
    """Save secrets first, then initialize every account atomically without retries."""
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    validate(options)
    accounts: list[dict[str, str]] = []
    entries: list[tuple[str, str]] = []
    for email in options.emails:
        key, digest = recovery_pair()
        accounts.append({"email": email, "recoveryKey": key})
        entries.append((email, digest))
    save_keys(options.output, accounts)
    environment = {
        key: os.environ[key]
        for key in ("HOME", "USER", "LOGNAME", "SSH_AUTH_SOCK")
        if key in os.environ
    }
    status, output, _ = bounded_process.run(
        ssh_arguments(options),
        input_data=transaction(entries),
        env=dict(environment, PATH="/usr/bin:/bin", LC_ALL="C"),
        limits=bounded_process.Limits(timeout=45, stdout=1024, stderr=4096),
    )
    require(status == 0 and output == SUCCESS, "recovery_result_unconfirmed_keep_private_file")


def main(argv: Sequence[str] | None = None) -> int:
    """Expose only fixed success or failure diagnostics; never print private output."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("--host", required=True)
    _ = parser.add_argument("--identity", required=True, type=Path)
    _ = parser.add_argument("--container", required=True)
    _ = parser.add_argument("--email", required=True, action="append")
    _ = parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args(argv)
    options = Options(
        host=cast("str", args.host),
        identity=cast("Path", args.identity),
        container=cast("str", args.container),
        emails=tuple(cast("list[str]", args.email)),
        output=cast("Path", args.output),
    )
    try:
        issue(options)
    except (OSError, ValueError, RecoveryError, bounded_process.ProcessError):
        _ = sys.stderr.write("account_recovery_failed_keep_private_file_no_automatic_retry\n")
        return 1
    _ = sys.stdout.write(SUCCESS.decode("ascii"))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
