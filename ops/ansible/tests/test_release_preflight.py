"""Offline prepared-host checks using disposable files, never installed helpers."""

from contextlib import ExitStack, redirect_stdout
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import stat
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


SOURCE = Path(__file__).resolve().parents[1] / 'files/release_preflight.py'
SPEC = importlib.util.spec_from_file_location('release_preflight_tests', SOURCE)
PREFLIGHT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PREFLIGHT)


class ReleasePreflightTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='simplestchat-preflight.')
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name).resolve()
        self.config = self.directory / 'etc/simplestchat-public'
        self.root = self.directory / 'srv/simplestchat-public'
        self.helpers = self.directory / 'usr/local/libexec/simplestchat-public'
        for path in (self.config, self.root, self.root / 'releases', self.helpers):
            path.mkdir(parents=True, exist_ok=True)
            path.chmod(0o755 if path == self.helpers else 0o700)
        for path in self.helpers.parents:
            if path != self.directory and self.directory in path.parents:
                path.chmod(0o755)
        self.selection = self.config / 'images.json'
        self.selection.write_bytes(b'PRIVATE_CONFIGURATION_CONTENT\n')
        self.selection.chmod(0o600)
        self.marker = self.directory / 'helper-was-executed'
        self.contents = (
            f'from pathlib import Path\nPath({str(self.marker)!r}).touch()\n'
            '# PRIVATE_INSTALLED_HELPER_CONTENT\n\n'
        ).encode()
        self.expected = {}
        for name in sorted(PREFLIGHT.BASE_HELPERS | {'fetch-release.py'}):
            path = self.helpers / name
            path.write_bytes(self.contents)
            path.chmod(0o644)
            self.expected[name] = hashlib.sha256(self.contents).hexdigest()

        self.original_lstat = Path.lstat
        self.metadata_overrides = {}
        # Only fixture ownership and its synthetic /usr ancestors are modeled.
        # Actual fixture permissions, types, sizes, and bytes remain under test.
        self.external_ancestors = set(self.directory.parents) | {self.directory}
        stack = ExitStack()
        self.addCleanup(stack.close)
        for name, value in (('CONFIG', self.config), ('ROOT', self.root), ('HELPERS', self.helpers)):
            stack.enter_context(patch.object(PREFLIGHT, name, value))
        stack.enter_context(patch.object(PREFLIGHT.os, 'geteuid', return_value=0))
        stack.enter_context(patch.object(PREFLIGHT.platform, 'system', return_value='Linux'))
        stack.enter_context(patch.object(PREFLIGHT.platform, 'machine', return_value='x86_64'))
        stack.enter_context(patch.object(PREFLIGHT.platform, 'freedesktop_os_release',
                                        return_value={'ID': 'debian', 'VERSION_ID': '13'}))
        stack.enter_context(patch.object(Path, 'lstat', lambda path: self.fixture_lstat(path)))

    def fixture_lstat(self, path):
        path = Path(path)
        metadata = self.original_lstat(path)
        values = {name: getattr(metadata, name) for name in ('st_uid', 'st_gid', 'st_mode', 'st_size')}
        if path in self.external_ancestors:
            values.update(st_uid=0, st_gid=0, st_mode=stat.S_IFDIR | 0o755)
        elif self.directory in path.parents:
            values.update(st_uid=0, st_gid=0)
        values.update(self.metadata_overrides.get(path, {}))
        return SimpleNamespace(**values)

    def snapshot(self):
        result = {}
        for path in (self.directory, *sorted(self.directory.rglob('*'))):
            value = self.original_lstat(path)
            result[str(path.relative_to(self.directory))] = (
                value.st_mode, value.st_uid, value.st_gid, value.st_ino, value.st_size,
                value.st_mtime_ns, value.st_ctime_ns,
                path.read_bytes() if stat.S_ISREG(value.st_mode) else None,
            )
        return result

    def run_main(self, arguments):
        output = io.StringIO()
        with patch.object(sys, 'argv', ['release_preflight.py', *arguments]), redirect_stdout(output):
            code = PREFLIGHT.main()
        return code, output.getvalue()

    def test_complete_base_and_github_sets_accept_exact_raw_source_bytes(self):
        for expected in ({name: self.expected[name] for name in PREFLIGHT.BASE_HELPERS}, self.expected):
            with self.subTest(names=sorted(expected)):
                result = PREFLIGHT.check(expected)
                self.assertEqual(set(result), {'schemaVersion', 'runId', 'prepared'})
                self.assertEqual(result['schemaVersion'], 1)
                self.assertIs(result['prepared'], True)
                self.assertRegex(result['runId'], r'^[a-f0-9]{32}$')
        trimmed = dict(self.expected, **{'release-public.py': hashlib.sha256(self.contents.rstrip()).hexdigest()})
        with self.assertRaisesRegex(PREFLIGHT.PreflightError, '^helper_digest_mismatch$'):
            PREFLIGHT.check(trimmed)

    def test_success_is_read_only_does_not_execute_helpers_and_has_fresh_run_ids(self):
        before = self.snapshot()
        first = PREFLIGHT.check(self.expected)
        second = PREFLIGHT.check(self.expected)
        self.assertNotEqual(first['runId'], second['runId'])
        self.assertEqual(self.snapshot(), before)
        self.assertFalse(self.marker.exists())
        self.assertFalse(any(path.name == '__pycache__' for path in self.directory.rglob('*')))

    def test_normal_mode_supports_initial_helper_and_storage_reconciliation(self):
        for path in self.helpers.iterdir():
            path.unlink()
        self.helpers.rmdir()
        (self.root / 'releases').rmdir()
        before = self.snapshot()
        result = PREFLIGHT.check({})
        self.assertIs(result['prepared'], False)
        self.assertEqual(self.snapshot(), before)

    def test_helper_set_must_be_exact_and_digest_shape_is_strict(self):
        for expected in (None, [], True, {'release-public.py': 'a' * 64},
                         {**self.expected, '../extra.py': 'a' * 64},
                         {name: value for name, value in self.expected.items() if name != 'reboot-public.py'}):
            with self.subTest(expected=expected), self.assertRaisesRegex(PREFLIGHT.PreflightError, '^invalid_helper_set$'):
                PREFLIGHT.check(expected)
        for value in (None, True, '', 'a' * 63, 'A' * 64, 'g' * 64, 'a' * 64 + '\n'):
            with self.subTest(value=value), self.assertRaisesRegex(PREFLIGHT.PreflightError, '^invalid_helper_digest$'):
                PREFLIGHT.check(dict(self.expected, **{'release-public.py': value}))

    def test_missing_required_paths_are_rejected(self):
        for path in (self.helpers / 'release-public.py', self.root / 'releases', self.helpers,
                     self.selection, self.config, self.root):
            retained = path.with_name(path.name + '.retained')
            path.rename(retained)
            try:
                with self.subTest(path=path), self.assertRaises(FileNotFoundError):
                    PREFLIGHT.check(self.expected)
            finally:
                retained.rename(path)

    def test_same_size_changed_helper_is_rejected(self):
        path = self.helpers / 'release-public.py'
        path.write_bytes(self.contents.replace(b'PRIVATE', b'CHANGED', 1))
        self.assertEqual(path.stat().st_size, len(self.contents))
        with self.assertRaisesRegex(PREFLIGHT.PreflightError, '^helper_digest_mismatch$'):
            PREFLIGHT.check(self.expected)

    def test_wrong_directory_file_and_selection_modes_are_rejected(self):
        paths = (self.config, self.root, self.root / 'releases', self.helpers,
                 self.helpers.parent, self.helpers / 'release-public.py', self.selection)
        for path in paths:
            previous = stat.S_IMODE(self.original_lstat(path).st_mode)
            path.chmod(previous | 0o002)
            try:
                with self.subTest(path=path), self.assertRaisesRegex(PREFLIGHT.PreflightError, '^unsafe_release_path$'):
                    PREFLIGHT.check(self.expected)
            finally:
                path.chmod(previous)

    def test_nonroot_owner_or_group_is_rejected(self):
        for path in (self.config, self.root, self.helpers, self.helpers.parent,
                     self.helpers / 'release-public.py', self.selection):
            for field in ('st_uid', 'st_gid'):
                self.metadata_overrides[path] = {field: 501}
                try:
                    with self.subTest(path=path, field=field), \
                            self.assertRaisesRegex(PREFLIGHT.PreflightError, '^unsafe_release_path$'):
                        PREFLIGHT.check(self.expected)
                finally:
                    self.metadata_overrides.clear()

    def test_symlink_files_and_intermediate_helper_directory_are_rejected(self):
        for path in (self.helpers / 'release-public.py', self.selection, self.helpers.parent):
            retained = path.with_name(path.name + '.retained')
            path.rename(retained)
            path.symlink_to(retained, target_is_directory=retained.is_dir())
            try:
                with self.subTest(path=path), self.assertRaisesRegex(PREFLIGHT.PreflightError, '^unsafe_release_path$'):
                    PREFLIGHT.check(self.expected)
            finally:
                path.unlink()
                retained.rename(path)

    def test_directories_cannot_substitute_for_helper_or_selection_files(self):
        for path in (self.helpers / 'release-public.py', self.selection):
            contents = path.read_bytes()
            mode = stat.S_IMODE(self.original_lstat(path).st_mode)
            path.unlink()
            path.mkdir(mode=mode)
            try:
                with self.subTest(path=path), self.assertRaisesRegex(PREFLIGHT.PreflightError, '^unsafe_release_path$'):
                    PREFLIGHT.check(self.expected)
            finally:
                path.rmdir()
                path.write_bytes(contents)
                path.chmod(mode)

    def test_empty_or_oversized_helper_and_selection_are_rejected(self):
        for path, limit in ((self.helpers / 'release-public.py', PREFLIGHT.MAX_HELPER), (self.selection, 65536)):
            contents = path.read_bytes()
            for size in (0, limit + 1):
                path.write_bytes(b'x' * size)
                try:
                    with self.subTest(path=path, size=size), \
                            self.assertRaisesRegex(PREFLIGHT.PreflightError, '^invalid_release_file_size$'):
                        PREFLIGHT.check(self.expected)
                finally:
                    path.write_bytes(contents)

    def test_root_linux_architecture_and_debian_major_are_required(self):
        cases = ((PREFLIGHT.os, 'geteuid', 501, 'root_required'),
                 (PREFLIGHT.platform, 'system', 'Darwin', 'unsupported_platform'),
                 (PREFLIGHT.platform, 'machine', 'aarch64', 'unsupported_platform'),
                 (PREFLIGHT.platform, 'freedesktop_os_release', {'ID': 'ubuntu', 'VERSION_ID': '13'}, 'unsupported_distribution'),
                 (PREFLIGHT.platform, 'freedesktop_os_release', {'ID': 'debian', 'VERSION_ID': '12'}, 'unsupported_distribution'),
                 (PREFLIGHT.platform, 'freedesktop_os_release', {'ID': 'debian'}, 'unsupported_distribution'))
        for target, name, value, failure in cases:
            with self.subTest(name=name, value=value), patch.object(target, name, return_value=value), \
                    self.assertRaisesRegex(PREFLIGHT.PreflightError, '^' + failure + '$'):
                PREFLIGHT.check(self.expected)
        with patch.object(PREFLIGHT.platform, 'freedesktop_os_release', return_value={'ID': 'debian', 'VERSION_ID': '13.1'}):
            self.assertTrue(PREFLIGHT.check(self.expected)['prepared'])

    def test_main_success_returns_only_public_receipt_without_file_contents(self):
        before = self.snapshot()
        code, output = self.run_main([json.dumps(self.expected)])
        self.assertEqual(code, 0)
        value = json.loads(output)
        self.assertEqual(set(value), {'schemaVersion', 'runId', 'prepared'})
        self.assertTrue(value['prepared'])
        self.assertNotIn('PRIVATE', output)
        self.assertNotIn(str(self.directory), output)
        self.assertEqual(self.snapshot(), before)

    def test_main_invalid_requests_and_filesystem_failures_are_coarse(self):
        for arguments, failure in (([], 'invalid_request'), (['{}', '{}'], 'invalid_request'),
                                   (['x' * 4097], 'invalid_request'), (['PRIVATE_INVALID_JSON'], 'preflight_unavailable'),
                                   (['[]'], 'invalid_helper_set')):
            with self.subTest(arguments=arguments[:1]):
                code, output = self.run_main(arguments)
                self.assertEqual(code, 1)
                self.assertEqual(json.loads(output), {'schemaVersion': 1, 'passed': False, 'failure': failure})
                self.assertNotIn('PRIVATE', output)
        with patch.object(PREFLIGHT, 'protected', side_effect=OSError('PRIVATE_FILESYSTEM_DETAIL')):
            code, output = self.run_main(['{}'])
        self.assertEqual(code, 1)
        self.assertEqual(json.loads(output)['failure'], 'preflight_unavailable')
        self.assertNotIn('PRIVATE', output)


if __name__ == '__main__':
    unittest.main()
