"""Move a public deployment to one empty host while preserving its data and identity.

Both inventories and HTTPS origins must be explicit. The controller preserves
private evidence, never builds an image or sends chat, and never automatically
restarts the source once destination startup has been attempted.
"""

from __future__ import annotations

import argparse
import ipaddress
import json
import os
import re
import shutil
import signal
import stat
import tempfile
import time
from dataclasses import dataclass
from datetime import UTC, datetime
from html.parser import HTMLParser
from pathlib import Path
from typing import TYPE_CHECKING, override
from urllib.parse import urlsplit

import migration_artifact
import migration_dns
import release_attestation
import release_build as BUILD  # noqa: N812 -- Reuse the maintained release boundary.
import release_deploy as DEPLOY  # noqa: N812 -- Reuse exact inventory and CI validation.
import release_fetch_controller as FETCH  # noqa: N812 -- Reuse signed release selection.
from release_json import JsonObject, integer_value, object_value, string_value

if TYPE_CHECKING:
    from types import FrameType

ROOT = Path(__file__).resolve().parents[1]
MAX_HTTP_BODY = 128 * 1024
MAX_ASSET_BODY = 2 * 1024 * 1024
HTTP_SECONDS = 90
REQUEST_SECONDS = 15
ASSET_PATH = re.compile(r"/assets/index-[A-Za-z0-9_-]+\.js")
MAX_INSPECTION = 64 * 1024
MAX_HOSTNAME = 253
MAX_INVENTORY = 1024 * 1024
MAX_DNS_WAIT_SECONDS = 1800
PRIVATE_FILE_MODE = 0o600
HOSTNAME = re.compile(r"(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]{2,63}")
GET_PATHS = ("/health", "/ready", "/", "/api/capabilities")


class MigrationError(Exception):
    """A fixed diagnostic; raw inventory and command evidence remain private."""


def require(condition: object, code: str) -> None:
    """Reject unsafe or ambiguous migration inputs without exposing their values."""
    if not condition:
        raise MigrationError(code)


@dataclass
class MigrationOptions(DEPLOY.DeployOptions):
    """Explicit source and destination selectors with a bounded CI wait."""

    source_inventory: str = ""
    source_origin: str = ""
    source_limit: str | None = None
    activate_inventory: str | None = None
    update_canary: bool = False
    revision: str | None = None
    dns_wait_seconds: int = 600


def source_options(args: MigrationOptions) -> DEPLOY.DeployOptions:
    """Select the source using the same validated inventory contract as releases."""
    return DEPLOY.DeployOptions(
        inventory=args.source_inventory,
        repository=args.repository,
        origin=args.source_origin,
        limit=args.source_limit,
        wait_seconds=args.wait_seconds,
        ansible_playbook=args.ansible_playbook,
    )


def options(argv: list[str] | None = None) -> MigrationOptions:
    """Validate both exact targets before any controller or host operation."""
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("source-inventory", "source-origin", "inventory", "origin", "repository"):
        _ = parser.add_argument("--" + name, required=True)
    for name in ("source-limit", "limit"):
        _ = parser.add_argument("--" + name, help="Exact inventory hostname; no patterns")
    _ = parser.add_argument("--wait-seconds", type=int, default=3600)
    _ = parser.add_argument(
        "--revision", help="Exact ancestor application revision; defaults to HEAD"
    )
    _ = parser.add_argument(
        "--force",
        action="store_true",
        help="Explicitly migrate an unsigned retained release without waiting for CI",
    )
    _ = parser.add_argument(
        "--artifact-dir", help="With --force, an existing exact-revision release export"
    )
    _ = parser.add_argument(
        "--dns-wait-seconds",
        type=int,
        default=600,
        help="Same-hostname cutover DNS deadline (0-1800 seconds)",
    )
    _ = parser.add_argument("--ansible-playbook", help="Existing Ansible controller executable")
    _ = parser.add_argument(
        "--activate-inventory", help="Replace this existing private source inventory after cutover"
    )
    _ = parser.add_argument(
        "--update-canary", action="store_true", help="Move the existing CANARY_ORIGIN after cutover"
    )
    args = parser.parse_args(argv, namespace=MigrationOptions())
    validated: list[DEPLOY.DeployOptions] = []
    for selected in (source_options(args), args):
        values = [
            "--inventory",
            selected.inventory,
            "--origin",
            selected.origin,
            "--repository",
            args.repository,
            "--wait-seconds",
            str(args.wait_seconds),
        ]
        if selected.limit is not None:
            values.extend(("--limit", selected.limit))
        if args.ansible_playbook is not None:
            values.extend(("--ansible-playbook", args.ansible_playbook))
        validated.append(DEPLOY.options(values))
    source, target = validated
    args.source_inventory, args.source_origin = source.inventory, source.origin
    args.inventory, args.origin = target.inventory, target.origin
    for origin in (args.source_origin, args.origin):
        hostname = urlsplit(origin).hostname
        require(
            isinstance(hostname, str)
            and len(hostname) <= MAX_HOSTNAME
            and HOSTNAME.fullmatch(hostname),
            "migration_invalid_hostname",
        )
    require(
        args.revision is None or re.fullmatch(r"[a-f0-9]{40}", args.revision),
        "migration_invalid_revision",
    )
    require(0 <= args.dns_wait_seconds <= MAX_DNS_WAIT_SECONDS, "migration_invalid_dns_deadline")
    require(bool(args.force) == bool(args.artifact_dir), "migration_force_artifact_required")
    return args


def controller_tools(args: MigrationOptions, root: Path) -> tuple[str, str, str]:
    """Resolve existing Ansible and curl without requiring the message-writing smoke tool."""
    local = root / "ops/ansible/.venv/bin/ansible-playbook"
    selected = args.ansible_playbook or (
        str(local) if local.is_file() else shutil.which("ansible-playbook")
    )
    require(selected is not None, "ansible_not_found")
    playbook = Path(str(selected)).absolute()
    inventory = playbook.with_name("ansible-inventory")
    require(
        all(path.is_file() and os.access(path, os.X_OK) for path in (playbook, inventory)),
        "ansible_not_found",
    )
    curl = shutil.which("curl")
    require(curl is not None, "curl_not_found")
    return str(playbook), str(inventory), str(curl)


class ModuleAsset(HTMLParser):
    """Read the single Vite module declaration without following HTML-controlled URLs."""

    def __init__(self) -> None:
        """Retain only module script paths for the bounded homepage."""
        super().__init__(convert_charrefs=True)
        self.paths: list[str] = []

    @override
    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        """Reject ambiguous attributes and record exactly the declared module source."""
        if tag != "script":
            return
        attributes = dict(attrs)
        require(len(attributes) == len(attrs), "migration_ambiguous_script_attributes")
        if attributes.get("type") == "module":
            self.paths.append(attributes.get("src") or "")


def module_asset(homepage: str) -> str:
    """Accept one root-relative content-addressed module from the known Vite layout."""
    parser = ModuleAsset()
    parser.feed(homepage)
    parser.close()
    require(
        len(parser.paths) == 1 and ASSET_PATH.fullmatch(parser.paths[0]),
        "migration_frontend_asset_path",
    )
    return parser.paths[0]


def frontend_revision(script: str, revision: str) -> None:
    """Bind Vite's __APP_REVISION__ ClientTelemetry constructor, refusing changed layouts."""
    # web/vite.config.ts replaces __APP_REVISION__ in new ClientTelemetry(...).
    # This is a serving check, not a substitute for the signed image provenance.
    matches = re.findall(
        r"\bnew\s+[$A-Za-z_][$A-Za-z0-9_]*\(\s*([`'\"])(development|[a-f0-9]{40})\1\s*\)", script
    )
    require(len(matches) == 1 and matches[0][1] == revision, "migration_frontend_revision")


def verify_https(  # noqa: PLR0913 -- Explicit optional address retains the TLS hostname independently.
    runner: BUILD.RunnerProtocol,
    root: Path,
    curl: str,
    origin: str,
    revision: str,
    *,
    address: str | None = None,
) -> None:
    """Make at most five bounded anonymous GETs with one shared certificate-startup deadline."""
    require(runner.output is not None, "migration_https_private_evidence_required")
    directory = Path(tempfile.mkdtemp(prefix="https.", dir=runner.output))
    deadline = time.monotonic() + HTTP_SECONDS
    environment = {"PATH": os.defpath, "LC_ALL": "C"}
    requests = 0
    resolution: list[str] = []
    if address is not None:
        selected = ipaddress.ip_address(address)
        host = urlsplit(origin).hostname
        connect = f"[{selected}]" if isinstance(selected, ipaddress.IPv6Address) else str(selected)
        resolution = ["--resolve", f"{host}:443:{connect}"]

    def get(path: str, limit: int) -> str:
        nonlocal requests
        remaining = deadline - time.monotonic()
        require(remaining > 0, "migration_https_timeout")
        require(path in GET_PATHS or ASSET_PATH.fullmatch(path), "migration_https_path")
        requests += 1
        output = directory / f"response-{requests}.body"
        per_request = min(REQUEST_SECONDS, remaining)
        _, status = runner.run(
            [
                curl,
                "--disable",
                "--silent",
                "--show-error",
                "--fail",
                "--proto",
                "=https",
                "--tlsv1.2",
                "--noproxy",
                "*",
                "--connect-timeout",
                "5",
                "--max-time",
                str(per_request),
                "--retry",
                "30",
                "--retry-delay",
                "1",
                "--retry-max-time",
                str(max(1, int(remaining - per_request))),
                "--retry-all-errors",
                "--max-filesize",
                str(limit),
                "--request",
                "GET",
                "--header",
                "Accept-Encoding: identity",
                "--stderr",
                str(directory / f"response-{requests}.stderr"),
                "--output",
                str(output),
                "--write-out",
                "\n%{http_code}",
                *resolution,
                origin + path,
            ],
            cwd=root,
            env=environment,
            timeout=remaining,
        )
        # Curl keeps retry diagnostics separate from its final status and response body.
        require(status.rsplit("\n", 1)[-1] == "200", "migration_https_response")
        metadata = output.lstat()
        require(
            stat.S_ISREG(metadata.st_mode) and 0 < metadata.st_size <= limit,
            "migration_https_response",
        )
        return output.read_text(encoding="utf-8")

    asset = ""
    for path in GET_PATHS:
        body = get(path, MAX_HTTP_BODY)
        if path == "/":
            require("<!doctype html>" in body[:256].lower(), "migration_https_homepage")
            asset = module_asset(body)
        else:
            value = DEPLOY.object_record(FETCH.decoded(body), "migration_https_json")
            if path in ("/health", "/ready"):
                require(
                    value.get("status") == ("ok" if path == "/health" else "ready"),
                    "migration_https_readiness",
                )
    frontend_revision(get(asset, MAX_ASSET_BODY), revision)


def inspection(directory: Path, args: MigrationOptions) -> str:
    """Require distinct physical machines and preserve the actual source passkey identity."""
    path = directory / "inspection.json"
    metadata = path.lstat()
    require(
        stat.S_ISREG(metadata.st_mode) and metadata.st_size <= MAX_INSPECTION,
        "migration_inspection_file",
    )
    value = object_value(FETCH.decoded(path.read_bytes()))
    source, target = object_value(value["source"]), object_value(value["target"])
    source_domain = urlsplit(args.source_origin).hostname
    target_domain = urlsplit(args.origin).hostname
    source_id, target_id = source.get("machineId"), target.get("machineId")
    require(
        isinstance(source_id, str)
        and re.fullmatch(r"[a-f0-9]{32}", source_id)
        and isinstance(target_id, str)
        and re.fullmatch(r"[a-f0-9]{32}", target_id)
        and source_id != target_id,
        "migration_distinct_machines_required",
    )
    require(
        source.get("domain") == source_domain and target.get("domain") in (None, target_domain),
        "migration_remote_domain_mismatch",
    )
    require(
        isinstance(source.get("revision"), str)
        and re.fullmatch(r"[a-f0-9]{40}", string_value(source["revision"])),
        "migration_source_revision",
    )
    rp_id = source.get("rpId")
    require(
        isinstance(rp_id, str) and len(rp_id) <= MAX_HOSTNAME and HOSTNAME.fullmatch(rp_id),
        "migration_source_rp_id",
    )
    rp_id = string_value(rp_id)
    require(
        all(
            isinstance(domain, str) and (domain == rp_id or domain.endswith("." + rp_id))
            for domain in (source_domain, target_domain)
        ),
        "migration_passkey_domain_incompatible",
    )
    return rp_id


def private_inventory(path: Path) -> bytes:
    """Read a bounded, ordinary private file without following a final symlink."""
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as stream:
        metadata = os.fstat(stream.fileno())
        require(
            stat.S_ISREG(metadata.st_mode)
            and metadata.st_uid == os.getuid()
            and stat.S_IMODE(metadata.st_mode) == PRIVATE_FILE_MODE
            and 0 < metadata.st_size <= MAX_INVENTORY,
            "migration_activation_inventory_private_file_required",
        )
        data = stream.read(MAX_INVENTORY + 1)
    require(len(data) == metadata.st_size, "migration_activation_inventory_changed")
    return data


@dataclass(frozen=True)
class InventoryActivation:
    """The exact existing source bytes retained before any remote migration work."""

    path: Path
    original: bytes

    @classmethod
    def prepare(
        cls, args: MigrationOptions, root: Path, runner: BUILD.RunnerProtocol, directory: Path
    ) -> InventoryActivation | None:
        """Require an explicit ignored source inventory and retain its original bytes privately."""
        if args.activate_inventory is None:
            return None
        path = Path(args.activate_inventory).absolute()
        require(
            path.is_relative_to(root)
            and path == path.resolve()
            and path == Path(args.source_inventory),
            "migration_activation_requires_source_inventory",
        )
        status, ignored = runner.run(
            ["git", "check-ignore", "--", str(path)], cwd=root, allow_failure=True
        )
        require(status == 0 and bool(ignored), "migration_activation_inventory_must_be_ignored")
        original = private_inventory(path)
        descriptor = os.open(
            directory / "original-inventory", os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600
        )
        with os.fdopen(descriptor, "wb") as output:
            _ = output.write(original)
        return cls(path, original)

    def activate(self, value: JsonObject, report: JsonObject) -> None:
        """Atomically replace only unchanged source bytes, preserving private permissions."""
        require(
            private_inventory(self.path) == self.original, "migration_activation_inventory_changed"
        )
        descriptor, name = tempfile.mkstemp(prefix=".migration-inventory-", dir=self.path.parent)
        temporary = Path(name)
        try:
            with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
                json.dump(value, stream, indent=2, sort_keys=True)
                _ = stream.write("\n")
                stream.flush()
                os.fsync(stream.fileno())
            require(
                private_inventory(self.path) == self.original,
                "migration_activation_inventory_changed",
            )
            _ = temporary.replace(self.path)
            report["inventoryActivated"] = True
            parent = os.open(self.path.parent, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(parent)
            finally:
                os.close(parent)
        finally:
            temporary.unlink(missing_ok=True)


def canary_origin(repository: str, expected: str) -> None:
    """Read the existing exact GitHub variable without creating or dispatching anything."""
    value = DEPLOY.object_record(
        FETCH.api(f"repos/{repository}/actions/variables/CANARY_ORIGIN"),
        "migration_canary_variable_invalid",
    )
    require(
        value.get("name") == "CANARY_ORIGIN" and value.get("value") == expected,
        "migration_canary_origin_mismatch",
    )


def release_selection(  # noqa: PLR0913 -- Keep the controller and application identities explicit at this trust boundary.
    args: MigrationOptions,
    root: Path,
    directory: Path,
    report: JsonObject,
    *,
    tool_revision: str,
    revision: str,
) -> JsonObject:
    """Verify one signed release or an explicitly selected retained unsigned artifact."""
    if args.force:
        report.update(
            phase="force_artifact", githubAttested=False, ciVerification="skipped-explicit-force"
        )
        require(args.artifact_dir is not None, "migration_force_artifact_required")
        artifact, _ = migration_artifact.prepare(
            directory, root, tool_revision, revision, Path(str(args.artifact_dir)), args.repository
        )
        return {
            "scmig_force_release": True,
            "scmig_force_directory": str(artifact),
            "scmig_force_request": str(directory / "force.json"),
            "scmig_tool_revision": tool_revision,
        }
    report["phase"] = "ci"
    envelope = DEPLOY.select_ci_artifact(args, revision)
    report["phase"] = "attestation"
    selection = FETCH.ReleaseSelection(
        args.repository,
        integer_value(envelope["artifactId"]),
        revision,
        integer_value(envelope["ciRunId"]),
    )
    verified = directory / "verified"
    envelope = release_attestation.fetch_verify(FETCH.download_url(selection), verified, envelope)
    BUILD.write_json(directory / "selection.json", envelope)
    report.update(
        ciRunId=envelope["ciRunId"], artifactId=envelope["artifactId"], githubAttested=True
    )
    return {
        "scpub_release_artifact_id": envelope["artifactId"],
        "scpub_release_ci_run": envelope["ciRunId"],
        "scpub_release_verified_directory": str(verified),
    }


@dataclass
class Migration:
    """One transaction with durable startup boundaries and bounded recovery."""

    runner: BUILD.Runner
    combined: DEPLOY.PlaybookTarget
    target: DEPLOY.PlaybookTarget
    directory: Path
    extra: JsonObject
    report: JsonObject
    sequence: int = 0
    source_freeze_attempted: bool = False
    destination_startup_attempted: bool = False

    def checkpoint(self, phase: str) -> None:
        """Record intent before invoking any host operation, including ambiguous failures."""
        self.sequence += 1
        self.report.update(
            phase=phase,
            sourceFreezeAttempted=self.source_freeze_attempted,
            destinationStartupAttempted=self.destination_startup_attempted,
        )
        BUILD.write_json(self.directory / f"phase-{self.sequence:02d}.json", self.report)

    def play(
        self, name: str, *, action: str | None = None, extra: JsonObject | None = None
    ) -> None:
        """Use frozen combined targets only for the maintained migration playbooks."""
        variables = {**self.extra, **(extra or {})}
        if action is not None:
            variables["scmig_action"] = action
        self.checkpoint(action or name.removesuffix(".yml"))
        target = self.combined if name.startswith("migration-") else self.target
        DEPLOY.run_playbook(self.runner, target, name, variables)

    def recover(self) -> None:
        """Resume the source only after proving the destination stopped before startup."""
        if not self.source_freeze_attempted:
            self.report["recovery"] = "source_untouched"
            return
        if self.destination_startup_attempted:
            self.report["recovery"] = "manual_destination_recovery_source_must_remain_frozen"
            return
        self.report["recovery"] = "target_stop_required_before_source_resume"
        self.play("migration-data.yml", action="stop-target")
        self.report["recovery"] = "source_resume_pending"
        self.play("migration-data.yml", action="resume-source")
        self.report["recovery"] = "source_resumed_target_stopped"

    def perform(self, args: MigrationOptions, curl: str, services: dict[str, bool]) -> None:
        """Prepare while source stays live, transfer while frozen, then start the destination."""
        self.play("migration-prepare.yml", action="inspect")
        self.extra["scpub_webauthn_rp_id"] = inspection(self.directory, args)
        BUILD.write_json(self.directory / "variables.json", self.extra)
        self.play("site.yml")
        self.play("migration-prepare.yml", action="bootstrap")
        self.play("migration-prepare.yml", action="stage")
        self.play("public.yml", extra={"scpub_turn_enabled": False})
        self.source_freeze_attempted = True
        self.play("migration-data.yml", action="transfer")
        # The command can start the destination before its transport returns.
        # A failure or interrupt from this point therefore never resumes source.
        self.destination_startup_attempted = True
        self.play("migration-data.yml", action="launch")
        self.report["destinationStarted"] = True
        self.checkpoint("https")
        verify_https(
            self.runner,
            self.target.root,
            curl,
            args.origin,
            string_value(self.extra["scmig_revision"]),
            address=string_value(self.extra["scmig_destination_address"])
            if args.source_origin == args.origin
            else None,
        )
        if services["scpub_turn_enabled"]:
            self.play("turn.yml", extra={"scpub_turn_activate": False})
            self.play("turn.yml", extra={"scpub_turn_activate": True})
        for enabled, name in (
            ("scpub_backup_enabled", "backup.yml"),
            ("scmon_enabled", "monitoring.yml"),
        ):
            if services[enabled]:
                self.play(name)
        self.play("migration-data.yml", action="finalize-source")
        if args.source_origin == args.origin:
            self.checkpoint("awaiting_dns_cutover")
            migration_dns.wait_for_cutover(
                args.origin,
                string_value(self.extra["scmig_destination_address"]),
                string_value(self.extra.get("scmig_destination_ipv6", "")),
                args.dns_wait_seconds,
            )
            self.checkpoint("public_https")
            verify_https(
                self.runner,
                self.target.root,
                curl,
                args.origin,
                string_value(self.extra["scmig_revision"]),
            )
            self.report["dnsCutoverVerified"] = True
        self.report.update(recovery="source_retained_stopped", phase="destination_ready")

    def activate_selectors(
        self,
        args: MigrationOptions,
        activation: InventoryActivation | None,
        original_target: tuple[str, JsonObject],
    ) -> None:
        """Explicitly move operator selectors only after the destination and services are ready."""
        if activation is not None:
            self.checkpoint("activate_inventory")
            alias, hostvars = original_target
            selected: JsonObject = {
                **hostvars,
                "scbench_revision": self.extra["scmig_revision"],
                "scbench_repository": self.extra["scbench_repository"],
                "scpub_release_revision": self.extra["scmig_revision"],
                "scpub_webauthn_rp_id": self.extra["scpub_webauthn_rp_id"],
                "scpub_release_deploy": True,
            }
            activation.activate({"benchmark_hosts": {"hosts": {alias: selected}}}, self.report)
        if args.update_canary:
            self.checkpoint("update_canary")
            canary_origin(args.repository, args.source_origin)
            self.report["canaryUpdateAttempted"] = True
            self.checkpoint("update_canary")
            _ = self.runner.run(
                [
                    "gh",
                    "variable",
                    "set",
                    "CANARY_ORIGIN",
                    "--repo",
                    args.repository,
                    "--body",
                    args.origin,
                ],
                cwd=self.target.root,
                env={**os.environ, "GH_HOST": "github.com", "GH_PROMPT_DISABLED": "1"},
                timeout=30,
            )
            canary_origin(args.repository, args.origin)
            self.report["canaryUpdated"] = True
        self.report.update(passed=True, phase="complete")


def frozen_targets(  # noqa: PLR0913 -- Both independently selected hosts share one frozen transaction.
    directory: Path,
    root: Path,
    playbook: str,
    environment: dict[str, str],
    *,
    source: JsonObject,
    target: JsonObject,
) -> tuple[DEPLOY.PlaybookTarget, DEPLOY.PlaybookTarget]:
    """Freeze private single-host inventories and a two-host migration inventory."""
    values: dict[str, JsonObject] = {
        "source": {"migration_source": source},
        "target": {"migration_target": target},
        "migration": {"migration_source": source, "migration_target": target},
    }
    for name, hosts in values.items():
        BUILD.write_json(
            directory / f"{name}-inventory.json", {"benchmark_hosts": {"hosts": hosts}}
        )
    return (
        DEPLOY.PlaybookTarget(
            playbook=playbook,
            inventory=directory / "migration-inventory.json",
            host="migration_source:migration_target",
            root=root,
            environment=environment,
        ),
        DEPLOY.PlaybookTarget(
            playbook=playbook,
            inventory=directory / "target-inventory.json",
            host="migration_target",
            root=root,
            environment=environment,
        ),
    )


def target_transport(args: MigrationOptions, source: JsonObject, target: JsonObject) -> JsonObject:
    """Select IP-directed probes only when the public hostname remains unchanged."""
    if args.source_origin != args.origin:
        return {}
    migration_dns.validate_addresses(source, target)
    return {
        "scmig_destination_address": target["ansible_host"],
        "scmig_destination_ipv6": target.get("scpub_announce_ipv6", ""),
        "scpub_turn_verify_address": target["ansible_host"],
    }


def release_identity(
    runner: BUILD.RunnerProtocol, args: MigrationOptions, root: Path
) -> tuple[str, str]:
    """Require published clean main tools and an explicitly selected ancestor application."""
    tool_revision = DEPLOY.checkout_identity(runner, root, args.repository)
    _, branch = runner.run(["git", "symbolic-ref", "--short", "HEAD"], cwd=root)
    require(branch == "main", "migration_main_checkout_required")
    _, published = runner.run(
        ["git", "ls-remote", "--exit-code", "origin", "refs/heads/main"], cwd=root
    )
    require(published == tool_revision + "\trefs/heads/main", "migration_publish_tools_required")
    revision = args.revision or tool_revision
    require(re.fullmatch(r"[a-f0-9]{40}", revision), "migration_invalid_revision")
    if revision != tool_revision:
        status, _ = runner.run(
            ["git", "merge-base", "--is-ancestor", revision, tool_revision],
            cwd=root,
            allow_failure=True,
        )
        require(status == 0, "migration_release_not_tool_ancestor")
    return tool_revision, revision


def execute(args: MigrationOptions, root: Path = ROOT) -> JsonObject:  # noqa: PLR0915 -- Keep trust acquisition and one recovery boundary explicit.
    """Verify a clean signed main release before any remote writes, then migrate once."""
    report: JsonObject = {
        "schemaVersion": 1,
        "operation": "migrate",
        "passed": False,
        "phase": "preflight",
        "sourceOrigin": args.source_origin,
        "destinationOrigin": args.origin,
        "sourceFreezeAttempted": False,
        "destinationStartupAttempted": False,
        "destinationStarted": False,
        "inventoryActivated": False,
        "inventoryActivationRequested": args.activate_inventory is not None,
        "canaryUpdateAttempted": False,
        "canaryUpdated": False,
        "canaryUpdateRequested": args.update_canary,
        "recovery": "source_untouched",
        "failureClass": None,
        "startedAt": datetime.now(UTC).isoformat(),
    }
    directory: Path | None = None
    migration: Migration | None = None
    started = time.monotonic()
    try:
        inspector = BUILD.Runner()
        tool_revision, revision = release_identity(inspector, args, root)
        report.update(
            repository=args.repository,
            revision=revision,
            toolRevision=tool_revision,
            forced=args.force,
        )
        require(bool(args.force) == bool(args.artifact_dir), "migration_force_artifact_required")
        playbook, inventory_tool, curl = controller_tools(args, root)
        environment = DEPLOY.ansible_environment(root)
        source_args = source_options(args)
        source = DEPLOY.selected_host(source_args, inventory_tool, root, environment)
        target = DEPLOY.selected_host(args, inventory_tool, root, environment)
        source_address, target_address = (
            source[1].get("ansible_host"),
            target[1].get("ansible_host"),
        )
        require(
            isinstance(source_address, str)
            and isinstance(target_address, str)
            and source_address.casefold().rstrip(".") != target_address.casefold().rstrip("."),
            "migration_distinct_hosts_required",
        )
        transport = target_transport(args, source[1], target[1])
        services: dict[str, bool] = {}
        for name in ("scpub_turn_enabled", "scpub_backup_enabled", "scmon_enabled"):
            value = target[1].get(name, False)
            require(type(value) is bool, "migration_service_opt_in_invalid")
            services[name] = bool(value)
        _, ignored = inspector.run(
            ["git", "check-ignore", "--", str(root / "results/migration.evidence")], cwd=root
        )
        require(bool(ignored), "results_must_be_git_ignored")
        parent = root / "results"
        require(not parent.is_symlink(), "invalid_evidence_parent")
        parent.mkdir(mode=0o700, exist_ok=True)
        directory = Path(tempfile.mkdtemp(prefix="migration.", dir=parent))
        report["evidence"] = str(directory)
        runner = BUILD.Runner(directory)
        activation = InventoryActivation.prepare(args, root, inspector, directory)
        if activation is not None:
            report["activationInventory"] = str(activation.path)
        if args.update_canary:
            canary_origin(args.repository, args.source_origin)
        release_variables = release_selection(
            args, root, directory, report, tool_revision=tool_revision, revision=revision
        )
        require(
            DEPLOY.selected_host(source_args, inventory_tool, root, environment) == source
            and DEPLOY.selected_host(args, inventory_tool, root, environment) == target,
            "inventory_target_changed",
        )
        require(
            DEPLOY.checkout_identity(inspector, root, args.repository) == tool_revision,
            "checkout_changed",
        )
        combined, destination = frozen_targets(
            directory, root, playbook, environment, source=source[1], target=target[1]
        )
        run_id = os.urandom(16).hex()
        report["runId"] = run_id
        extra: JsonObject = {
            "scmig_run_id": run_id,
            "scmig_evidence": str(directory),
            "scmig_source_origin": args.source_origin,
            "scmig_destination_origin": args.origin,
            "scmig_revision": revision,
            "scbench_revision": revision,
            "scbench_repository": f"https://github.com/{args.repository}.git",
            "scpub_release_revision": revision,
            "scpub_release_repository": args.repository,
            "scpub_release_expected_revision": revision,
            **release_variables,
            **transport,
        }
        migration = Migration(runner, combined, destination, directory, extra, report)
        migration.perform(args, curl, services)
        migration.activate_selectors(args, activation, target)
    except (Exception, KeyboardInterrupt) as error:  # noqa: BLE001 -- Retain redacted failure and bounded recovery evidence.
        report["passed"] = False
        report["failureClass"] = (
            str(error)
            if isinstance(
                error,
                (MigrationError, migration_dns.CutoverError, DEPLOY.DeployError, FETCH.FetchError),
            )
            else "migration_step_failed"
        )
        report["failedPhase"] = report["phase"]
        if migration is not None:
            try:
                migration.recover()
            except (Exception, KeyboardInterrupt):  # noqa: BLE001 -- A failed target stop must never fall through to source restart.
                report["recovery"] = "manual_recovery_required"
        report["phase"] = "failed"
    finally:
        report["elapsedSeconds"] = round(time.monotonic() - started, 3)
        if directory is not None:
            try:
                BUILD.write_json(directory / "outcome.json", report)
            except OSError:
                report.update(passed=False, failureClass="outcome_write_failed")
    return report


def main(argv: list[str] | None = None) -> int:
    """Run the bounded migration and print only its redacted outcome."""
    try:
        args = options(argv)
    except SystemExit as error:
        return error.code if isinstance(error.code, int) else 1
    except (MigrationError, DEPLOY.DeployError, OSError, ValueError):
        print(json.dumps({"passed": False, "phase": "options", "failureClass": "invalid_options"}))  # noqa: T201 -- Intentional redacted CLI result.
        return 1

    def interrupted(_signum: int, _frame: FrameType | None) -> None:
        message = "interrupted"
        raise MigrationError(message)

    previous = {
        number: signal.signal(number, interrupted) for number in (signal.SIGINT, signal.SIGTERM)
    }
    previous_umask = os.umask(0o077)
    try:
        report = execute(args)
    finally:
        _ = os.umask(previous_umask)
        for number, handler in previous.items():
            _ = signal.signal(number, handler)
    print(json.dumps(report), flush=True)  # noqa: T201 -- Intentional redacted CLI result.
    return 0 if report["passed"] else 1
