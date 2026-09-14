"""Offline fetch protocol, artifact validation and private publication tests."""

from copy import deepcopy
import fcntl
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import signal
import stat
import struct
import sys
import tarfile
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
from urllib.error import HTTPError, URLError
import warnings
import zipfile

FILES = Path(__file__).resolve().parents[1] / 'files'
SPEC = importlib.util.spec_from_file_location('release_fetch_tests', FILES / 'fetch-release.py')
FETCH = importlib.util.module_from_spec(SPEC)
with patch.object(sys, 'path', [str(FILES), *sys.path]):
    SPEC.loader.exec_module(FETCH)

REVISION = 'a' * 40
URL = 'https://productionresultssa6.blob.core.windows.net/path/artifact.zip?sig=PRIVATE_SIGNED_URL'


def envelope(data=b'fixture'):
    return {'schemaVersion': 1, 'repository': 'owner/repo', 'artifactId': 123, 'buildRunId': 456,
            'ciRunId': 789, 'revision': REVISION, 'artifactZipBytes': len(data),
            'zipSha256': hashlib.sha256(data).hexdigest()}


def fixture_files():
    image = io.BytesIO()
    config = {'architecture': 'amd64', 'os': 'linux', 'config': {
        'User': '10001:10001', 'Cmd': ['/app/simplestChat'],
        'Labels': {'org.opencontainers.image.revision': REVISION},
    }}
    files = {
        'manifest.json': json.dumps([{'Config': 'config.json', 'RepoTags': [f'simplestchat-release/production:{REVISION}'],
                                      'Layers': ['layer.tar']}]).encode(),
        'config.json': json.dumps(config).encode(), 'layer.tar': b'disposable fixture layer, never executed',
    }
    with tarfile.open(fileobj=image, mode='w') as archive:
        for name, data in files.items():
            member = tarfile.TarInfo(name)
            member.size = len(data)
            archive.addfile(member, io.BytesIO(data))
    manifest = {'schemaVersion': 1, 'revision': REVISION, 'platform': 'linux/amd64',
                'archiveSha256': hashlib.sha256(image.getvalue()).hexdigest(),
                'imageTag': f'simplestchat-release/production:{REVISION}',
                'migrations': {'1': hashlib.sha384(b'SELECT 1;').hexdigest()}, 'createdAt': '2026-09-13T00:00:00Z'}
    outcome = {'schemaVersion': 1, 'revision': REVISION, 'passed': True, 'error': None}
    source = {'revision': REVISION, 'inputsSha256': {name: hashlib.sha256(name.encode()).hexdigest()
                                                 for name in FETCH.SOURCE_KEYS}}
    return {'image.tar': image.getvalue(), 'release.json': json.dumps(manifest).encode(),
            'outcome.json': json.dumps(outcome).encode(), 'source.json': json.dumps(source).encode()}


def make_zip(files=None, extras=()):
    data = io.BytesIO()
    with warnings.catch_warnings(), zipfile.ZipFile(data, 'w', compression=zipfile.ZIP_DEFLATED) as archive:
        warnings.simplefilter('ignore', UserWarning)
        for name, value in (files or fixture_files()).items():
            archive.writestr(name, value)
        for name, value in extras:
            archive.writestr(name, value)
    return data.getvalue()


class Response(io.BytesIO):
    def __init__(self, data, *, status=200, headers=None):
        super().__init__(data)
        self.status = status
        self.headers = headers if headers is not None else {'Content-Length': str(len(data))}


class FetchTestCase(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='simplestchat-release-fetch.')
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name).resolve()
        original_umask = os.umask(0o077)
        self.addCleanup(os.umask, original_umask)


class FetchProtocolTests(FetchTestCase):
    def test_public_envelope_is_exact_and_identifiers_are_canonical(self):
        self.assertEqual(FETCH.validate_envelope(envelope()), envelope())
        for key, value in (('schemaVersion', True), ('repository', '../repo'), ('repository', 'owner/repo/other'),
                           ('artifactId', 0), ('buildRunId', True), ('ciRunId', -1), ('revision', 'main'),
                           ('artifactZipBytes', FETCH.MAX_ZIP_BYTES + 1), ('zipSha256', 'A' * 64)):
            with self.subTest(key=key, value=value), self.assertRaises(FETCH.FetchError):
                FETCH.validate_envelope(dict(envelope(), **{key: value}))
        with self.assertRaises(FETCH.FetchError):
            FETCH.validate_envelope(dict(envelope(), url=URL))

    def test_line_protocol_is_bounded_terminated_unique_and_finite_json(self):
        self.assertEqual(FETCH.read_line(io.BytesIO(b'{"field":1}\n')), {'field': 1})
        for data in (b'', b'{}', b'[]\n', b'{"url":"one","url":"two"}\n',
                     b'{"field":NaN}\n', b' ' * FETCH.LINE_BYTES + b'{}\n'):
            with self.subTest(data=data[:40]), self.assertRaises((FETCH.FetchError, ValueError)):
                FETCH.read_line(io.BytesIO(data))

    def test_url_allows_one_blob_account_label_and_rejects_other_authorities(self):
        self.assertEqual(FETCH.validate_url({'url': URL}), URL)
        other_account = URL.replace('productionresultssa6', 'github-artifacts')
        self.assertEqual(FETCH.validate_url({'url': other_account}), other_account)
        for url in (URL.replace('https:', 'http:'), URL.replace('.blob.core.windows.net', '.blob.core.windows.net.example.com'),
                    URL.replace('productionresultssa6', 'other.productionresultssa6'),
                    URL.replace('https://', 'https://user@'), URL.replace('.net/', '.net:443/'),
                    URL + '#fragment', URL + '\n', URL.replace('?sig=PRIVATE_SIGNED_URL', ''),
                    'https://127.0.0.1/path?sig=x'):
            with self.subTest(url=url), self.assertRaises(FETCH.FetchError):
                FETCH.validate_url({'url': url})

    def test_redirect_handler_never_follows_location(self):
        self.assertIsNone(FETCH.NoRedirect().redirect_request(None, None, 302, '', {}, URL))

    def test_failure_classes_never_expose_url_or_arbitrary_exception_text(self):
        for error, expected in ((URLError(URL), 'transport_error'), (OSError(URL), 'filesystem_error'),
                                (ValueError(URL), 'validation_failed'), (RuntimeError(URL), 'internal_error'),
                                (FETCH.FetchError(URL), 'validation_failed'),
                                (FETCH.FetchError({'url': URL}), 'validation_failed')):
            with self.subTest(error=type(error).__name__):
                self.assertEqual(FETCH.failure_class(error), expected)
                self.assertNotIn('PRIVATE_SIGNED_URL', FETCH.failure_class(error))
        error = HTTPError(URL, 403, URL, {}, io.BytesIO(b'private'))
        self.assertEqual(FETCH.failure_class(error), 'http_error')
        self.assertTrue(error.fp.closed)

    def test_root_ownership_regular_type_and_private_modes_are_required(self):
        path = self.directory / 'protected'
        path.write_bytes(b'{}')
        metadata = path.lstat()
        with patch.object(Path, 'lstat', return_value=SimpleNamespace(st_uid=0, st_mode=metadata.st_mode, st_size=2)):
            FETCH.protected(path, limit=2)
        for uid, mode, size in ((501, stat.S_IFREG | 0o600, 2), (0, stat.S_IFREG | 0o644, 2),
                                 (0, stat.S_IFLNK | 0o600, 2), (0, stat.S_IFREG | 0o600, 3)):
            with self.subTest(uid=uid, mode=mode, size=size), \
                    patch.object(Path, 'lstat', return_value=SimpleNamespace(st_uid=uid, st_mode=mode, st_size=size)), \
                    self.assertRaises(FETCH.FetchError):
                FETCH.protected(path, limit=2)


class FetchZipTests(FetchTestCase):
    def test_central_directory_is_bounded_before_zip_metadata_is_opened(self):
        path = self.directory / 'valid.zip'
        data = make_zip()
        path.write_bytes(data)
        FETCH.validate_zip_directory(path)
        end = data.rfind(b'PK\x05\x06')
        for field_offset, format_, replacement in ((4, '<H', 1), (6, '<H', 1), (10, '<H', 257),
                                                  (10, '<H', 65535), (12, '<L', FETCH.CHUNK + 1),
                                                  (16, '<L', 0xffffffff), (20, '<H', 1)):
            changed = bytearray(data)
            struct.pack_into(format_, changed, end + field_offset, replacement)
            candidate = self.directory / f'bad-{field_offset}-{replacement}.zip'
            candidate.write_bytes(changed)
            with self.subTest(field_offset=field_offset, replacement=replacement), self.assertRaises(FETCH.FetchError):
                FETCH.validate_zip_directory(candidate)

    def test_extracts_only_four_exact_regular_top_level_files(self):
        files = fixture_files()
        with zipfile.ZipFile(io.BytesIO(make_zip(files, [('build.log', b'private build log')]))) as archive:
            selected = FETCH.selected_members(archive)
            self.assertEqual(set(selected), set(FETCH.SELECTED))
            for name, member in selected.items():
                FETCH.extract_member(archive, member, self.directory / name)
            self.assertEqual({path.name for path in self.directory.iterdir()}, set(FETCH.SELECTED))
            for name, contents in files.items():
                self.assertEqual((self.directory / name).read_bytes(), contents)
                self.assertEqual(stat.S_IMODE((self.directory / name).stat().st_mode), 0o600)
            with self.assertRaises(FileExistsError):
                FETCH.extract_member(archive, selected['image.tar'], self.directory / 'image.tar')

    def test_duplicates_traversal_aliases_symlinks_and_unsupported_compression_are_rejected(self):
        link = zipfile.ZipInfo('link')
        link.external_attr = (stat.S_IFLNK | 0o777) << 16
        unsupported = zipfile.ZipInfo('compressed')
        unsupported.compress_type = zipfile.ZIP_BZIP2
        for name in ('image.tar', '../outside', '/absolute', 'a//b', 'a/./b', 'image.tar/', 'directory//',
                     'back\\slash', 'line\nfeed', link, unsupported):
            with self.subTest(name=str(name)), zipfile.ZipFile(io.BytesIO(make_zip(extras=[(name, b'bad')]))) as archive:
                with self.assertRaises(FETCH.FetchError):
                    FETCH.selected_members(archive)

    def test_zip_entry_count_total_bytes_and_member_sizes_are_bounded(self):
        data = make_zip()
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            with patch.object(FETCH, 'MAX_EXTRACTED_BYTES', 1), self.assertRaises(FETCH.FetchError):
                FETCH.selected_members(archive)
            with patch.object(FETCH, 'MAX_IMAGE_BYTES', 1), self.assertRaises(FETCH.FetchError):
                FETCH.selected_members(archive)
            with patch.object(FETCH, 'MAX_METADATA_BYTES', 1), self.assertRaises(FETCH.FetchError):
                FETCH.selected_members(archive)
        with zipfile.ZipFile(io.BytesIO(make_zip(extras=[(f'file-{index}', b'') for index in range(253)]))) as archive:
            with self.assertRaises(FETCH.FetchError):
                FETCH.selected_members(archive)


class FetchValidationTests(FetchTestCase):
    def write_files(self, files=None):
        for name, data in (files or fixture_files()).items():
            (self.directory / name).write_bytes(data)

    def test_original_build_source_manifest_and_archive_are_validated_together(self):
        self.write_files()
        manifest = FETCH.validate_build_evidence(self.directory, envelope())
        self.assertEqual(manifest['revision'], REVISION)
        self.assertEqual(manifest['archiveSha256'], FETCH.sha256_file(self.directory / 'image.tar'))

    def test_failed_build_or_changed_source_evidence_cannot_be_published(self):
        for filename, update in (
            ('outcome.json', {'passed': False}), ('outcome.json', {'schemaVersion': True}),
            ('outcome.json', {'error': 'build failed'}), ('outcome.json', {'revision': 'b' * 40}),
            ('source.json', {'revision': 'b' * 40}), ('source.json', {'inputsSha256': {}}),
            ('source.json', {'extra': 'unexpected'}),
        ):
            with self.subTest(filename=filename, update=update):
                files = fixture_files()
                value = json.loads(files[filename])
                value.update(update)
                files[filename] = json.dumps(value).encode()
                self.write_files(files)
                with self.assertRaises(FETCH.FetchError):
                    FETCH.validate_build_evidence(self.directory, envelope())

    def test_modified_image_archive_is_rejected_by_the_shared_validator(self):
        self.write_files()
        with (self.directory / 'image.tar').open('ab') as output:
            output.write(b'modified')
        with self.assertRaises(FETCH.ArtifactError):
            FETCH.validate_build_evidence(self.directory, envelope())


class FetchDownloadTests(FetchTestCase):
    def run_download(self, data, metadata=None, *, status=200, headers=None):
        response = Response(data, status=status, headers=headers)
        opener = Mock()
        opener.open.return_value = response
        report = {'downloadedBytes': 0}
        with patch.object(FETCH, 'build_opener', return_value=opener) as factory, \
                patch.object(FETCH.ssl, 'create_default_context', return_value=None) as tls:
            error = None
            try:
                FETCH.download(URL, self.directory / 'artifact.zip', metadata or envelope(data), report)
            except FETCH.FetchError as caught:
                error = str(caught)
        self.assertTrue(response.closed)
        self.assertEqual(factory.call_args.args[0].proxies, {})
        self.assertIsInstance(factory.call_args.args[2], FETCH.NoRedirect)
        self.assertEqual(opener.open.call_args.kwargs, {'timeout': 15})
        tls.assert_called_once_with(cafile='/etc/ssl/certs/ca-certificates.crt')
        self.assertEqual(opener.open.call_args.args[0].headers['Accept-encoding'], 'identity')
        return error, report

    def test_size_hash_and_normal_tls_are_required_with_no_proxy_or_redirect(self):
        error, report = self.run_download(b'abc')
        self.assertIsNone(error)
        self.assertEqual(report['downloadedBytes'], 3)
        self.assertEqual((self.directory / 'artifact.zip').read_bytes(), b'abc')

    def test_oversize_download_is_stopped_without_retaining_extra_bytes(self):
        error, report = self.run_download(b'abcd', envelope(b'abc'), headers={})
        self.assertEqual(error, 'download_size_mismatch')
        self.assertLessEqual(report['downloadedBytes'], 3)
        self.assertLessEqual((self.directory / 'artifact.zip').stat().st_size, 3)

    def test_incomplete_or_wrong_checksum_download_fails(self):
        error, _ = self.run_download(b'ab', envelope(b'abc'), headers={})
        self.assertEqual(error, 'download_size_mismatch')
        (self.directory / 'artifact.zip').unlink()
        error, _ = self.run_download(b'abc', dict(envelope(b'abc'), zipSha256='0' * 64))
        self.assertEqual(error, 'zip_checksum_mismatch')

    def test_encoded_or_nonmatching_http_response_is_rejected(self):
        for status, headers in ((206, {}), (200, {'Content-Encoding': 'gzip'}), (200, {'Content-Length': '999'})):
            with self.subTest(status=status, headers=headers):
                path = self.directory / 'artifact.zip'
                if path.exists():
                    path.unlink()
                error, _ = self.run_download(b'abc', status=status, headers=headers)
                self.assertEqual(error, 'http_response_rejected')


class FetchLifecycleTests(FetchTestCase):
    def setUp(self):
        super().setUp()
        self.root, self.config, self.work = [self.directory / name for name in ('public', 'config', 'work')]
        for path in (self.root, self.config, self.work):
            path.mkdir(mode=0o700)
        for name in ('releases', 'results'):
            (self.root / name).mkdir(mode=0o700)
        self.data = make_zip()
        self.patches = [
            patch.object(FETCH, 'ROOT', self.root), patch.object(FETCH, 'CONFIG', self.config), patch.object(FETCH, 'WORK', self.work),
            patch.object(FETCH.os, 'geteuid', return_value=0), patch.object(FETCH.signal, 'signal'), patch.object(FETCH.signal, 'alarm'),
            patch.object(FETCH, 'protected', side_effect=self.protected_fixture),
            patch.object(FETCH.shutil, 'disk_usage', return_value=SimpleNamespace(free=10 * 1024**3)),
            patch.object(FETCH.ssl, 'create_default_context', return_value=None),
        ]
        for patcher in self.patches:
            patcher.start()
            self.addCleanup(patcher.stop)
        self.opener = Mock()
        self.opener.open.side_effect = lambda *_args, **_kwargs: Response(self.data)
        opener_patch = patch.object(FETCH, 'build_opener', return_value=self.opener)
        opener_patch.start()
        self.addCleanup(opener_patch.stop)

    @staticmethod
    def protected_fixture(path, *, directory=False, mode=0o600, limit=None):
        # Model root ownership only; real mode/type/size safety stays exercised.
        metadata = path.lstat()
        FETCH.require(stat.S_IMODE(metadata.st_mode) == mode, 'unsafe_filesystem')
        FETCH.require(stat.S_ISDIR(metadata.st_mode) if directory else stat.S_ISREG(metadata.st_mode), 'unsafe_filesystem')
        if limit is not None:
            FETCH.require(0 < metadata.st_size <= limit, 'unsafe_filesystem')

    def execute(self, metadata=None, *, url=URL):
        output = io.StringIO()
        lines = [json.dumps(metadata or envelope(self.data)).encode() + b'\n', json.dumps({'url': url}).encode() + b'\n']
        parent = self

        class Input:
            def readline(self, limit):
                parent.assertEqual(limit, FETCH.LINE_BYTES + 1)
                if len(lines) == 1:
                    parent.assertEqual(json.loads(output.getvalue()), {'schemaVersion': 1, 'status': 'ready'})
                return lines.pop(0)

        code = FETCH.main(Input(), output)
        receipts = [json.loads(line) for line in output.getvalue().splitlines()]
        self.assertNotIn('PRIVATE_SIGNED_URL', output.getvalue())
        return code, receipts

    def test_handshake_precedes_url_read_and_success_is_durable_before_unlock(self):
        original_write = FETCH.write_json

        def checked_write(path, value):
            if path.name == 'fetch-outcome.json':
                with (self.work / 'workload.lock').open('rb') as concurrent:
                    with self.assertRaises(BlockingIOError):
                        fcntl.flock(concurrent, fcntl.LOCK_EX | fcntl.LOCK_NB)
            return original_write(path, value)

        with patch.object(FETCH, 'write_json', side_effect=checked_write):
            code, receipts = self.execute()
        self.assertEqual(code, 0)
        self.assertEqual(len(receipts), 2)
        report = receipts[-1]
        self.assertEqual(report['status'], 'complete')
        self.assertIs(report['passed'], True)
        self.assertIs(report['settled'], True)
        self.assertIs(report['finalized'], True)
        attempt = Path(report['evidence'])
        self.assertEqual(json.loads((attempt / 'fetch-outcome.json').read_text()), report)
        self.assertTrue(json.loads((attempt / 'outcome.json').read_text())['passed'])
        destination = self.root / 'releases' / REVISION
        for name in ('image.tar', 'release.json'):
            self.assertEqual((destination / name).stat().st_ino, (attempt / name).stat().st_ino)
            self.assertEqual(stat.S_IMODE((destination / name).stat().st_mode), 0o600)
        with (self.work / 'workload.lock').open('rb') as concurrent:
            fcntl.flock(concurrent, fcntl.LOCK_EX | fcntl.LOCK_NB)
        self.assertNotIn('PRIVATE_SIGNED_URL', '\n'.join(path.read_text(errors='replace') for path in attempt.glob('*.json')))
        FETCH.signal.alarm.assert_any_call(300)
        self.assertEqual(FETCH.signal.alarm.call_args.args, (0,))

    def test_refetch_retains_identical_destinations_and_all_original_attempts(self):
        first_code, first = self.execute()
        destination = self.root / 'releases' / REVISION
        identity = {name: (destination / name).stat().st_ino for name in ('image.tar', 'release.json')}
        second_code, second = self.execute()
        self.assertEqual((first_code, second_code), (0, 0))
        self.assertNotEqual(first[-1]['evidence'], second[-1]['evidence'])
        self.assertEqual(identity, {name: (destination / name).stat().st_ino for name in identity})
        self.assertEqual(self.opener.open.call_count, 2)

    def test_changed_retained_file_is_not_overwritten_or_reported_successful(self):
        destination = self.root / 'releases' / REVISION
        destination.mkdir(mode=0o700)
        retained = destination / 'release.json'
        retained.write_bytes(b'original different bytes')
        code, receipts = self.execute()
        self.assertEqual(code, 1)
        self.assertEqual(receipts[-1]['failureClass'], 'retained_artifact_differs')
        self.assertIs(receipts[-1]['passed'], False)
        self.assertIs(receipts[-1]['settled'], True)
        self.assertEqual(retained.read_bytes(), b'original different bytes')
        self.assertFalse((destination / 'image.tar').exists())

    def test_unfinished_operation_or_insufficient_disk_never_requests_a_url(self):
        for location in (self.work / 'current.json', self.root / 'release-state.json'):
            location.write_text('{"schemaVersion":1,"finalized":false}')
            code, receipts = self.execute()
            self.assertEqual(code, 1)
            self.assertEqual(len(receipts), 1)
            self.assertEqual(receipts[-1]['failureClass'], 'unfinished_operation')
            self.opener.open.assert_not_called()
            location.unlink()
        with patch.object(FETCH.shutil, 'disk_usage', return_value=SimpleNamespace(free=1)):
            code, receipts = self.execute()
        self.assertEqual(code, 1)
        self.assertEqual(len(receipts), 1)
        self.assertEqual(receipts[-1]['failureClass'], 'insufficient_disk')
        self.opener.open.assert_not_called()

    def test_failed_build_download_hash_or_http_failure_preserves_failed_receipt(self):
        files = fixture_files()
        files['outcome.json'] = json.dumps({'schemaVersion': 1, 'revision': REVISION, 'passed': False, 'error': 'failed'}).encode()
        self.data = make_zip(files)
        code, receipts = self.execute()
        self.assertEqual(code, 1)
        self.assertEqual(receipts[-1]['failureClass'], 'build_outcome_rejected')
        self.assertIs(receipts[-1]['passed'], False)
        self.assertFalse((self.root / 'releases' / REVISION / 'image.tar').exists())
        self.opener.open.side_effect = URLError(URL)
        code, receipts = self.execute()
        self.assertEqual(code, 1)
        self.assertEqual(receipts[-1]['failureClass'], 'transport_error')
        self.assertIs(receipts[-1]['settled'], True)

    def test_receipt_failure_can_never_return_a_successful_fetch(self):
        original = FETCH.write_json

        def write(path, value):
            if path.name == 'fetch-outcome.json':
                raise OSError('private storage error')
            return original(path, value)

        with patch.object(FETCH, 'write_json', side_effect=write):
            code, receipts = self.execute()
        self.assertEqual(code, 1)
        self.assertIs(receipts[-1]['passed'], False)
        self.assertIs(receipts[-1]['settled'], False)
        self.assertEqual(receipts[-1]['failureClass'], 'receipt_write_failed')
        self.assertNotIn('private storage error', str(receipts))

    def test_url_wait_is_covered_by_the_global_deadline_and_retains_failure(self):
        output = io.StringIO()
        first = json.dumps(envelope(self.data)).encode() + b'\n'

        class Input:
            def readline(self, _limit):
                if not output.getvalue():
                    return first
                handler = next(call.args[1] for call in FETCH.signal.signal.call_args_list
                               if call.args[0] == signal.SIGALRM)
                handler(signal.SIGALRM, None)

        code = FETCH.main(Input(), output)
        report = json.loads(output.getvalue().splitlines()[-1])
        self.assertEqual(code, 1)
        self.assertEqual(report['phase'], 'await_url')
        self.assertEqual(report['failureClass'], 'deadline_exceeded')
        self.assertIs(report['passed'], False)
        self.assertIs(report['settled'], True)
        self.assertEqual(json.loads((Path(report['evidence']) / 'fetch-outcome.json').read_text()), report)
        self.opener.open.assert_not_called()

    def test_busy_workload_lock_refuses_before_ready_or_network(self):
        (self.work / 'workload.lock').touch(mode=0o600)
        with (self.work / 'workload.lock').open('rb') as existing:
            fcntl.flock(existing, fcntl.LOCK_EX | fcntl.LOCK_NB)
            code, receipts = self.execute()
        self.assertEqual(code, 1)
        self.assertEqual(len(receipts), 1)
        self.assertIs(receipts[-1]['passed'], False)
        self.opener.open.assert_not_called()

    def test_malicious_zip_metadata_is_refused_before_constructing_zipfile(self):
        data = bytearray(self.data)
        end = data.rfind(b'PK\x05\x06')
        struct.pack_into('<L', data, end + 12, FETCH.CHUNK + 1)
        self.data = bytes(data)
        with patch.object(FETCH.zipfile, 'ZipFile', side_effect=AssertionError('Unbounded ZIP metadata read')) as constructor:
            code, receipts = self.execute()
        self.assertEqual(code, 1)
        self.assertEqual(receipts[-1]['failureClass'], 'zip_shape_rejected')
        constructor.assert_not_called()


if __name__ == '__main__':
    unittest.main()
