"""Changed dependency licenses must retain exact locks, evidence and policy semantics."""

from __future__ import annotations

import hashlib
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from typing import cast
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "build"))

import security_dependency_licenses as licenses
from security_context import Context
from security_tools import ToolError

# isort: split
import bounded_process
from release_json import array_value, decode_json, object_value, string_value


def lock(expression: str = "MIT", version: str = "1.0.0") -> bytes:
    """Produce a nested development dependency with a fixed registry identity."""
    return json.dumps(
        {
            "lockfileVersion": 3,
            "packages": {
                "": {},
                "node_modules/fixture": {
                    "version": version,
                    "license": expression,
                    "dev": True,
                    "resolved": "https://registry.npmjs.org/fixture/-/fixture-1.0.0.tgz",
                    "integrity": "sha512-fixture",
                },
            },
        }
    ).encode()


def python_dependency() -> licenses.Dependency:
    """Represent an exact wheel pin with a marker irrelevant to the host platform."""
    return licenses.python_requirement(
        "requirements.txt", 'fixture==1.0.0; sys_platform == "linux" --hash=sha256:' + "a" * 64
    )


def metadata(expression: str = "MIT") -> bytes:
    """Create standards-shaped metadata without installing or importing a package."""
    return (
        "Metadata-Version: 2.4\nName: fixture\nVersion: 1.0.0\n"
        + f"License-Expression: {expression}\n\n"
    ).encode()


def index(content: bytes, wheel_hash: str = "a" * 64) -> bytes:
    """Bind a public wheel identity to its authenticated metadata sidecar."""
    return json.dumps(
        {
            "name": "fixture",
            "files": [
                {
                    "filename": "fixture-1.0.0-py3-none-any.whl",
                    "hashes": {"sha256": wheel_hash},
                    "url": "https://files.pythonhosted.org/packages/fixture-1.0.0-py3-none-any.whl",
                    "core-metadata": {"sha256": hashlib.sha256(content).hexdigest()},
                }
            ],
        }
    ).encode()


class DependencyLicenseTests(unittest.TestCase):
    """The local and hosted gate inspect the same precise changed declarations."""

    def context(self, root: Path) -> Context:
        """Create private report directories and the existing license allowlist."""
        output = root / "output"
        output.mkdir()
        _ = (root / "deny.toml").write_text(
            '[licenses]\nallow = ["MIT", "Apache-2.0 WITH LLVM-exception"]\n'
        )
        return Context(root, output)

    def test_npm_keeps_development_license_source_and_integrity_changes(self) -> None:
        """Changing metadata at the same version cannot evade the diff check."""
        initial = licenses.npm_dependencies("web/package-lock.json", lock())
        self.assertEqual(len(initial), 1)
        self.assertNotEqual(
            initial, licenses.npm_dependencies("web/package-lock.json", lock("GPL-3.0-only"))
        )
        self.assertNotEqual(
            initial, licenses.npm_dependencies("web/package-lock.json", lock(version="2.0.0"))
        )
        self.assertNotEqual(
            initial,
            licenses.npm_dependencies(
                "web/package-lock.json", lock().replace(b"sha512-fixture", b"sha512-other")
            ),
        )
        self.assertNotEqual(
            initial,
            licenses.npm_dependencies(
                "web/package-lock.json", lock().replace(b"registry.npmjs.org", b"example.org")
            ),
        )

    def test_npm_rejects_links_old_lock_formats_and_untyped_license(self) -> None:
        """Unsupported dependency contracts fail instead of being silently omitted."""
        for content in (
            lock().replace(b'"lockfileVersion": 3', b'"lockfileVersion": 2'),
            lock().replace(b'"dev": true', b'"link": true'),
            lock().replace(b'"MIT"', b"null"),
        ):
            with self.subTest(content=content), self.assertRaises((ToolError, ValueError)):
                _ = licenses.npm_dependencies("web/package-lock.json", content)

    def test_python_follows_includes_and_retains_cross_platform_markers(self) -> None:
        """Included hashes and markers remain part of the source identity on every host."""
        pin = b'fixture==1.0.0; sys_platform == "linux" \\\n --hash=sha256:' + b"a" * 64
        files = {
            "build/requirements.txt": b"--require-hashes\n--only-binary=:all:\n"
            + b"-r ../ops/requirements.txt\n",
            "ops/requirements.txt": pin,
        }
        result = licenses.python_dependencies("build/requirements.txt", files.get)
        self.assertEqual(len(result), 1)
        self.assertEqual(next(iter(result)).manifest, "ops/requirements.txt")
        changed = {**files, "ops/requirements.txt": pin.replace(b"linux", b"win32")}
        self.assertNotEqual(
            result, licenses.python_dependencies("build/requirements.txt", changed.get)
        )

    def test_python_rejects_unpinned_url_missing_hash_and_invalid_includes(self) -> None:
        """No alternate resolver or include outside the repository is accepted."""
        for line in (
            "fixture>=1 --hash=sha256:" + "a" * 64,
            "fixture==1.0.0",
            "fixture @ https://example.org/a.whl --hash=sha256:" + "a" * 64,
            "fixture==1.0.0 --hash=sha256:bad",
        ):
            with self.subTest(line=line), self.assertRaises((ToolError, ValueError)):
                _ = licenses.python_requirement("requirements.txt", line)
        for include in (b"-r ../outside.txt", b"-r requirements.txt", b"-r missing.txt"):
            with self.subTest(include=include), self.assertRaises(ToolError):
                _ = licenses.python_dependencies(
                    "requirements.txt", {"requirements.txt": include}.get
                )

    def test_changed_unmanaged_locks_require_shared_audit_support(self) -> None:
        """A new first-party project cannot evade the maintained audit graphs."""
        empty: dict[str, bytes] = {}
        for path in (
            "other/package-lock.json",
            "other/requirements-dev.txt",
            "other/uv.lock",
            "other/pylock.dev.toml",
        ):
            with self.subTest(path=path):
                previous = {path: b"old"}
                licenses.require_audited_locks({path}, previous.get, previous.get)
                licenses.require_audited_locks({path}, empty.get, previous.get)
                with self.assertRaisesRegex(ToolError, "dependency_unaudited_lock_changed"):
                    licenses.require_audited_locks({path}, {path: b"new"}.get, previous.get)
                with self.assertRaisesRegex(ToolError, "dependency_unaudited_lock_changed"):
                    licenses.require_audited_locks({path}, previous.get, empty.get)

    def test_unused_vendor_locks_follow_optional_local_source_scan_scope(self) -> None:
        """Upstream development locks are optional; installed build graphs remain audited."""
        path = "vendor/upstream/scripts/package-lock.json"
        current = {path: b"new"}
        previous: dict[str, bytes] = {}
        licenses.require_audited_locks({path}, current.get, previous.get)
        with (
            patch.dict(os.environ, {}, clear=True),
            self.assertRaisesRegex(ToolError, "dependency_unaudited_lock_changed"),
        ):
            licenses.require_audited_locks({path}, current.get, previous.get, include_vendor=True)
        self.assertIn(
            "vendor/mediasoup-sys-0.19.0/python-tools-requirements.txt", licenses.PYTHON_LOCKS
        )

    def test_python_requires_exact_metadata_identity_and_explicit_expression(self) -> None:
        """Free text, duplicate fields and another release never become license evidence."""
        self.assertEqual(licenses.metadata_license(metadata(), python_dependency()), "MIT")
        for content in (
            metadata().replace(b"Name: fixture", b"Name: other"),
            metadata().replace(b"Version: 1.0.0", b"Version: 2.0.0"),
            metadata().replace(b"License-Expression: MIT", b"License: MIT"),
            metadata().replace(
                b"License-Expression: MIT", b"License-Expression: MIT\nLicense-Expression: MIT"
            ),
        ):
            with self.subTest(content=content), self.assertRaises(ToolError):
                _ = licenses.metadata_license(content, python_dependency())

    def test_python_sidecar_hash_and_wheel_binding_are_required(self) -> None:
        """A passing declaration is useful only for an actual locked wheel."""
        with tempfile.TemporaryDirectory() as directory:
            registry = licenses.Registry(self.context(Path(directory)))
            with patch.object(
                licenses.Registry, "fetch", side_effect=[index(metadata()), metadata()]
            ):
                evidence = licenses.python_licenses(python_dependency(), registry)
            self.assertEqual(evidence[0]["wheelSha256"], "a" * 64)
            for replies in (
                [index(metadata()), metadata("Apache-2.0")],
                [index(metadata(), "b" * 64)],
                [index(metadata()).replace(b"files.pythonhosted.org", b"example.org")],
            ):
                with (
                    patch.object(licenses.Registry, "fetch", side_effect=replies),
                    self.assertRaises(ToolError),
                ):
                    _ = licenses.python_licenses(python_dependency(), registry)

    def test_every_python_hash_resolves_to_the_exact_package_release(self) -> None:
        """An allowed wheel cannot conceal an unknown or different-version hash."""
        dependency = licenses.python_requirement(
            "requirements.txt",
            "fixture==1.0.0 --hash=sha256:" + "a" * 64 + " --hash=sha256:" + "b" * 64,
        )
        for change in (None, "fixture-2.0.0-py3-none-any.whl", "other-1.0.0-py3-none-any.whl"):
            raw = object_value(decode_json(index(metadata())))
            if change is not None:
                second = object_value(array_value(raw["files"])[0]).copy()
                second.update(
                    filename=change,
                    hashes={"sha256": "b" * 64},
                    url="https://files.pythonhosted.org/packages/" + change,
                )
                array_value(raw["files"]).append(second)
            with self.subTest(change=change), tempfile.TemporaryDirectory() as directory:
                registry = licenses.Registry(self.context(Path(directory)))
                with (
                    patch.object(
                        licenses.Registry,
                        "fetch",
                        side_effect=[json.dumps(raw).encode(), metadata()],
                    ),
                    self.assertRaises(ToolError),
                ):
                    _ = licenses.python_licenses(dependency, registry)

    def test_exact_sdist_hashes_are_explicitly_excluded_by_wheel_only_policy(self) -> None:
        """Source archives are accounted for without being installed or claimed as reviewed."""
        raw = object_value(decode_json(index(metadata())))
        array_value(raw["files"]).append(
            {
                "filename": "fixture-1.0.0.tar.gz",
                "hashes": {"sha256": "b" * 64},
                "url": "https://files.pythonhosted.org/packages/fixture-1.0.0.tar.gz",
            }
        )
        dependency = licenses.python_requirement(
            "requirements.txt",
            "fixture==1.0.0 --hash=sha256:" + "a" * 64 + " --hash=sha256:" + "b" * 64,
        )
        with tempfile.TemporaryDirectory() as directory:
            registry = licenses.Registry(self.context(Path(directory)))
            with patch.object(
                licenses.Registry, "fetch", side_effect=[json.dumps(raw).encode(), metadata()]
            ) as fetch:
                evidence = licenses.python_licenses(dependency, registry)
            self.assertEqual(fetch.call_count, 2)
            self.assertEqual(evidence[1]["kind"], "sdist")
            self.assertEqual(evidence[1]["artifactSha256"], "b" * 64)
            self.assertEqual(evidence[1]["excluded"], "wheel-only-installation")
            self.assertNotIn("license", evidence[1])
            raw["files"] = [array_value(raw["files"])[1]]
            with (
                patch.object(licenses.Registry, "fetch", return_value=json.dumps(raw).encode()),
                self.assertRaises(ToolError),
            ):
                _ = licenses.python_licenses(dependency, registry)

    def test_source_archive_exclusion_requires_existing_wheel_only_installers(self) -> None:
        """Removing the binary-only restriction fails even when package versions stay fixed."""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "ops/ansible").mkdir(parents=True)
            requirement = root / "ops/ansible/requirements.txt"
            _ = requirement.write_text("--require-hashes\n--only-binary=:all:\n")
            licenses.require_wheel_only(root)
            _ = requirement.write_text("--require-hashes\n")
            with self.assertRaisesRegex(ToolError, "dependency_python_wheel_only_policy"):
                licenses.require_wheel_only(root)
            requirement.unlink()
            (root / "security").mkdir()
            _ = (root / "security/requirements.txt").write_text("# pinned scanner tools\n")
            (root / "build").mkdir()
            installer = root / "build/check-security.sh"
            _ = installer.write_text(
                "pip install --require-hashes --only-binary=:all: "
                + "--requirement security/requirements.txt\n"
            )
            licenses.require_wheel_only(root)
            _ = installer.write_text(
                "pip install --require-hashes --requirement security/requirements.txt\n"
            )
            with self.assertRaisesRegex(ToolError, "dependency_security_wheel_only_policy"):
                licenses.require_wheel_only(root)

    def test_every_platform_wheel_license_is_checked(self) -> None:
        """An allowed universal wheel cannot hide a differently licensed native wheel."""
        first = object_value(decode_json(index(metadata())))
        second = object_value(
            array_value(
                object_value(decode_json(index(metadata("GPL-3.0-only"), "b" * 64)))["files"]
            )[0]
        )
        second["filename"] = "fixture-1.0.0-cp312-cp312-manylinux_2_17_x86_64.whl"
        second["url"] = "https://files.pythonhosted.org/packages/" + string_value(
            second["filename"]
        )
        array_value(first["files"]).append(second)
        dependency = licenses.python_requirement(
            "requirements.txt",
            "fixture==1.0.0 --hash=sha256:" + "a" * 64 + " --hash=sha256:" + "b" * 64,
        )
        with tempfile.TemporaryDirectory() as directory:
            context = self.context(Path(directory))
            with (
                patch.object(
                    licenses.Registry,
                    "fetch",
                    side_effect=[json.dumps(first).encode(), metadata(), metadata("GPL-3.0-only")],
                ),
                patch.object(
                    licenses, "changed_dependencies", return_value=("b" * 40, {dependency})
                ),
                self.assertRaisesRegex(ToolError, "dependency_license_not_allowed"),
            ):
                licenses.check(context, context.root, "b" * 40)
            self.assertIn(
                b"GPL-3.0-only", (context.output / "dependency-licenses.json").read_bytes()
            )

    def test_npm_registry_metadata_must_match_lock_exactly(self) -> None:
        """Registry licenses cannot be substituted by editing the lock's license field."""
        dependency = next(iter(licenses.npm_dependencies("web/package-lock.json", lock())))
        value = {
            "name": "fixture",
            "version": "1.0.0",
            "license": "MIT",
            "dist": {"tarball": dependency.source, "integrity": "sha512-fixture"},
        }
        with tempfile.TemporaryDirectory() as directory:
            registry = licenses.Registry(self.context(Path(directory)))
            with patch.object(licenses.Registry, "fetch", return_value=json.dumps(value).encode()):
                self.assertEqual(licenses.npm_license(dependency, registry)[0]["license"], "MIT")
            for change in (
                {"license": "GPL-3.0-only"},
                {"version": "2.0.0"},
                {"dist": {"tarball": dependency.source, "integrity": "sha512-other"}},
            ):
                with (
                    patch.object(
                        licenses.Registry,
                        "fetch",
                        return_value=json.dumps({**value, **change}).encode(),
                    ),
                    self.assertRaises(ToolError),
                ):
                    _ = licenses.npm_license(dependency, registry)

    def test_no_changes_need_no_network_and_still_write_evidence(self) -> None:
        """An unchanged lock graph is a measured zero-work check, not a skipped gate."""
        with tempfile.TemporaryDirectory() as directory:
            context = self.context(Path(directory))
            with (
                patch.object(licenses, "changed_dependencies", return_value=("b" * 40, set())),
                patch.object(licenses.Registry, "fetch") as fetch,
            ):
                licenses.check(context, context.root, None)
            fetch.assert_not_called()
            self.assertEqual(context.checks[0]["changedDependencies"], 0)
            self.assertTrue(
                json.loads((context.output / "dependency-licenses.json").read_text())["passed"]
            )

    def test_working_snapshot_is_compared_to_the_resolved_ancestor(self) -> None:
        """Both an explicit publication base and HEAD iteration inspect unstaged changes."""
        for base in (None, "b" * 40):
            with self.subTest(base=base), tempfile.TemporaryDirectory() as directory:
                context = self.context(Path(directory))
                (context.root / "web").mkdir()
                _ = (context.root / "web/package-lock.json").write_bytes(lock(version="2.0.0"))
                commands: list[list[str]] = []

                def run(
                    _name: str, argv: list[str], recorded: list[list[str]] = commands
                ) -> tuple[int, bytes]:
                    recorded.append(argv)
                    if "rev-parse" in argv:
                        return 0, ("b" * 40 + "\n").encode()
                    if "ls-tree" in argv:
                        return 0, b"web/package-lock.json\0"
                    if "show" in argv:
                        return 0, lock()
                    return 0, b""

                with patch.object(context, "run", side_effect=run):
                    resolved, changed = licenses.changed_dependencies(context, context.root, base)
                self.assertEqual(resolved, "b" * 40)
                self.assertEqual({item.version for item in changed}, {"2.0.0"})
                self.assertEqual(commands[0][-1], (base or "HEAD") + "^{commit}")
                self.assertIn(
                    ["merge-base", "--is-ancestor", "b" * 40, "HEAD"],
                    [argv[1:] for argv in commands],
                )

    def test_bad_base_and_nonancestor_cannot_become_an_empty_diff(self) -> None:
        """An invalid comparison fails rather than claiming there are no changed packages."""
        with tempfile.TemporaryDirectory() as directory:
            context = self.context(Path(directory))
            with (
                patch.object(context, "run") as run,
                self.assertRaisesRegex(ToolError, "dependency_diff_base"),
            ):
                _ = licenses.changed_dependencies(context, context.root, "HEAD~1")
            run.assert_not_called()
            with (
                patch.object(
                    context, "run", side_effect=[(0, ("b" * 40).encode()), ToolError("nonancestor")]
                ),
                self.assertRaisesRegex(ToolError, "nonancestor"),
            ):
                _ = licenses.changed_dependencies(context, context.root, "b" * 40)

    def test_identical_wheel_metadata_is_fetched_once_for_all_platforms(self) -> None:
        """Deduplication saves network work while preserving both wheel identities."""
        raw = object_value(decode_json(index(metadata())))
        files = array_value(raw["files"])
        second = object_value(files[0]).copy()
        second["filename"] = "fixture-1.0.0-cp312-cp312-manylinux_2_17_x86_64.whl"
        second["url"] = "https://files.pythonhosted.org/packages/" + string_value(
            second["filename"]
        )
        files.append(second)
        with tempfile.TemporaryDirectory() as directory:
            registry = licenses.Registry(self.context(Path(directory)))
            with patch.object(
                licenses.Registry, "fetch", side_effect=[json.dumps(raw).encode(), metadata()]
            ) as fetch:
                evidence = licenses.python_licenses(python_dependency(), registry)
            self.assertEqual(len(evidence), 2)
            self.assertEqual(fetch.call_count, 2)

    def test_spdx_choices_conjunctions_exceptions_and_invalid_grammar(self) -> None:
        """The existing policy parser remains strict while preserving SPDX choices."""
        for expression, allowed in (
            ("MIT OR GPL-3.0-only", True),
            ("MIT AND GPL-3.0-only", False),
            ("Apache-2.0 WITH LLVM-exception", True),
            ("Apache-2.0 WITH Other-exception", False),
            ("LicenseRef-unknown", False),
            ("MIT OR (GPL-3.0-only", False),
        ):
            with self.subTest(expression=expression), tempfile.TemporaryDirectory() as directory:
                context = self.context(Path(directory))
                dependency = next(
                    iter(licenses.npm_dependencies("web/package-lock.json", lock(expression)))
                )
                with (
                    patch.object(
                        licenses, "changed_dependencies", return_value=("b" * 40, {dependency})
                    ),
                    patch.object(licenses, "npm_license", return_value=[{"license": expression}]),
                ):
                    if allowed:
                        licenses.check(context, context.root, None)
                    else:
                        with self.assertRaises(ToolError):
                            licenses.check(context, context.root, None)

    def test_fetch_uses_fixed_origins_deadlines_and_no_redirects(self) -> None:
        """Neither metadata redirects nor ambient curl configuration are accepted."""
        with tempfile.TemporaryDirectory() as directory:
            registry = licenses.Registry(self.context(Path(directory)))
            with (
                patch.object(licenses, "executable", return_value="curl"),
                patch.object(bounded_process, "run", return_value=(0, b"{}\n200", b"")) as run,
            ):
                self.assertEqual(registry.fetch("https://pypi.org/simple/fixture/"), b"{}")
            arguments = cast("list[str]", run.call_args.args[0])
            self.assertEqual(arguments[1], "--disable")
            self.assertNotIn("--location", arguments)
            self.assertEqual(run.call_args.kwargs["env"], registry.context.env)
            for url in (
                "http://pypi.org/simple/fixture/",
                "https://pypi.org.example.org/a",
                "https://pypi.org/a?token=x",
            ):
                with self.subTest(url=url), self.assertRaises(ToolError):
                    _ = registry.fetch(url)
            registry.started -= licenses.MAX_SECONDS
            with self.assertRaisesRegex(ToolError, "dependency_registry_budget"):
                _ = registry.fetch("https://pypi.org/simple/other/")

    def test_failed_http_empty_and_oversized_responses_never_enter_cache(self) -> None:
        """A redirect, partial response or permissive mocked transport cannot pass."""
        for response in (
            (22, b"{}\n200", b""),
            (0, b"{}\n301", b""),
            (0, b"\n200", b""),
            (0, b"large\n200", b""),
        ):
            with self.subTest(response=response), tempfile.TemporaryDirectory() as directory:
                registry = licenses.Registry(self.context(Path(directory)))
                with (
                    patch.object(bounded_process, "run", return_value=response),
                    self.assertRaises(ToolError),
                ):
                    _ = registry.fetch("https://pypi.org/simple/fixture/", 2)
                self.assertFalse(registry.cache)
