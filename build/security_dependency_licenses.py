"""Enforce the existing license policy on changed npm and Python lock entries.

Current vulnerability audits still inspect complete dependency graphs. This
additional gate compares an ancestor with the source snapshot and checks newly
introduced package identities using authenticated registry metadata. It never
installs packages, builds source distributions or infers licenses from prose.
"""

from __future__ import annotations

import hashlib
import json
import posixpath
import re
import time
import tomllib
from dataclasses import dataclass, field
from email import policy
from email.parser import BytesParser
from typing import TYPE_CHECKING, cast
from urllib.parse import quote, urlsplit

from packaging.requirements import Requirement
from packaging.utils import canonicalize_name, parse_sdist_filename, parse_wheel_filename
from security_context import Context, executable
from security_image_policy import LicenseExpression
from security_source_scope import require_local_vendor, selected
from security_tools import bounded_file, require, safe_name, write_private

# isort: split
import bounded_process
from release_json import array_value, decode_json, json_value, object_value, string_value

if TYPE_CHECKING:
    from collections.abc import Callable
    from pathlib import Path

NPM_LOCKS = ("web/package-lock.json", "web/e2e/package-lock.json")
PYTHON_LOCKS = (
    "build/python-requirements.txt",
    "ops/ansible/requirements.txt",
    "security/requirements.txt",
    "vendor/mediasoup-sys-0.19.0/python-invoke-requirements.txt",
    "vendor/mediasoup-sys-0.19.0/python-tools-requirements.txt",
)
MAX_LOCK = 2 * 1024**2
MAX_INDEX = 16 * 1024**2
MAX_METADATA = 1024**2
MAX_ENTRIES = 10000
MAX_FETCHES = 512
MAX_SECONDS = 240
MAX_CACHE = 64 * 1024**2
MAX_INCLUDES = 16
MAX_INDEX_FILES = 50000
NPM_LOCK_VERSION = 3
SHA256 = re.compile(r"[a-f0-9]{64}")
OTHER_LOCK_NAMES = frozenset(
    {
        "package-lock.json",
        "npm-shrinkwrap.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "Pipfile.lock",
        "poetry.lock",
        "uv.lock",
        "pdm.lock",
        "pylock.toml",
    }
)


@dataclass(frozen=True)
class Dependency:
    """A complete changed declaration, including hashes, source and markers."""

    ecosystem: str
    manifest: str
    name: str
    version: str
    identity: str
    hashes: frozenset[str] = frozenset()
    license: str = ""
    source: str = ""


def allowed_licenses(snapshot: Path) -> set[str]:
    """Use Cargo's existing exact policy rather than maintaining another allowlist."""
    raw = object_value(
        json_value(tomllib.loads(bounded_file(snapshot / "deny.toml", MAX_LOCK).decode()))
    )
    values = array_value(object_value(raw["licenses"])["allow"])
    allowed = {string_value(value) for value in values}
    require(bool(allowed) and len(allowed) == len(values), "dependency_license_policy")
    return allowed


def npm_dependencies(path: str, content: bytes) -> set[Dependency]:
    """Retain each installed path, including dev, optional and nested packages."""
    raw = object_value(decode_json(content))
    require(raw.get("lockfileVersion") == NPM_LOCK_VERSION, "dependency_npm_lock_version")
    packages = object_value(raw["packages"])
    require(len(packages) <= MAX_ENTRIES, "dependency_npm_count")
    result: set[Dependency] = set()
    for location, value in packages.items():
        if not location:
            continue
        package = object_value(value)
        require("node_modules/" in location and not package.get("link"), "dependency_npm_path")
        name = string_value(package.get("name", location.rsplit("node_modules/", 1)[1]))
        identity = json.dumps([location, package], sort_keys=True, separators=(",", ":"))
        result.add(
            Dependency(
                "npm",
                path,
                name,
                string_value(package["version"]),
                identity,
                license=string_value(package.get("license", "")),
                source=string_value(package.get("resolved", "")),
                hashes=frozenset({string_value(package.get("integrity", ""))}),
            )
        )
    return result


def requirement_lines(content: bytes) -> list[str]:
    """Join bounded hash-lock continuations and discard only requirement comments."""
    require(len(content) <= MAX_LOCK, "dependency_requirements_size")
    text = content.decode("utf-8").replace("\\\n", " ")
    lines = [line.split("#", 1)[0].strip() for line in text.splitlines()]
    require(len(lines) <= MAX_ENTRIES, "dependency_requirements_count")
    return [line for line in lines if line]


def python_requirement(path: str, line: str) -> Dependency:
    """Accept only exact named versions with explicit SHA-256 artifact hashes."""
    parts = re.split(r"\s+--hash=sha256:", line)
    require(
        len(parts) > 1 and all(SHA256.fullmatch(item.strip()) for item in parts[1:]),
        "dependency_python_hash",
    )
    requirement = Requirement(parts[0].strip())
    versions = list(requirement.specifier)
    require(
        requirement.url is None
        and not requirement.extras
        and len(versions) == 1
        and versions[0].operator == "=="
        and "*" not in versions[0].version,
        "dependency_python_exact_version",
    )
    hashes = frozenset(item.strip() for item in parts[1:])
    identity = json.dumps([str(requirement), sorted(hashes)], separators=(",", ":"))
    return Dependency(
        "pypi", path, canonicalize_name(requirement.name), versions[0].version, identity, hashes
    )


def python_dependencies(
    path: str, load: Callable[[str], bytes | None], stack: tuple[str, ...] = ()
) -> set[Dependency]:
    """Follow in-repository includes without evaluating platform markers locally."""
    require(
        path not in stack and len(stack) < MAX_INCLUDES and safe_name(path),
        "dependency_requirements_include",
    )
    content = load(path)
    if content is None:
        require(not stack, "dependency_requirements_missing_include")
        return set()
    result: set[Dependency] = set()
    for line in requirement_lines(content):
        if line in {"--require-hashes", "--only-binary=:all:"}:
            continue
        if line.startswith("-r "):
            included = posixpath.normpath(posixpath.join(posixpath.dirname(path), line[3:].strip()))
            result.update(python_dependencies(included, load, (*stack, path)))
        else:
            result.add(python_requirement(path, line))
    return result


def dependencies(load: Callable[[str], bytes | None]) -> set[Dependency]:
    """Inspect all maintained lock graphs with the same parser for base and head."""
    result: set[Dependency] = set()
    for path in NPM_LOCKS:
        content = load(path)
        if content is not None:
            result.update(npm_dependencies(path, content))
    for path in PYTHON_LOCKS:
        result.update(python_dependencies(path, load))
    require(len(result) <= MAX_ENTRIES, "dependency_count")
    return result


def require_wheel_only(snapshot: Path) -> None:
    """Keep the existing installer policy explicit before excluding source archives."""
    for path in PYTHON_LOCKS:
        target = snapshot / path
        if not target.exists():
            continue
        if path == "security/requirements.txt":
            installer = bounded_file(snapshot / "build/check-security.sh", MAX_LOCK).decode()
            command = re.sub(r"\s+", " ", installer.replace("\\\n", " "))
            require(
                "--require-hashes --only-binary=:all: --requirement security/requirements.txt"
                in command,
                "dependency_security_wheel_only_policy",
            )
        else:
            require(
                "--only-binary=:all:" in requirement_lines(bounded_file(target, MAX_LOCK)),
                "dependency_python_wheel_only_policy",
            )


def require_audited_locks(
    names: set[str],
    current: Callable[[str], bytes | None],
    previous: Callable[[str], bytes | None],
    *,
    include_vendor: bool = False,
) -> None:
    """Require shared audit support before a new lock graph introduces dependencies."""
    require_local_vendor(include_vendor=include_vendor)
    maintained = set(NPM_LOCKS) | set(PYTHON_LOCKS)
    for path in sorted(names - maintained):
        # Installed build requirements are audited above even inside vendor/.
        # Unused upstream development locks follow the optional source-scan policy.
        if not selected(path, include_vendor=include_vendor):
            continue
        name = posixpath.basename(path)
        is_lock = (
            name in OTHER_LOCK_NAMES
            or ("requirements" in name and name.endswith(".txt"))
            or (name.startswith("pylock.") and name.endswith(".toml"))
        )
        if is_lock:
            content = current(path)
            require(
                content is None or content == previous(path), "dependency_unaudited_lock_changed"
            )


@dataclass
class Registry:
    """One bounded, credential-free metadata session; identical sidecars are reused."""

    context: Context
    started: float = field(default_factory=time.monotonic)
    requests: int = 0
    cached_bytes: int = 0
    cache: dict[str, bytes] = field(default_factory=dict)

    def fetch(self, url: str, limit: int = MAX_INDEX) -> bytes:
        """Require exact HTTPS origins, HTTP 200, hard byte limits and a total deadline."""
        if url in self.cache:
            return self.cache[url]
        parts = urlsplit(url)
        require(
            parts.scheme == "https"
            and parts.netloc in {"pypi.org", "files.pythonhosted.org", "registry.npmjs.org"}
            and not parts.query
            and not parts.fragment,
            "dependency_registry_origin",
        )
        remaining = min(30, int(MAX_SECONDS - (time.monotonic() - self.started)))
        self.requests += 1
        require(remaining > 0 and self.requests <= MAX_FETCHES, "dependency_registry_budget")
        status, output, _ = bounded_process.run(
            [
                executable("curl"),
                "--disable",
                "--fail",
                "--silent",
                "--show-error",
                "--proto",
                "=https",
                "--tlsv1.2",
                "--connect-timeout",
                "10",
                "--max-time",
                str(remaining),
                "--max-filesize",
                str(limit),
                "--header",
                "Accept: application/vnd.pypi.simple.v1+json, application/json",
                "--write-out",
                "\n%{http_code}",
                url,
            ],
            cwd=self.context.root,
            env=self.context.env,
            limits=bounded_process.Limits(timeout=remaining + 2, stdout=limit + 4, stderr=16384),
        )
        require(status == 0 and output.endswith(b"\n200"), "dependency_registry_fetch")
        content = output[:-4]
        require(0 < len(content) <= limit, "dependency_registry_size")
        self.cached_bytes += len(content)
        require(self.cached_bytes <= MAX_CACHE, "dependency_registry_cache_size")
        self.cache[url] = content
        return content


def npm_license(dependency: Dependency, registry: Registry) -> list[dict[str, object]]:
    """Cross-check lock declarations against the registry's exact tarball identity."""
    url = (
        "https://registry.npmjs.org/"
        + quote(dependency.name, safe="")
        + "/"
        + quote(dependency.version, safe="")
    )
    content = registry.fetch(url, MAX_METADATA)
    raw = object_value(decode_json(content))
    dist = object_value(raw["dist"])
    require(
        raw.get("name") == dependency.name
        and raw.get("version") == dependency.version
        and dist.get("tarball") == dependency.source
        and dependency.hashes == frozenset({string_value(dist["integrity"])}),
        "dependency_npm_registry_identity",
    )
    expression = string_value(raw.get("license", ""))
    require(
        expression == dependency.license and bool(expression), "dependency_npm_license_identity"
    )
    return [
        {
            "license": expression,
            "source": url,
            "metadataSha256": hashlib.sha256(content).hexdigest(),
        }
    ]


def metadata_license(content: bytes, dependency: Dependency) -> str:
    """Read explicit SPDX metadata only; old free-text fields are never guessed."""
    require(0 < len(content) <= MAX_METADATA, "dependency_python_metadata_size")
    metadata = BytesParser(policy=policy.default).parsebytes(content)
    require(not metadata.defects, "dependency_python_metadata_format")
    for name in ("Name", "Version", "License-Expression"):
        require(len(metadata.get_all(name, [])) == 1, "dependency_python_metadata_fields")
    require(
        canonicalize_name(str(cast("object", metadata["Name"]))) == dependency.name
        and str(cast("object", metadata["Version"])) == dependency.version,
        "dependency_python_metadata_identity",
    )
    return str(cast("object", metadata["License-Expression"]))


def python_licenses(dependency: Dependency, registry: Registry) -> list[dict[str, object]]:
    """Verify every locked wheel's index-linked, hash-authenticated PEP 658 metadata."""
    index = object_value(
        decode_json(registry.fetch("https://pypi.org/simple/" + dependency.name + "/"))
    )
    require(
        canonicalize_name(string_value(index["name"])) == dependency.name,
        "dependency_python_index_identity",
    )
    files = array_value(index["files"])
    require(len(files) <= MAX_INDEX_FILES, "dependency_python_index_count")
    result: list[dict[str, object]] = []
    metadata_cache: dict[str, str] = {}
    observed: set[str] = set()
    wheel_count = 0
    for value in files:
        item = object_value(value)
        digest = string_value(object_value(item["hashes"])["sha256"])
        filename = string_value(item["filename"])
        if digest not in dependency.hashes:
            continue
        url = string_value(item["url"])
        require(
            urlsplit(url).scheme == "https"
            and urlsplit(url).netloc == "files.pythonhosted.org"
            and not urlsplit(url).query
            and not urlsplit(url).fragment
            and url.endswith("/" + filename),
            "dependency_python_wheel_origin",
        )
        observed.add(digest)
        if not filename.endswith(".whl"):
            name, version = parse_sdist_filename(filename)
            require(
                name == dependency.name and str(version) == dependency.version,
                "dependency_python_sdist_identity",
            )
            result.append(
                {
                    "kind": "sdist",
                    "excluded": "wheel-only-installation",
                    "source": url,
                    "artifactSha256": digest,
                }
            )
            continue
        name, version, _, _ = parse_wheel_filename(filename)
        require(
            name == dependency.name and str(version) == dependency.version,
            "dependency_python_wheel_identity",
        )
        metadata_hash = string_value(object_value(item["core-metadata"])["sha256"])
        require(SHA256.fullmatch(metadata_hash), "dependency_python_metadata_hash")
        if metadata_hash not in metadata_cache:
            content = registry.fetch(url + ".metadata", MAX_METADATA)
            require(
                hashlib.sha256(content).hexdigest() == metadata_hash,
                "dependency_python_metadata_integrity",
            )
            metadata_cache[metadata_hash] = metadata_license(content, dependency)
        result.append(
            {
                "kind": "wheel",
                "license": metadata_cache[metadata_hash],
                "source": url,
                "wheelSha256": digest,
                "metadataSha256": metadata_hash,
            }
        )
        wheel_count += 1
    require(frozenset(observed) == dependency.hashes, "dependency_python_unresolved_hash")
    require(wheel_count > 0, "dependency_python_wheel_missing")
    return result


def changed_dependencies(
    context: Context, snapshot: Path, base: str | None, *, include_vendor: bool = False
) -> tuple[str, set[Dependency]]:
    """Require an ancestor commit; omitted base compares working changes with HEAD."""
    selected = base or "HEAD"
    require(selected == "HEAD" or re.fullmatch(r"[a-f0-9]{40}", selected), "dependency_diff_base")
    _, revision = context.run(
        "dependency-base", [executable("git"), "rev-parse", "--verify", selected + "^{commit}"]
    )
    resolved = revision.decode().strip()
    _ = context.run(
        "dependency-base-ancestor",
        [executable("git"), "merge-base", "--is-ancestor", resolved, "HEAD"],
    )
    _, listing = context.run(
        "dependency-base-files", [executable("git"), "ls-tree", "-rz", "--name-only", resolved]
    )
    names = set(listing.decode().rstrip("\0").split("\0"))
    cache: dict[str, bytes | None] = {}

    def previous(path: str) -> bytes | None:
        require(safe_name(path), "dependency_base_path")
        if path not in cache:
            cache[path] = (
                context.run(
                    "dependency-base-lock", [executable("git"), "show", resolved + ":" + path]
                )[1]
                if path in names
                else None
            )
        return cache[path]

    def current(path: str) -> bytes | None:
        require(safe_name(path), "dependency_current_path")
        target = snapshot / path
        return bounded_file(target, MAX_LOCK) if target.exists() else None

    _, current_listing = context.run(
        "dependency-current-files",
        [executable("git"), "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
    )
    current_names = set(current_listing.decode().rstrip("\0").split("\0"))
    require_audited_locks(names | current_names, current, previous, include_vendor=include_vendor)
    return resolved, dependencies(current) - dependencies(previous)


def check(
    context: Context, snapshot: Path, base: str | None, *, include_vendor: bool = False
) -> None:
    """Record exact changed declarations and fail unapproved, absent or invalid licenses."""
    started = time.monotonic()
    result: dict[str, object] = {"name": "changed-dependency-licenses", "exitStatus": 1}
    context.checks.append(result)
    records: list[dict[str, object]] = []
    receipt: dict[str, object] = {"schemaVersion": 1, "passed": False, "dependencies": records}
    try:
        allowed = allowed_licenses(snapshot)
        require_wheel_only(snapshot)
        resolved, changed = changed_dependencies(
            context, snapshot, base, include_vendor=include_vendor
        )
        receipt.update(baseRevision=resolved, allowedLicenses=sorted(allowed))
        registry = Registry(context)
        for dependency in sorted(
            changed, key=lambda item: (item.ecosystem, item.manifest, item.name, item.identity)
        ):
            evidence = (
                npm_license(dependency, registry)
                if dependency.ecosystem == "npm"
                else python_licenses(dependency, registry)
            )
            records.append(
                {
                    "ecosystem": dependency.ecosystem,
                    "manifest": dependency.manifest,
                    "name": dependency.name,
                    "version": dependency.version,
                    "declarationSha256": hashlib.sha256(dependency.identity.encode()).hexdigest(),
                    "evidence": evidence,
                }
            )
            for item in evidence:
                if item.get("kind") == "sdist":
                    require(
                        item.get("excluded") == "wheel-only-installation",
                        "dependency_python_sdist_policy",
                    )
                    continue
                require(
                    LicenseExpression(str(item["license"]), allowed).allowed_expression(),
                    "dependency_license_not_allowed",
                )
        receipt["passed"] = True
        result.update(
            exitStatus=0, changedDependencies=len(changed), registryRequests=registry.requests
        )
    finally:
        write_private(
            context.output / "dependency-licenses.json",
            (json.dumps(receipt, indent=2, sort_keys=True) + "\n").encode(),
            0o600,
        )
        result["elapsedSeconds"] = round(time.monotonic() - started, 3)
