"""Deploy clean HEAD and verify it with anonymous HTTPS requests.

Requires Python 3.12+, Ansible, curl and trusted SSH access on the controller.
Signed CI artifacts also require authenticated GitHub CLI; --force admits an
off-host build without waiting for CI or requiring its attestation. Chat-writing
verification requires --public-chat-smoke. Runtime release safeguards remain.
"""

# Failure codes are intentionally fixed literals at validation boundaries.
# ruff: noqa: EM101

import argparse
import json
import os
import re
import shutil
import signal
import tempfile
import time
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from types import FrameType
from urllib.parse import urlsplit

import force_deploy
import release_attestation
import release_build as BUILD  # noqa: N812 -- Keep established helper aliases.
import release_fetch_controller as FETCH  # noqa: N812 -- Keep established helper aliases.
from release_json import JsonObject, JsonValue, array_value, integer_value

ROOT = Path(__file__).resolve().parents[1]
MAX_CI_WAIT_SECONDS = 7200
MAX_QUIET_SECONDS = 600


class DeployError(Exception):
    """Fixed failure codes only; raw inventory or subprocess output stays private."""


def require(condition: object, code: str) -> None:
    """Reject an unmet invariant with its stable, non-sensitive failure code."""
    if not condition:
        raise DeployError(code)


@dataclass
class DeployOptions(argparse.Namespace):
    """Explicit controller, target, CI wait and public verification selectors."""

    inventory: str = ""
    repository: str = ""
    origin: str = ""
    limit: str | None = None
    room: str = "lobby"
    wait_seconds: int = 3600
    quiet_seconds: int = 600
    install_helpers: bool = False
    maintenance: bool = False
    ansible_playbook: str | None = None
    force: bool = False
    artifact_dir: str | None = None
    public_chat_smoke: bool = False


def object_record(value: JsonValue, code: str) -> JsonObject:
    """Validate an object without exposing raw metadata in an error."""
    if not isinstance(value, dict):
        raise DeployError(code)
    return value


def options(argv: list[str] | None = None) -> DeployOptions:
    """Parse and validate explicit command-line selectors before side effects."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("--inventory", required=True)
    _ = parser.add_argument("--repository", required=True)
    _ = parser.add_argument("--origin", required=True)
    _ = parser.add_argument("--limit", help="Exact inventory hostname; patterns are not supported")
    _ = parser.add_argument("--room", default="lobby")
    _ = parser.add_argument(
        "--force",
        action="store_true",
        help="Explicitly deploy an unsigned off-host build without waiting for CI",
    )
    _ = parser.add_argument(
        "--artifact-dir", help="With --force, reuse an exact-HEAD local build/export directory"
    )
    _ = parser.add_argument(
        "--public-chat-smoke",
        action="store_true",
        help="Explicitly join guests and write one public test chat message",
    )
    _ = parser.add_argument("--wait-seconds", type=int, default=3600)
    _ = parser.add_argument(
        "--quiet-seconds",
        type=int,
        default=MAX_QUIET_SECONDS,
        help="wait up to this long for zero active rooms before the app is replaced (0-600)",
    )
    _ = parser.add_argument(
        "--install-helpers",
        action="store_true",
        help=(
            "Explicitly reconcile release helpers "
            + "instead of requiring their exact installed hashes"
        ),
    )
    _ = parser.add_argument(
        "--maintenance",
        action="store_true",
        help=(
            "Stage, then replace the app through the maintenance launcher "
            + "(new migrations and grants; the app and proxy stop for the duration)"
        ),
    )
    _ = parser.add_argument(
        "--ansible-playbook", help="Controller executable; defaults to local .venv, then PATH"
    )
    args = parser.parse_args(argv, namespace=DeployOptions())
    require(args.force or args.artifact_dir is None, "artifact_directory_requires_force")
    if args.artifact_dir is not None:
        artifact = Path(args.artifact_dir).expanduser().absolute()
        require(
            not artifact.is_symlink() and artifact.is_dir() and artifact.resolve() == artifact,
            "invalid_artifact_directory",
        )
        args.artifact_dir = str(artifact)
    require(
        re.fullmatch(
            r"[A-Za-z0-9][A-Za-z0-9-]{0,38}/[A-Za-z0-9][A-Za-z0-9_.-]{0,99}", args.repository
        ),
        "invalid_repository",
    )
    require(
        args.limit is None or re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,252}", args.limit),
        "invalid_limit",
    )
    require(re.fullmatch(r"[A-Za-z0-9_-]{1,128}", args.room), "invalid_room")
    require(0 <= args.wait_seconds <= MAX_CI_WAIT_SECONDS, "invalid_wait_seconds")
    require(0 <= args.quiet_seconds <= MAX_QUIET_SECONDS, "invalid_quiet_seconds")
    origin = urlsplit(args.origin)
    require(
        origin.scheme == "https"
        and origin.hostname
        and not origin.username
        and not origin.password
        and origin.path in ("", "/")
        and not origin.query
        and not origin.fragment
        and origin.port in (None, 443)
        and re.fullmatch(r"https://[a-z0-9.-]+(?::443)?/?", args.origin),
        "invalid_origin",
    )
    if origin.hostname is None:
        raise DeployError("invalid_origin")
    args.origin = "https://" + origin.hostname
    args.inventory = str(Path(args.inventory).expanduser().resolve(strict=True))
    require(Path(args.inventory).is_file(), "invalid_inventory")
    return args


def controller_tools(args: DeployOptions, root: Path) -> tuple[str, str, str]:
    """Resolve required controller executables without installing or changing them."""
    local = root / "ops/ansible/.venv/bin/ansible-playbook"
    playbook = args.ansible_playbook or (
        str(local) if local.is_file() else shutil.which("ansible-playbook")
    )
    if playbook is None:
        raise DeployError("ansible_not_found")
    playbook = Path(playbook).absolute()
    inventory = playbook.with_name("ansible-inventory")
    require(
        playbook.is_file()
        and os.access(playbook, os.X_OK)
        and inventory.is_file()
        and os.access(inventory, os.X_OK),
        "ansible_not_found",
    )
    verifier = shutil.which("node" if args.public_chat_smoke else "curl")
    if verifier is None:
        raise DeployError("verification_tool_not_found")
    return str(playbook), str(inventory), verifier


def ansible_environment(root: Path) -> dict[str, str]:
    """Select checkout-owned Ansible configuration and private timing evidence."""
    # Use the reviewed configuration, including host-key checks. GitHub auth is
    # available to the controller's delegated fetch, never passed as extra vars.
    environment = {
        key: value for key, value in os.environ.items() if not key.startswith("ANSIBLE_")
    }
    environment.update(
        ANSIBLE_CONFIG=str(root / "ops/ansible/ansible.cfg"),
        ANSIBLE_HOST_KEY_CHECKING="True",
        ANSIBLE_RETRY_FILES_ENABLED="False",
    )
    return environment


def checkout_identity(runner: BUILD.RunnerProtocol, root: Path, repository: str) -> str:
    """Bind clean HEAD to the explicitly selected GitHub repository."""
    revision = BUILD.clean_revision(runner, root)
    _, remote = runner.run(["git", "remote", "get-url", "origin"], cwd=root)
    match = re.fullmatch(
        r"(?:https://github\.com/|git@github\.com:|ssh://git@github\.com/)"
        + r"([A-Za-z0-9][A-Za-z0-9-]{0,38}/[A-Za-z0-9][A-Za-z0-9_.-]{0,99}?)(?:\.git)?",
        remote,
    )
    require(
        match is not None and match[1].lower() == repository.lower(), "repository_origin_mismatch"
    )
    return revision


def selected_host(
    args: DeployOptions, inventory_tool: str, root: Path, environment: dict[str, str]
) -> tuple[str, JsonObject]:
    """Provide selected host for the release contract."""
    # Inventory can contain secrets. Inspect through an unlogged bounded runner,
    # and retain only the selected host's resolved variables. They may later be
    # frozen in a private one-host inventory, but are never printed or logged.
    _, encoded = BUILD.Runner().run(
        [inventory_tool, "-i", args.inventory, "--list"], cwd=root, env=environment, timeout=30
    )
    inventory = object_record(FETCH.decoded(encoded), "invalid_inventory_graph")

    def hosts(group: str, seen: set[str]) -> set[str]:
        require(group not in seen, "invalid_inventory_graph")
        value = object_record(inventory.get(group), "invalid_inventory_graph")
        direct, children = value.get("hosts", []), value.get("children", [])
        require(
            isinstance(direct, list)
            and isinstance(children, list)
            and all(isinstance(item, str) for item in direct + children),
            "invalid_inventory_graph",
        )
        direct_hosts = [item for item in array_value(direct) if isinstance(item, str)]
        child_groups = [item for item in array_value(children) if isinstance(item, str)]
        result = set(direct_hosts)
        for child in child_groups:
            result.update(hosts(child, seen | {group}))
        return result

    targets = hosts("benchmark_hosts", set())
    if args.limit is not None:
        require(args.limit in targets, "inventory_target_not_found")
        targets = {args.limit}
    require(len(targets) == 1, "select_exactly_one_inventory_host")
    host = next(iter(targets))
    require(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,252}", host), "invalid_inventory_host")
    metadata = object_record(inventory.get("_meta", {}), "invalid_inventory_graph")
    hostvars = object_record(metadata.get("hostvars", {}), "invalid_inventory_graph")
    variables = object_record(hostvars.get(host, {}), "invalid_inventory_graph")
    require(
        variables.get("scpub_domain") == urlsplit(args.origin).hostname,
        "inventory_smoke_origin_mismatch",
    )
    return host, variables


def require_ci_identity(
    run: JsonObject, repository: str, revision: str, run_id: int | None = None
) -> None:
    """Require the exact trusted main-branch push workflow and revision."""
    require(
        FETCH.positive(run.get("id"))
        and (run_id is None or run["id"] == run_id)
        and run.get("name") == "CI"
        and run.get("path") == ".github/workflows/ci.yml"
        and run.get("event") == "push"
        and run.get("head_branch") == "main"
        and run.get("head_sha") == revision
        and FETCH.repository_name(run, "repository") == repository.lower()
        and FETCH.repository_name(run, "head_repository") == repository.lower(),
        "ci_identity_mismatch",
    )


def select_ci_artifact(args: DeployOptions, revision: str) -> JsonObject:
    """Wait for one pinned CI run and select its unique matching release artifact."""
    runs = object_record(
        FETCH.api(
            f"repos/{args.repository}/actions/workflows/ci.yml/runs"
            + f"?event=push&branch=main&head_sha={revision}&per_page=1"
        ),
        "ci_response_invalid",
    )
    run_list = runs.get("workflow_runs")
    require(isinstance(run_list, list), "ci_response_invalid")
    candidates = array_value(run_list)
    require(len(candidates) == 1, "ci_missing_push_required")
    run = object_record(candidates[0], "ci_identity_mismatch")
    require_ci_identity(run, args.repository, revision)
    run_id = integer_value(run["id"])
    deadline = time.monotonic() + args.wait_seconds
    while True:
        # Pin the selected run throughout waiting; never fall back to an older
        # successful run when the latest run fails, and never trigger a rerun.
        run = object_record(
            FETCH.api(f"repos/{args.repository}/actions/runs/{run_id}"), "ci_identity_mismatch"
        )
        require_ci_identity(run, args.repository, revision, run_id)
        if run.get("status") == "completed":
            require(run.get("conclusion") == "success", "ci_failed")
            break
        require(
            run.get("status") in ("queued", "in_progress", "pending", "waiting", "requested"),
            "ci_status_invalid",
        )
        remaining = deadline - time.monotonic()
        require(remaining > 0, "ci_wait_timeout")
        print(f"Waiting for CI run {run_id}; at most {int(remaining)} seconds remain.", flush=True)  # noqa: T201 -- Intentional CLI status output.
        time.sleep(min(15, remaining))
    artifacts = object_record(
        FETCH.api(
            f"repos/{args.repository}/actions/runs/{run_id}/artifacts"
            + f"?name=simplestchat-production-{revision}&per_page=100"
        ),
        "ci_artifact_missing_or_ambiguous",
    )
    artifact_list = artifacts.get("artifacts")
    require(
        isinstance(artifact_list, list)
        and artifacts.get("total_count") == 1
        and len(artifact_list) == 1,
        "ci_artifact_missing_or_ambiguous",
    )
    artifact = object_record(array_value(artifact_list)[0], "ci_artifact_identity_invalid")
    require(FETCH.positive(artifact.get("id")), "ci_artifact_identity_invalid")
    selection = FETCH.ReleaseSelection(
        repository=args.repository,
        revision=revision,
        ci_run=run_id,
        artifact_id=integer_value(artifact["id"]),
    )
    envelope = FETCH.verified_envelope(selection)
    require(
        envelope["buildRunId"] == run_id and envelope["ciRunId"] == run_id,
        "ci_artifact_identity_invalid",
    )
    return envelope


@dataclass(frozen=True, slots=True, kw_only=True)
class PlaybookTarget:
    """The frozen inventory, host and controller environment every playbook run shares."""

    playbook: str
    inventory: Path
    host: str
    root: Path
    environment: dict[str, str]


def run_playbook(
    runner: BUILD.Runner, target: PlaybookTarget, name: str, extra: JsonObject
) -> None:
    """Run one playbook from `ops/ansible` against the frozen host with explicit variables."""
    _ = runner.run(
        [
            target.playbook,
            "-i",
            str(target.inventory),
            str(target.root / "ops/ansible" / name),
            "--limit",
            target.host,
            "--extra-vars",
            json.dumps(extra),
        ],
        cwd=target.root,
        env=target.environment,
        # Migration transfer runs two bounded 1800-second units sequentially,
        # with additional time for archive transport and source recovery.
        timeout={"release.yml": 4500, "force-release.yml": 4500, "migration-data.yml": 5400}.get(
            name, 2400
        ),
        capture=False,
    )


def anonymous_verification(runner: BUILD.Runner, curl: str, root: Path, origin: str) -> None:
    """Make bounded anonymous GETs without redirects, guests or public chat writes."""
    for path, expected in (("/health", "ok"), ("/ready", "ready"), ("/", None)):
        _, output = runner.run(
            [
                curl,
                "--disable",
                "--silent",
                "--show-error",
                "--fail",
                "--noproxy",
                "*",
                "--proto",
                "=https",
                "--tlsv1.2",
                "--connect-timeout",
                "5",
                "--max-time",
                "10",
                "--max-filesize",
                "1048576",
                "--request",
                "GET",
                "--write-out",
                "\n%{http_code}",
                origin + path,
            ],
            cwd=root,
            env=FETCH.ssh_environment(),
            timeout=15,
        )
        body, separator, status = output.rpartition("\n")
        require(separator and status == "200", "public_http_verification_failed")
        if expected is None:
            require("<!doctype html>" in body[:256].lower(), "public_homepage_verification_failed")
        else:
            value = object_record(FETCH.decoded(body), "public_http_verification_failed")
            require(value.get("status") == expected, "public_http_verification_failed")


def select_artifact(
    args: DeployOptions, directory: Path, root: Path, revision: str, report: JsonObject
) -> tuple[str, JsonObject]:
    """Keep unsigned authorization separate from the unchanged signed artifact flow."""
    if args.force:
        report.update(
            phase="force_artifact", githubAttested=False, ciVerification="skipped-explicit-force"
        )
        artifact, request = force_deploy.prepare(
            directory,
            root,
            revision,
            {
                "artifactDirectory": args.artifact_dir,
                "repository": args.repository,
                "deploy": not args.maintenance,
                "quietSeconds": args.quiet_seconds,
            },
        )
        report["fileDigests"] = request["fileDigests"]
        return "force-release.yml", {
            "scpub_force_release": True,
            "scpub_force_directory": str(artifact),
            "scpub_force_request": str(directory / "force.json"),
            "scpub_release_revision": revision,
        }
    report["phase"] = "ci"
    envelope = select_ci_artifact(args, revision)
    report["phase"] = "attestation"
    selection = FETCH.ReleaseSelection(
        args.repository,
        integer_value(envelope["artifactId"]),
        revision,
        integer_value(envelope["ciRunId"]),
    )
    verified_directory = directory / "verified"
    envelope = release_attestation.fetch_verify(
        FETCH.download_url(selection), verified_directory, envelope
    )
    BUILD.write_json(directory / "selection.json", envelope)
    report.update(
        ciRunId=envelope["ciRunId"],
        artifactId=envelope["artifactId"],
        githubAttested=True,
        ciVerification="passed",
    )
    return "release.yml", {
        "scpub_release_repository": args.repository,
        "scpub_release_artifact_id": envelope["artifactId"],
        "scpub_release_expected_revision": revision,
        "scpub_release_ci_run": envelope["ciRunId"],
        "scpub_release_verified_directory": str(verified_directory),
    }


def execute(args: DeployOptions, root: Path = ROOT) -> JsonObject:  # noqa: PLR0915 - one release transaction: identity, artifact, staging, optional maintenance, verification.
    """Deploy the pinned artifact once to a frozen host, then run public verification."""
    report: JsonObject = {
        "schemaVersion": 1,
        "operation": "maintenance" if args.maintenance else "deploy",
        "passed": False,
        "phase": "preflight",
        "deployed": False,
        "remoteOutcome": "not_started",
        "failureClass": None,
        "startedAt": datetime.now(UTC).isoformat(),
        "forced": args.force,
        "verification": "public-chat-smoke" if args.public_chat_smoke else "anonymous-http",
    }
    directory = None
    started = time.monotonic()
    try:
        inspector = BUILD.Runner()
        require(args.force or args.artifact_dir is None, "artifact_directory_requires_force")
        revision = checkout_identity(inspector, root, args.repository)
        report.update(repository=args.repository, revision=revision)
        playbook, inventory_tool, verifier = controller_tools(args, root)
        environment = ansible_environment(root)
        target = selected_host(args, inventory_tool, root, environment)
        host, hostvars = target
        if args.public_chat_smoke:
            _, version = inspector.run([verifier, "--version"], cwd=root)
            match = re.fullmatch(r"v(\d+)\.(\d+)\.\d+", version)
            require(
                match and (int(match[1]), int(match[2])) >= (22, 12), "node_version_unsupported"
            )
        _, ignored = inspector.run(
            ["git", "check-ignore", "--", str(root / "results/deploy.evidence")], cwd=root
        )
        require(bool(ignored), "results_must_be_git_ignored")
        parent = root / "results"
        require(not parent.is_symlink(), "invalid_evidence_parent")
        parent.mkdir(mode=0o700, exist_ok=True)
        directory = Path(tempfile.mkdtemp(prefix="deploy.", dir=parent))
        report["evidence"] = str(directory)
        runner = BUILD.Runner(directory)
        if args.force and args.maintenance:
            status, remote_main = runner.run(
                ["git", "ls-remote", "origin", "refs/heads/main"], cwd=root, allow_failure=True
            )
            require(
                status == 0 and remote_main.split() == [revision, "refs/heads/main"],
                "force_maintenance_requires_published_main",
            )
        release_playbook, artifact_extra = select_artifact(args, directory, root, revision, report)
        require(
            selected_host(args, inventory_tool, root, environment) == target,
            "inventory_target_changed",
        )
        # Ansible --limit accepts patterns and a host alias can equal a group
        # name. A frozen one-host inventory makes the destination unambiguous
        # and prevents a later original-inventory edit from redirecting it.
        frozen_inventory = directory / "inventory.json"
        BUILD.write_json(frozen_inventory, {"benchmark_hosts": {"hosts": {host: hostvars}}})
        extra: JsonObject = {
            **artifact_extra,
            "scpub_release_prepared": not args.install_helpers,
            "scpub_release_deploy": not args.maintenance,
            "scpub_release_quiet_seconds": args.quiet_seconds,
        }
        require(checkout_identity(inspector, root, args.repository) == revision, "checkout_changed")
        report.update(phase="deploy", remoteOutcome="inspect_if_interrupted")
        target = PlaybookTarget(
            playbook=playbook,
            inventory=frozen_inventory,
            host=host,
            root=root,
            environment=environment,
        )
        run_playbook(runner, target, release_playbook, extra)
        if args.maintenance:
            # Staged while live; the launcher now migrates, grants and restarts.
            report.update(phase="maintenance", remoteOutcome="inspect_if_interrupted")
            run_playbook(runner, target, "maintenance.yml", {"scpub_release_revision": revision})
        report.update(deployed=True, remoteOutcome="release_succeeded", phase="public_smoke")
        if not args.public_chat_smoke:
            report["phase"] = "anonymous_verification"
            anonymous_verification(runner, verifier, root, args.origin)
            report.update(passed=True, phase="complete")
            return report
        _, output = runner.run(
            [
                verifier,
                str(root / "build/public-smoke.mjs"),
                "--origin",
                args.origin,
                "--room",
                args.room,
            ],
            cwd=root,
            env=FETCH.ssh_environment(),
            timeout=60,
        )
        smoke = FETCH.decoded(output)
        require(
            isinstance(smoke, dict)
            and smoke.get("passed") is True
            and smoke.get("origin") == args.origin
            and smoke.get("room") == args.room,
            "public_smoke_failed",
        )
        report.update(passed=True, phase="complete")
    except (Exception, KeyboardInterrupt) as error:  # noqa: BLE001 -- Redact all failures while finalizing owned resources.
        report["failureClass"] = (
            str(error)
            if isinstance(error, (DeployError, FETCH.FetchError))
            else "controller_step_failed"
        )
    finally:
        report["elapsedSeconds"] = round(time.monotonic() - started, 3)
        if directory is not None:
            try:
                BUILD.write_json(directory / "outcome.json", report)
            except OSError:
                report.update(passed=False, failureClass="outcome_write_failed")
    return report


def main(argv: list[str] | None = None) -> int:
    """Run the CLI transaction and return a redacted success or failure status."""
    try:
        args = options(argv)
    except SystemExit as error:
        return error.code if isinstance(error.code, int) else 1
    except (DeployError, OSError, ValueError):
        print(json.dumps({"passed": False, "phase": "options", "failureClass": "invalid_options"}))  # noqa: T201 -- Intentional CLI status output.
        return 1

    def interrupted(_signum: int, _frame: FrameType | None) -> None:
        raise DeployError("interrupted")

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
    print(json.dumps(report), flush=True)  # noqa: T201 -- Intentional CLI status output.
    if not report["passed"]:
        if report["failureClass"] == "ci_missing_push_required":
            print(  # noqa: T201 -- Intentional CLI status output.
                "No push CI exists for this clean HEAD on main. "
                + "Push the reviewed commit explicitly, then rerun."
            )
        elif report["failureClass"] == "force_maintenance_requires_published_main":
            print(  # noqa: T201 -- Intentional CLI status output.
                "Force maintenance requires clean HEAD to match published origin/main "
                + "because the host fetches its source. App-only force does not require a push."
            )
        else:
            print(  # noqa: T201 -- Intentional CLI status output.
                "Stopped without retry. Inspect the retained evidence "
                + "and any remote release journal before another attempt."
            )
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
