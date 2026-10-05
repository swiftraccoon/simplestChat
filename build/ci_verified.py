"""Bind reusable CI successes and backend executables to their exact checked inputs."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import shlex
import shutil
import stat
import subprocess
import sys
from pathlib import Path
from typing import cast

from security_tools import ToolError, bounded_file, record, require, string

ROOT = Path(__file__).resolve().parents[1]
SCOPES = (
    "automation",
    "rust-lint",
    "rust-test",
    "backend",
    "native-asan",
    "native-ubsan",
    "native-replay",
    "codeql-native",
    "codeql-rust",
)
BINARIES = ("simplestChat", "load_test")
NATIVE_PREFIXES = ("vendor/", "security/native/")
NATIVE_FILES = frozenset(
    {
        ".github/workflows/security.yml",
        "build/ci-local-act.json",
        "build/ci-local-act.patch",
        "build/ci-local-docker.sh",
        "build/ci-local.sh",
        "build/ci_local_act.py",
        "build/ci_verified.py",
        "build/install-openssl.sh",
        "build/native-security.Dockerfile",
        "build/native_security.py",
        "build/native_security_cache.py",
        "build/pip-constraints.txt",
        "build/security_codeql_resources.py",
        "build/security_context.py",
        "build/security_source_scope.py",
        "build/security_tools.py",
        "build/security_vendor.py",
        "ops/ansible/files/bounded_process.py",
        "ops/ansible/files/release_json.py",
    }
)
CODEQL_NATIVE_PREFIXES = ("vendor/", "security/codeql/")
CODEQL_POLICY_DATA = frozenset(
    {
        "security/codeql-review-2026-09-30.json",
        "security/codeql-native-review-2026-09-30.json",
        "security/codeql-review-2026-09-30.md",
        "security/codeql-native-review-2026-09-30.md",
        "security/exceptions.json",
    }
)
CODEQL_NATIVE_FILES = frozenset(
    {
        ".github/workflows/codeql.yml",
        "build/ci_verified.py",
        "build/codeql-native-build.sh",
        "build/install-openssl.sh",
        "build/pip-constraints.txt",
        "build/security_codeql.py",
        "build/security_codeql_cache.py",
        "build/security_codeql_cargo.py",
        "build/security_codeql_local.py",
        "build/security_codeql_resources.py",
        "build/security_codeql_sources.py",
        "build/security_codeql_tools.py",
        "build/security_codeql_triage.py",
        "build/security_context.py",
        "build/security_findings.py",
        "build/security_policy.py",
        "build/security_source_scope.py",
        "build/security_tools.py",
        "build/security_vendor.py",
        "ops/ansible/files/bounded_process.py",
        "ops/ansible/files/release_json.py",
        "security/codeql-toolchain.json",
    }
)
CODEQL_RUST_PREFIXES = (
    "src/",
    "tests/",
    "migrations/",
    "vendor/",
    ".cargo/",
    "security/authorization/",
    "security/codeql/",
)
CODEQL_RUST_MANIFESTS = frozenset(
    {"Cargo.toml", "Cargo.lock", "rust-toolchain", "rust-toolchain.toml", "rust-project.json"}
)
BACKEND_PREFIXES = ("src/", "tests/", "load_tests/", "migrations/", "vendor/", ".cargo/")
BACKEND_FILES = frozenset(
    {
        ".github/workflows/ci.yml",
        ".github/actions/native-toolchain/action.yml",
        "build/ci_verified.py",
        "build/security_tools.py",
        "build/install-openssl.sh",
        "build/pip-constraints.txt",
        "build/ci-local.sh",
        "build/ci-local-act.json",
        "build/ci-local-act.patch",
        "build/ci_local_act.py",
        "build/ci-local-postgres.sh",
        "ops/ansible/files/bounded_process.py",
        "ops/ansible/files/release_json.py",
    }
)
BACKEND_ENVIRONMENT = frozenset(
    {
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTC",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "RUSTUP_TOOLCHAIN",
        "CARGO_BUILD_TARGET",
        "CARGO_TARGET_DIR",
        "CC",
        "CXX",
        "AR",
        "CFLAGS",
        "CPPFLAGS",
        "CXXFLAGS",
        "LDFLAGS",
        "OPENSSL_DIR",
        "OPENSSL_STATIC",
        "PKG_CONFIG_PATH",
        "PIP_CONSTRAINT",
        "PIP_CONFIG_FILE",
        "PIP_INDEX_URL",
        "PIP_EXTRA_INDEX_URL",
        "PYTHON",
        "PYTHONPATH",
        "SDKROOT",
        "MACOSX_DEPLOYMENT_TARGET",
        "DOCS_RS",
        "MESON",
        "MESON_ARGS",
        "MESON_VERSION",
        "NINJA_VERSION",
        "PIP_CERT",
        "PIP_CLIENT_CERT",
        "PIP_FIND_LINKS",
        "PIP_NO_BINARY",
        "PIP_NO_INDEX",
        "PIP_ONLY_BINARY",
        "PIP_PREFER_BINARY",
        "PIP_PROXY",
        "PIP_REQUIRE_HASHES",
        "PIP_TRUSTED_HOST",
    }
)


def backend_input(name: str) -> bool:
    """Bind all targets and embedded data while excluding independent operational tooling."""
    return (
        name.endswith(".rs")
        or name.rsplit("/", 1)[-1] in CODEQL_RUST_MANIFESTS
        or name.startswith(BACKEND_PREFIXES)
        or name.startswith("security/authorization/")
        or (name.startswith("web/tests/") and name.endswith(".json"))
        or name in BACKEND_FILES
    )


def backend_workflow(path: Path) -> bytes:
    """Bind global configuration and the complete existing browser build prefix.

    This is a strict source-region extraction, not a YAML parser. Unsupported
    layout, aliases or later global configuration fail closed instead of silently
    hiding a build input. Runtime/browser steps after publication remain fresh.
    """
    text = bounded_file(path, 1024**2).decode()
    require(text.count("\njobs:\n") == 1, "ci_backend_workflow_jobs")
    global_config, jobs = text.split("\njobs:\n", 1)
    require(not re.search(r"^[^\s#]", jobs, re.MULTILINE), "ci_backend_workflow_global")
    require(jobs.count("\n  browser:\n") == 1, "ci_backend_workflow_browser")
    browser = jobs.split("\n  browser:\n", 1)[1]
    browser = re.split(r"^  [a-z][a-z0-9-]*:\s*$", browser, maxsplit=1, flags=re.MULTILINE)[0]
    boundary = "      - name: Migrate disposable test database with the real server\n"
    require(browser.count(boundary) == 1, "ci_backend_workflow_boundary")
    prefix, runtime = browser.split(boundary, 1)
    require(not re.search(r"^    [^\s#]", runtime, re.MULTILINE), "ci_backend_workflow_late_job")
    for marker in (
        "    steps:\n",
        "      - name: Build server and load generator\n",
        "      - name: Save the exact backend success\n",
    ):
        require(prefix.count(marker) == 1, "ci_backend_workflow_build")
    selected = global_config + "\njobs:\n  browser:\n" + prefix
    require(not re.search(r"(?:^|[\s\[{},:])[&*][\w-]+", selected), "ci_backend_workflow_alias")
    return selected.encode()


def backend_environment(root: Path) -> dict[str, str]:
    """Bind compiler overrides; pinned setup actions supply their documented defaults."""
    environment = {
        name: value
        for name, value in os.environ.items()
        if name in BACKEND_ENVIRONMENT
        or name.startswith(("CARGO_BUILD_", "CARGO_PROFILE_", "CARGO_TARGET_"))
    }
    require(
        Path(os.environ.get("CARGO_TARGET_DIR", root / "target")).resolve() == root / "target",
        "ci_backend_target_directory",
    )
    environment["CARGO_HOME"] = os.environ.get("CARGO_HOME") or str(Path.home() / ".cargo")
    environment["CARGO_INCREMENTAL"] = os.environ.get("CARGO_INCREMENTAL", "0")
    environment["workspace"] = str(root.resolve())
    return environment


def backend_dependencies(root: Path, directory: Path, name: str, *, built: bool) -> None:
    """Reject compiler dependencies outside the keyed source and audited generated inputs."""
    raw = bounded_file(directory / (name + ".d"), 4 * 1024**2).decode()
    # Cargo emits Make dependencies, not shell strings. Its escaped spaces and
    # continuations are supported; quote/dollar syntax is deliberately rejected.
    require(not any(char in raw for char in "\x00\r\"'$"), "ci_backend_dep_syntax")
    lines = raw.replace("\\\n", "").splitlines()
    require(len([line for line in lines if line.strip()]) == 1, "ci_backend_dep_lines")
    words = shlex.split(" ".join(lines), comments=False)
    require(
        len(words) > 1 and words[0] == str(root / "target/debug" / name) + ":",
        "ci_backend_dep_target",
    )
    tracked = set(
        subprocess.run(
            ["git", "ls-files", "-z"],  # noqa: S607 -- Same workflow Git as the key inventory.
            cwd=root,
            check=True,
            capture_output=True,
            timeout=30,
        )
        .stdout.decode()
        .split("\0")
    ) - {""}
    openssl = os.environ.get("OPENSSL_DIR", "")
    allowed_openssl: set[str] = (
        {
            str(Path(openssl) / suffix)
            for suffix in (
                "include/openssl",
                "lib/libssl.a",
                "lib/libcrypto.a",
                "lib/pkgconfig/openssl.pc",
            )
        }
        if openssl
        else set()
    )
    source_count = 0
    for word in words[1:]:
        path = Path(word)
        require(path.is_absolute() and path.is_relative_to(root), "ci_backend_dep_external")
        require(path.resolve() == path, "ci_backend_dep_symlink")
        relative = str(path.relative_to(root))
        if relative in tracked:
            require(backend_input(relative), "ci_backend_dep_unkeyed")
            require(path.is_file(), "ci_backend_dep_missing")
            source_count += 1
        elif path.is_dir() and (relative == "vendor" or relative.startswith("vendor/")):
            children = [entry for entry in tracked if entry.startswith(relative + "/")]
            require(
                children and all(backend_input(entry) for entry in children), "ci_backend_dep_tree"
            )
        else:
            generated = re.fullmatch(
                r"target/debug/build/mediasoup-sys-[a-f0-9]{16}/out/fbs\.rs", relative
            )
            require(bool(generated) or str(path) in allowed_openssl, "ci_backend_dep_unkeyed")
            require(not built or path.exists(), "ci_backend_dep_missing")
    require(source_count > 0, "ci_backend_dep_empty")


def native_security_input(name: str) -> bool:
    """Bind native sources, corpus, provenance checks and their actual execution helpers."""
    return name.startswith(NATIVE_PREFIXES) or name in NATIVE_FILES


def native_codeql_input(name: str) -> bool:
    """Bind the complete worker and its analysis helpers without unrelated CI tooling."""
    return name.startswith(CODEQL_NATIVE_PREFIXES) or name in CODEQL_NATIVE_FILES


def rust_codeql_input(name: str) -> bool:
    """Bind every Rust target, embedded fixture, dependency and analysis helper.

    Vendored build scripts consume their full native package trees. Embedded
    SQL, authorization JSON and shared browser cases also affect Rust semantics.
    Retain all Rust files and Cargo manifests regardless of their directory;
    archived tracked inputs outside this closure are rejected before query reuse.
    """
    return (
        name.endswith(".rs")
        or name.rsplit("/", 1)[-1] in CODEQL_RUST_MANIFESTS
        or name.startswith(CODEQL_RUST_PREFIXES)
        or (name.startswith("web/tests/") and name.endswith(".json"))
        or name in CODEQL_NATIVE_FILES
    )


class Options(argparse.Namespace):
    """Validated fixed operations and explicit paths from the workflow."""

    operation: str = ""
    scope: str = ""
    key: str = ""
    directory: Path = Path()


def digest(path: Path) -> str:
    """Hash only regular files, including empty tracked files, without loading executables."""
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as source:
        metadata = os.fstat(source.fileno())
        require(stat.S_ISREG(metadata.st_mode), "ci_cache_regular_file")
        require(metadata.st_size <= 1024**3, "ci_cache_file_size")
        return hashlib.file_digest(source, "sha256").hexdigest()


def runner_identity() -> str:
    """Keep immutable local images and hosted image generations in separate namespaces."""
    if os.environ.get("ACT") == "true":
        image = os.environ.get("LOCAL_CI_RUNNER_IMAGE", "")
        require(re.fullmatch(r"[a-z0-9./_-]+@sha256:[a-f0-9]{64}", image), "ci_cache_local_image")
        return "local:" + image
    # These mixed-case names are supplied by the GitHub-hosted runner image.
    image_os = os.environ.get("ImageOS", "")  # noqa: SIM112
    version = os.environ.get("ImageVersion", "")  # noqa: SIM112
    require(
        re.fullmatch(r"[a-zA-Z0-9._-]{1,100}", image_os)
        and re.fullmatch(r"[a-zA-Z0-9._-]{1,100}", version),
        "ci_cache_hosted_image",
    )
    return f"hosted:{image_os}:{version}"


def cache_namespace() -> str:
    """Expose trust, OS and ISA for conservative cache retention without weakening key hashes."""
    trusted = (
        os.environ.get("GITHUB_REF") == "refs/heads/main"
        and os.environ.get("GITHUB_EVENT_NAME") != "pull_request"
    )
    system, architecture = platform.system().lower(), platform.machine().lower()
    require(
        re.fullmatch(r"[a-z0-9_]{1,32}", system) and re.fullmatch(r"[a-z0-9_]{1,32}", architecture),
        "ci_cache_namespace",
    )
    return ("main" if trusted else "untrusted") + "-" + system + "-" + architecture


def cache_key(root: Path, scope: str) -> str:
    """Bind each check's source, helpers, policy, workflow, tool and runner inputs."""
    require(scope in SCOPES, "ci_cache_scope")
    trusted = (
        os.environ.get("GITHUB_REF") == "refs/heads/main"
        and os.environ.get("GITHUB_EVENT_NAME") != "pull_request"
    )
    identity = {
        "scope": scope,
        "trust": "main" if trusted else "untrusted",
        "system": platform.system(),
        "architecture": platform.machine(),
        "runner": runner_identity(),
    }
    hashed = hashlib.sha256(json.dumps(identity, sort_keys=True).encode())
    if scope == "backend":
        hashed.update(json.dumps(backend_environment(root), sort_keys=True).encode())
    tracked = subprocess.run(
        ["git", "ls-files", "--stage", "-z"],  # noqa: S607 -- Use the workflow's Git installation.
        cwd=root,
        check=True,
        capture_output=True,
        timeout=30,
    ).stdout
    count = 0
    for entry in tracked.split(b"\0"):
        if not entry:
            continue
        metadata, raw_name = entry.split(b"\t", 1)
        mode, _, stage = metadata.decode().split()
        name = raw_name.decode()
        require(stage == "0" and mode in {"100644", "100755"}, "ci_cache_tracked_type")
        # Each native closure covers its complete sources and transitive helpers;
        # unrelated application and CodeQL review changes cannot alter these checks.
        if scope.startswith("native-") and not native_security_input(name):
            continue
        if scope == "backend" and not backend_input(name):
            continue
        # A database stores evaluated queries, never a successful policy verdict.
        # Current exact reviews are checked after every fresh or reused analysis.
        if scope.startswith("codeql-") and name in CODEQL_POLICY_DATA:
            continue
        if scope == "codeql-native" and not native_codeql_input(name):
            continue
        if scope == "codeql-rust" and not rust_codeql_input(name):
            continue
        # Rust tests embed shared JSON fixtures from web/tests. Include all web
        # JSON configuration and data for Rust scopes; JS helper tests stay fresh.
        # Automation inspects frontend helpers, so it retains every tracked file.
        rust_data = scope in {"rust-lint", "rust-test", "backend", "codeql-rust"} and name.endswith(
            ".json"
        )
        if (
            scope not in {"automation", "codeql-rust", "backend"}
            and name.startswith("web/")
            and not rust_data
        ):
            continue
        path = root / name
        require(not path.is_symlink() and path.is_file(), "ci_cache_source_missing")
        content = (
            hashlib.sha256(backend_workflow(path)).hexdigest()
            if scope == "backend" and name == ".github/workflows/ci.yml"
            else digest(path)
        )
        hashed.update(raw_name + b"\0" + mode.encode() + b"\0" + content.encode())
        count += 1
    require(count > 0, "ci_cache_empty_inputs")
    return f"ci-verified-v1-{scope}-{identity['trust']}-{hashed.hexdigest()}"


def save(root: Path, scope: str, key: str, directory: Path) -> None:
    """Save only after the workflow's original check completed successfully."""
    require(not scope.startswith("codeql-"), "ci_cache_codeql_requires_database")
    require(cache_key(root, scope) == key, "ci_cache_inputs_changed")
    revision = os.environ.get("GITHUB_SHA", "")
    require(re.fullmatch(r"[a-f0-9]{40}", revision), "ci_cache_revision")
    if scope == "backend":
        for name in BINARIES:
            backend_dependencies(root, root / "target/debug", name, built=True)
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    artifacts: dict[str, str] = {}
    if scope == "backend":
        for name in (*BINARIES, *(name + ".d" for name in BINARIES)):
            source = root / "target/debug" / name
            artifacts[name] = digest(source)
            require(
                name.endswith(".d") or os.access(source, os.X_OK), "ci_cache_binary_not_executable"
            )
            destination = directory / name
            require(
                not destination.exists() and not destination.is_symlink(),
                "ci_cache_existing_binary",
            )
            _ = shutil.copyfile(source, destination)
            destination.chmod(0o700)
    with (directory / "receipt.json").open("x", encoding="utf-8") as target:
        _ = target.write(
            json.dumps({"schema": 1, "key": key, "revision": revision, "artifacts": artifacts})
            + "\n"
        )
    (directory / "receipt.json").chmod(0o600)


def verify(root: Path, scope: str, key: str, directory: Path) -> str:
    """Validate an exact restored success and every executable before installing it."""
    require(not scope.startswith("codeql-"), "ci_cache_codeql_requires_database")
    require(cache_key(root, scope) == key, "ci_cache_inputs_changed")
    receipt = record(
        cast("object", json.loads(bounded_file(directory / "receipt.json", 65536))),
        {"schema", "key", "revision", "artifacts"},
    )
    require(receipt["schema"] == 1 and receipt["key"] == key, "ci_cache_receipt_identity")
    revision = string(receipt["revision"])
    require(re.fullmatch(r"[a-f0-9]{40}", revision), "ci_cache_revision")
    names: set[str] = (
        {*BINARIES, *(name + ".d" for name in BINARIES)} if scope == "backend" else set()
    )
    artifacts = record(receipt["artifacts"], names)
    for name in names:
        require(digest(directory / name) == artifacts[name], "ci_cache_binary_changed")
    if scope == "backend":
        for name in BINARIES:
            backend_dependencies(root, directory, name, built=False)
    for name in BINARIES if scope == "backend" else ():
        destination = root / "target/debug" / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        require(not destination.is_symlink(), "ci_cache_binary_destination")
        _ = shutil.copyfile(directory / name, destination)
        destination.chmod(0o700)
    return revision


def main() -> int:
    """Expose a fixed scope and explicit receipt directory to the pinned cache action."""
    parser = argparse.ArgumentParser(description=__doc__)
    _ = parser.add_argument("operation", choices=("key", "save", "verify"))
    _ = parser.add_argument("--scope", choices=SCOPES, required=True)
    _ = parser.add_argument("--key", default="")
    _ = parser.add_argument("--directory", type=Path, default=Path())
    args = parser.parse_args(namespace=Options())
    try:
        scope, operation = string(args.scope), string(args.operation)
        if operation == "key":
            _ = sys.stdout.write(cache_key(ROOT, scope) + "\n")
        else:
            directory = Path(str(args.directory))
            require(directory.is_absolute() and not directory.is_symlink(), "ci_cache_directory")
            key = string(args.key)
            if operation == "save":
                save(ROOT, scope, key, directory)
            else:
                revision = verify(ROOT, scope, key, directory)
                _ = sys.stdout.write(
                    f"Reused {scope} success for identical inputs; original revision: {revision}\n"
                )
    except (ToolError, OSError, ValueError, subprocess.SubprocessError) as error:
        _ = sys.stderr.write(f"CI cache rejected: {error}\n")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
