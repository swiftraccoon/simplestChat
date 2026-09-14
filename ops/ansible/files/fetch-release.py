#!/usr/bin/env python3
"""Receive one verified GitHub artifact URL over stdin; never log its signature.

The controller sends a public envelope, waits for the ready receipt, then obtains
and sends the expiring URL. Only four selected artifact files are extracted into
private evidence. Docker, services, credentials and deployment state are untouched.
"""

import fcntl
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import signal
import ssl
import stat
import struct
import sys
import tempfile
import time
from urllib.error import HTTPError, URLError
from urllib.parse import urlsplit
from urllib.request import HTTPSHandler, HTTPRedirectHandler, ProxyHandler, Request, build_opener
import zipfile

from release_artifact import ArtifactError, sha256_file, validate_manifest, verify_archive

ROOT = Path('/srv/simplestchat-public')
CONFIG = Path('/etc/simplestchat-public')
WORK = Path('/run/simplestchat-bench')
LINE_BYTES = 16384
CHUNK = 1024 * 1024
MAX_ZIP_BYTES = 2 * 1024**3
MAX_IMAGE_BYTES = 2 * 1024**3
MAX_METADATA_BYTES = 8 * CHUNK
MAX_EXTRACTED_BYTES = MAX_IMAGE_BYTES + 16 * CHUNK
SELECTED = ('image.tar', 'release.json', 'outcome.json', 'source.json')
SOURCE_KEYS = {'Dockerfile', '.dockerignore', 'Cargo.lock', 'web/package-lock.json', 'build/pip-constraints.txt'}
ENVELOPE_KEYS = {'schemaVersion', 'repository', 'artifactId', 'buildRunId', 'ciRunId', 'revision',
                 'artifactZipBytes', 'zipSha256'}
FAILURE_CLASSES = {
    'precondition_failed', 'ambiguous_json', 'invalid_json', 'invalid_request', 'invalid_envelope',
    'invalid_url', 'unsafe_filesystem', 'unfinished_operation', 'http_response_rejected',
    'download_size_mismatch', 'zip_checksum_mismatch', 'zip_shape_rejected', 'zip_member_size_mismatch',
    'build_outcome_rejected', 'source_evidence_rejected', 'release_revision_mismatch',
    'retained_artifact_differs', 'root_required', 'insufficient_disk', 'deadline_exceeded', 'interrupted',
}


class FetchError(Exception):
    """Only fixed coarse classes, never exception messages, enter public output."""


def require(condition, failure='precondition_failed'):
    if not condition:
        raise FetchError(failure)


def unique_object(pairs):
    value = {}
    for key, item in pairs:
        require(key not in value, 'ambiguous_json')
        value[key] = item
    return value


def decode_json(data):
    return json.loads(data, object_pairs_hook=unique_object,
                      parse_constant=lambda _value: (_ for _ in ()).throw(FetchError('invalid_json')))


def read_line(source):
    data = source.readline(LINE_BYTES + 1)
    require(0 < len(data) <= LINE_BYTES and data.endswith(b'\n'), 'invalid_request')
    value = decode_json(data)
    require(isinstance(value, dict), 'invalid_request')
    return value


def validate_envelope(value):
    require(isinstance(value, dict) and set(value) == ENVELOPE_KEYS, 'invalid_envelope')
    require(type(value['schemaVersion']) is int and value['schemaVersion'] == 1, 'invalid_envelope')
    repository = value['repository']
    require(isinstance(repository, str)
            and re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9-]{0,38}/[A-Za-z0-9][A-Za-z0-9_.-]{0,99}', repository),
            'invalid_envelope')
    for key in ('artifactId', 'buildRunId', 'ciRunId'):
        require(type(value[key]) is int and 0 < value[key] <= 2**63 - 1, 'invalid_envelope')
    require(isinstance(value['revision'], str) and re.fullmatch(r'[a-f0-9]{40}', value['revision']), 'invalid_envelope')
    require(isinstance(value['zipSha256'], str) and re.fullmatch(r'[a-f0-9]{64}', value['zipSha256']), 'invalid_envelope')
    require(type(value['artifactZipBytes']) is int and 0 < value['artifactZipBytes'] <= MAX_ZIP_BYTES, 'invalid_envelope')
    return value


def validate_url(value):
    require(isinstance(value, dict) and set(value) == {'url'}, 'invalid_url')
    url = value['url']
    require(isinstance(url, str) and 0 < len(url) <= LINE_BYTES
            and not any(ord(character) <= 32 or ord(character) >= 127 for character in url), 'invalid_url')
    parsed = urlsplit(url)
    require(parsed.scheme == 'https'
            and re.fullmatch(r'[a-z0-9][a-z0-9-]{0,62}\.blob\.core\.windows\.net', parsed.netloc)
            and parsed.path.startswith('/') and parsed.query and not parsed.fragment, 'invalid_url')
    return url


def protected(path, *, directory=False, mode=0o600, limit=None):
    metadata = path.lstat()
    require(metadata.st_uid == 0 and stat.S_IMODE(metadata.st_mode) == mode, 'unsafe_filesystem')
    require(stat.S_ISDIR(metadata.st_mode) if directory else stat.S_ISREG(metadata.st_mode), 'unsafe_filesystem')
    if limit is not None:
        require(0 < metadata.st_size <= limit, 'unsafe_filesystem')


def sync_directory(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def write_json(path, value):
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, 'w', encoding='utf-8') as output:
        json.dump(value, output, indent=2)
        output.write('\n')
        output.flush()
        os.fsync(output.fileno())


def finished_journal(path):
    if path.exists() or path.is_symlink():
        protected(path, limit=LINE_BYTES)
        value = decode_json(path.read_bytes())
        require(isinstance(value, dict) and type(value.get('schemaVersion')) is int
                and value['schemaVersion'] == 1 and value.get('finalized') is True, 'unfinished_operation')


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def download(url, target, envelope, report):
    context = ssl.create_default_context(cafile='/etc/ssl/certs/ca-certificates.crt')
    opener = build_opener(ProxyHandler({}), HTTPSHandler(context=context), NoRedirect())
    request = Request(url, headers={'Accept': 'application/zip', 'Accept-Encoding': 'identity'})
    expected = envelope['artifactZipBytes']
    digest = hashlib.sha256()
    with opener.open(request, timeout=15) as response, target.open('xb') as output:
        require(response.status == 200, 'http_response_rejected')
        require(response.headers.get('Content-Encoding', 'identity') == 'identity', 'http_response_rejected')
        length = response.headers.get('Content-Length')
        require(length is None or length == str(expected), 'http_response_rejected')
        while True:
            chunk = response.read(min(CHUNK, expected - report['downloadedBytes'] + 1))
            if not chunk:
                break
            require(report['downloadedBytes'] + len(chunk) <= expected, 'download_size_mismatch')
            output.write(chunk)
            digest.update(chunk)
            report['downloadedBytes'] += len(chunk)
        output.flush()
        os.fsync(output.fileno())
    require(report['downloadedBytes'] == expected, 'download_size_mismatch')
    require(digest.hexdigest() == envelope['zipSha256'], 'zip_checksum_mismatch')


def validate_zip_directory(path):
    """Bound central-directory allocation before zipfile reads its metadata."""
    size = path.stat().st_size
    require(22 <= size <= MAX_ZIP_BYTES, 'zip_shape_rejected')
    with path.open('rb') as source:
        source.seek(max(0, size - 65557))
        tail = source.read(65557)
    offset = tail.rfind(b'PK\x05\x06')
    require(offset >= 0 and len(tail) - offset >= 22, 'zip_shape_rejected')
    _, disk, directory_disk, disk_entries, entries, directory_bytes, directory_offset, comment_bytes = \
        struct.unpack_from('<4s4H2LH', tail, offset)
    end_offset = size - len(tail) + offset
    require(offset + 22 + comment_bytes == len(tail)
            and disk == directory_disk == 0 and disk_entries == entries and 0 < entries <= 256
            and 0 < directory_bytes <= CHUNK and directory_offset != 0xffffffff
            and directory_offset + directory_bytes == end_offset, 'zip_shape_rejected')


def selected_members(archive):
    members = archive.infolist()
    require(0 < len(members) <= 256, 'zip_shape_rejected')
    require(sum(member.file_size for member in members) <= MAX_EXTRACTED_BYTES, 'zip_shape_rejected')
    indexed = {}
    for member in members:
        name = member.filename
        path = PurePosixPath(name)
        canonical = name.rstrip('/')
        require(name and name == member.orig_filename and '\\' not in name
                and not any(ord(char) < 32 or ord(char) >= 127 for char in name)
                and not path.is_absolute() and '..' not in path.parts and str(path) == canonical
                and name == canonical + ('/' if member.is_dir() else '')
                and canonical not in ('', '.') and canonical not in indexed, 'zip_shape_rejected')
        kind = stat.S_IFMT(member.external_attr >> 16)
        require(kind in ((0, stat.S_IFDIR) if member.is_dir() else (0, stat.S_IFREG))
                and (not member.external_attr & 0x10 or member.is_dir())
                and not member.flag_bits & 1 and member.compress_type in (zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED),
                'zip_shape_rejected')
        require(0 <= member.file_size <= (MAX_IMAGE_BYTES if name == 'image.tar' else MAX_METADATA_BYTES),
                'zip_shape_rejected')
        indexed[canonical] = member
    for name in SELECTED:
        require(name in indexed and indexed[name].filename == name and not indexed[name].is_dir()
                and indexed[name].file_size > 0, 'zip_shape_rejected')
    return {name: indexed[name] for name in SELECTED}


def extract_member(archive, member, destination):
    total = 0
    with archive.open(member) as source, destination.open('xb') as output:
        while chunk := source.read(min(CHUNK, member.file_size - total + 1)):
            require(total + len(chunk) <= member.file_size, 'zip_member_size_mismatch')
            output.write(chunk)
            total += len(chunk)
        require(total == member.file_size, 'zip_member_size_mismatch')
        output.flush()
        os.fsync(output.fileno())


def validate_build_evidence(attempt, envelope):
    outcome = decode_json((attempt / 'outcome.json').read_bytes())
    require(isinstance(outcome, dict) and type(outcome.get('schemaVersion')) is int
            and outcome['schemaVersion'] == 1 and outcome.get('passed') is True
            and 'error' in outcome and outcome['error'] is None and outcome.get('revision') == envelope['revision'],
            'build_outcome_rejected')
    source = decode_json((attempt / 'source.json').read_bytes())
    require(isinstance(source, dict) and set(source) == {'revision', 'inputsSha256'}
            and source['revision'] == envelope['revision'] and isinstance(source['inputsSha256'], dict)
            and set(source['inputsSha256']) == SOURCE_KEYS
            and all(isinstance(value, str) and re.fullmatch(r'[a-f0-9]{64}', value)
                    for value in source['inputsSha256'].values()), 'source_evidence_rejected')
    manifest = validate_manifest(attempt / 'release.json')
    require(manifest['revision'] == envelope['revision'], 'release_revision_mismatch')
    verify_archive(attempt / 'image.tar', manifest)
    return manifest


def publish(attempt, destination):
    # Check both existing selections before creating either new hardlink.
    for name in ('release.json', 'image.tar'):
        target, source = destination / name, attempt / name
        if target.exists() or target.is_symlink():
            protected(target)
            require(target.stat().st_size == source.stat().st_size
                    and sha256_file(target) == sha256_file(source), 'retained_artifact_differs')
    for name in ('release.json', 'image.tar'):
        target = destination / name
        try:
            os.link(attempt / name, target, follow_symlinks=False)
        except FileExistsError:
            protected(target)
            require(target.stat().st_size == (attempt / name).stat().st_size
                    and sha256_file(target) == sha256_file(attempt / name), 'retained_artifact_differs')
    sync_directory(destination)


def failure_class(error):
    if isinstance(error, FetchError):
        return error.args[0] if len(error.args) == 1 and isinstance(error.args[0], str) \
            and error.args[0] in FAILURE_CLASSES else 'validation_failed'
    if isinstance(error, HTTPError):
        error.close()
        return 'http_error'
    if isinstance(error, (URLError, TimeoutError)):
        return 'transport_error'
    if isinstance(error, zipfile.BadZipFile):
        return 'zip_error'
    if isinstance(error, OSError):
        return 'filesystem_error'
    if isinstance(error, (ArtifactError, ValueError, KeyError, TypeError)):
        return 'validation_failed'
    return 'internal_error'


def main(source=None, output=None):
    source = source or sys.stdin.buffer
    output = output or sys.stdout
    os.umask(0o077)
    began = time.monotonic()
    attempt = None
    lock = None
    report = {'schemaVersion': 1, 'status': 'failed', 'passed': False, 'settled': False,
              'phase': 'envelope', 'failureClass': None, 'evidence': None, 'revision': None,
              'artifactId': None, 'zipSha256': None, 'artifactZipBytes': None, 'downloadedBytes': 0,
              'archiveSha256': None, 'manifestSha256': None, 'elapsedSeconds': 0, 'finalized': False}

    def interrupted(number, _frame):
        raise FetchError('deadline_exceeded' if number == signal.SIGALRM else 'interrupted')

    for number in (signal.SIGTERM, signal.SIGINT, signal.SIGALRM):
        signal.signal(number, interrupted)
    signal.alarm(300)
    try:
        require(os.geteuid() == 0, 'root_required')
        envelope = validate_envelope(read_line(source))
        report.update({key: envelope[key] for key in ('revision', 'artifactId', 'zipSha256', 'artifactZipBytes')})
        report['phase'] = 'preflight'
        for path in (ROOT, CONFIG, ROOT / 'releases', ROOT / 'results'):
            protected(path, directory=True, mode=0o700)
        WORK.mkdir(mode=0o700, exist_ok=True)
        protected(WORK, directory=True, mode=0o700)
        try:
            descriptor = os.open(WORK / 'workload.lock', os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            os.close(descriptor)
        except FileExistsError:
            pass
        protected(WORK / 'workload.lock')
        lock = (WORK / 'workload.lock').open('rb')
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        finished_journal(WORK / 'current.json')
        finished_journal(ROOT / 'release-state.json')
        destination = ROOT / 'releases' / envelope['revision']
        destination.mkdir(mode=0o700, exist_ok=True)
        protected(destination, directory=True, mode=0o700)
        sync_directory(ROOT / 'releases')
        attempt = Path(tempfile.mkdtemp(prefix='download.', dir=destination))
        report['evidence'] = str(attempt)
        write_json(attempt / 'envelope.json', envelope)
        sync_directory(attempt)
        sync_directory(destination)
        require(shutil.disk_usage(destination).free >= envelope['artifactZipBytes'] + MAX_EXTRACTED_BYTES + 1024**3,
                'insufficient_disk')
        report['phase'] = 'await_url'
        print(json.dumps({'schemaVersion': 1, 'status': 'ready'}), file=output, flush=True)
        url = validate_url(read_line(source))
        report['phase'] = 'download'
        download(url, attempt / 'artifact.zip', envelope, report)
        del url
        report['phase'] = 'extract'
        validate_zip_directory(attempt / 'artifact.zip')
        with zipfile.ZipFile(attempt / 'artifact.zip') as archive:
            for name, member in selected_members(archive).items():
                extract_member(archive, member, attempt / name)
        report['phase'] = 'verify'
        manifest = validate_build_evidence(attempt, envelope)
        report['archiveSha256'] = manifest['archiveSha256']
        report['manifestSha256'] = sha256_file(attempt / 'release.json')
        sync_directory(attempt)
        report['phase'] = 'publish'
        publish(attempt, destination)
        report.update({'status': 'complete', 'passed': True, 'phase': 'complete', 'finalized': True})
    except BaseException as error:
        report.update({'status': 'failed', 'passed': False})
        report['failureClass'] = failure_class(error)
    finally:
        report['elapsedSeconds'] = round(time.monotonic() - began, 3)
        report['settled'] = attempt is not None
        try:
            if attempt is not None:
                write_json(attempt / 'fetch-outcome.json', report)
                sync_directory(attempt)
        except BaseException:
            report.update({'status': 'failed', 'passed': False, 'settled': False, 'failureClass': 'receipt_write_failed'})
        finally:
            if lock is not None:
                lock.close()
            signal.alarm(0)
        print(json.dumps(report), file=output, flush=True)
    return 0 if report['passed'] and report['settled'] and report['finalized'] else 1


if __name__ == '__main__':
    sys.exit(main())
