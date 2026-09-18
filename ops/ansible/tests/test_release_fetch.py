"""Offline fetch protocol, artifact validation and private publication tests."""

import fcntl
import hashlib
import io
import json
import os
import shutil
import signal
import ssl
import stat
import struct
import tarfile
import tempfile
import unittest
import warnings
import zipfile
from collections.abc import Callable, Iterable
from dataclasses import dataclass, field
from email.message import Message
from pathlib import Path
from types import FrameType
from typing import Protocol, final, override
from unittest.mock import patch
from urllib.error import HTTPError, URLError
from urllib.request import BaseHandler, ProxyHandler, Request

# Fixture assertions and literal expected limits document the tested contract.
import test_support

# isort: split
import release_fetch_receiver as fetch
from release_artifact import ArtifactError, sha256_file
from release_json import JsonObject, JsonValue, decode_json, integer_value
from test_support import obj, string

ROOT = test_support.ROOT
FILES = ROOT / "ops/ansible/files"


def json_object(data: str | bytes) -> JsonObject:
    """Decode fixture evidence with the same checked JSON boundary as production."""
    return obj(decode_json(data))


REVISION = "a" * 40
URL = "https://productionresultssa6.blob.core.windows.net/path/artifact.zip?sig=PRIVATE_SIGNED_URL"


def envelope(data: bytes = b"fixture") -> JsonObject:
    """Create canonical public provenance matching the supplied archive bytes."""
    return {
        "schemaVersion": 1,
        "repository": "owner/repo",
        "artifactId": 123,
        "buildRunId": 456,
        "ciRunId": 789,
        "revision": REVISION,
        "artifactZipBytes": len(data),
        "zipSha256": hashlib.sha256(data).hexdigest(),
    }


def fixture_files() -> dict[str, bytes]:
    """Build the four artifact files with a harmless non-executable image layer."""
    image = io.BytesIO()
    config = {
        "architecture": "amd64",
        "os": "linux",
        "config": {
            "User": "10001:10001",
            "Cmd": ["/app/simplestChat"],
            "Labels": {"org.opencontainers.image.revision": REVISION},
        },
    }
    files = {
        "manifest.json": json.dumps(
            [
                {
                    "Config": "config.json",
                    "RepoTags": [f"simplestchat-release/production:{REVISION}"],
                    "Layers": ["layer.tar"],
                }
            ]
        ).encode(),
        "config.json": json.dumps(config).encode(),
        "layer.tar": b"disposable fixture layer, never executed",
    }
    with tarfile.open(fileobj=image, mode="w") as archive:
        for name, data in files.items():
            member = tarfile.TarInfo(name)
            member.size = len(data)
            archive.addfile(member, io.BytesIO(data))
    manifest = {
        "schemaVersion": 1,
        "revision": REVISION,
        "platform": "linux/amd64",
        "archiveSha256": hashlib.sha256(image.getvalue()).hexdigest(),
        "imageTag": f"simplestchat-release/production:{REVISION}",
        "migrations": {"1": hashlib.sha384(b"SELECT 1;").hexdigest()},
        "createdAt": "2026-09-13T00:00:00Z",
    }
    outcome = {"schemaVersion": 1, "revision": REVISION, "passed": True, "error": None}
    source = {
        "revision": REVISION,
        "inputsSha256": {
            name: hashlib.sha256(name.encode()).hexdigest() for name in fetch.SOURCE_KEYS
        },
    }
    return {
        "image.tar": image.getvalue(),
        "release.json": json.dumps(manifest).encode(),
        "outcome.json": json.dumps(outcome).encode(),
        "source.json": json.dumps(source).encode(),
    }


def make_zip(
    files: dict[str, bytes] | None = None,
    extras: Iterable[tuple[str | zipfile.ZipInfo, bytes]] = (),
) -> bytes:
    """Package selected files and optional malformed members entirely in memory."""
    data = io.BytesIO()
    with (
        warnings.catch_warnings(),
        zipfile.ZipFile(data, "w", compression=zipfile.ZIP_DEFLATED) as archive,
    ):
        warnings.simplefilter("ignore", UserWarning)
        for name, value in (files or fixture_files()).items():
            archive.writestr(name, value)
        for name, value in extras:
            archive.writestr(name, value)
    return data.getvalue()


class Response(io.BytesIO):
    """Expose a deterministic HTTP-like body without any network connection."""

    def __init__(
        self, data: bytes, *, status: int = 200, headers: dict[str, str] | None = None
    ) -> None:
        """Attach the requested status and headers to an in-memory response body."""
        super().__init__(data)
        self.status: int = status
        self.headers: dict[str, str] = (
            headers if headers is not None else {"Content-Length": str(len(data))}
        )


@dataclass(slots=True, kw_only=True)
class Opener:
    """Record typed HTTP requests and return only caller-provided fixture responses."""

    response: Callable[[], Response]
    error: BaseException | None = None
    calls: list[tuple[Request, float]] = field(default_factory=list)

    def open(self, request: Request, *, timeout: float) -> Response:
        """Record the timeout and either raise the selected failure or return bytes."""
        self.calls.append((request, timeout))
        if self.error is not None:
            raise self.error
        return self.response()


@dataclass(frozen=True, slots=True, kw_only=True)
class DiskUsage:
    """Supply only the free-space observation consumed by the receiver."""

    free: int


class PatchHandle(Protocol):
    """Represent reversible patches without exposing dynamically typed replacements."""

    def start(self) -> object:
        """Activate one patch."""
        ...

    def stop(self) -> object:
        """Restore one patched attribute."""
        ...


def metadata(*, uid: int, mode: int, size: int) -> os.stat_result:
    """Construct genuine stat-result metadata for permission boundary tests."""
    return os.stat_result((mode, 0, 0, 1, uid, 0, size, 0, 0, 0))


class FetchTestCase(unittest.TestCase):
    """Own a private temporary directory only while a test is executing."""

    def __init__(self, methodName: str = "runTest") -> None:  # noqa: N803 - unittest's public constructor keyword.
        """Keep discovery free of temporary filesystem mutations."""
        super().__init__(methodName)
        self.directory: Path = Path()

    @override
    def setUp(self) -> None:
        """Create isolated fixtures and register reversible test substitutions."""
        temporary = tempfile.TemporaryDirectory(prefix="simplestchat-release-fetch.")
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name).resolve()
        original_umask = os.umask(0o077)
        self.addCleanup(os.umask, original_umask)


@final
class FetchProtocolTests(FetchTestCase):
    """Validate canonical requests, redacted failures, and protected paths."""

    def test_public_envelope_is_exact_and_identifiers_are_canonical(self) -> None:
        """Public envelope is exact and identifiers are canonical."""
        self.assertEqual(fetch.validate_envelope(envelope()), envelope())
        for key, value in (
            ("schemaVersion", True),
            ("repository", "../repo"),
            ("repository", "owner/repo/other"),
            ("artifactId", 0),
            ("buildRunId", True),
            ("ciRunId", -1),
            ("revision", "main"),
            ("artifactZipBytes", fetch.MAX_ZIP_BYTES + 1),
            ("zipSha256", "A" * 64),
        ):
            with self.subTest(key=key, value=value), self.assertRaises(fetch.FetchError):
                _ = fetch.validate_envelope(dict(envelope(), **{key: value}))
        with self.assertRaises(fetch.FetchError):
            _ = fetch.validate_envelope(dict(envelope(), url=URL))

    def test_line_protocol_is_bounded_terminated_unique_and_finite_json(self) -> None:
        """Line protocol is bounded terminated unique and finite json."""
        self.assertEqual(fetch.read_line(io.BytesIO(b'{"field":1}\n')), {"field": 1})
        for data in (
            b"",
            b"{}",
            b"[]\n",
            b'{"url":"one","url":"two"}\n',
            b'{"field":NaN}\n',
            b" " * fetch.LINE_BYTES + b"{}\n",
        ):
            with self.subTest(data=data[:40]), self.assertRaises((fetch.FetchError, ValueError)):
                _ = fetch.read_line(io.BytesIO(data))

    def test_url_allows_one_blob_account_label_and_rejects_other_authorities(self) -> None:
        """Url allows one blob account label and rejects other authorities."""
        self.assertEqual(fetch.validate_url({"url": URL}), URL)
        other_account = URL.replace("productionresultssa6", "github-artifacts")
        self.assertEqual(fetch.validate_url({"url": other_account}), other_account)
        for url in (
            URL.replace("https:", "http:"),
            URL.replace(".blob.core.windows.net", ".blob.core.windows.net.example.com"),
            URL.replace("productionresultssa6", "other.productionresultssa6"),
            URL.replace("https://", "https://user@"),
            URL.replace(".net/", ".net:443/"),
            URL + "#fragment",
            URL + "\n",
            URL.replace("?sig=PRIVATE_SIGNED_URL", ""),
            "https://127.0.0.1/path?sig=x",
        ):
            with self.subTest(url=url), self.assertRaises(fetch.FetchError):
                _ = fetch.validate_url({"url": url})

    def test_json_failure_classes_preserve_syntax_ambiguity_and_nonfinite_boundaries(self) -> None:
        """Keep malformed syntax generic while distinguishing duplicate and non-finite JSON."""
        for data, expected in (
            (b'{"field":}\n', "validation_failed"),
            (b'{"field":"\xff"}\n', "validation_failed"),
            (b'{"field":1,"field":2}\n', "ambiguous_json"),
            (b'{"field":NaN}\n', "invalid_json"),
            (b'{"field":Infinity}\n', "invalid_json"),
        ):
            with self.subTest(expected=expected, data=data):
                with self.assertRaises((fetch.FetchError, ValueError)) as caught:
                    _ = fetch.read_line(io.BytesIO(data))
                self.assertEqual(fetch.failure_class(caught.exception), expected)

    def test_redirect_handler_never_follows_location(self) -> None:
        """Redirect handler never follows location."""
        self.assertIsNone(
            fetch.NoRedirect().redirect_request(Request(URL), io.BytesIO(), 302, "", Message(), URL)
        )

    def test_failure_classes_never_expose_url_or_arbitrary_exception_text(self) -> None:
        """Failure classes never expose url or arbitrary exception text."""
        for error, expected in (
            (URLError(URL), "transport_error"),
            (OSError(URL), "filesystem_error"),
            (ValueError(URL), "validation_failed"),
            (RuntimeError(URL), "internal_error"),
            (fetch.FetchError(URL), "validation_failed"),
            (fetch.FetchError({"url": URL}), "validation_failed"),
        ):
            with self.subTest(error=type(error).__name__):
                self.assertEqual(fetch.failure_class(error), expected)
                self.assertNotIn("PRIVATE_SIGNED_URL", fetch.failure_class(error))
        body = io.BytesIO(b"private")
        error = HTTPError(URL, 403, URL, Message(), body)
        self.assertEqual(fetch.failure_class(error), "http_error")
        self.assertTrue(body.closed)

    def test_root_ownership_regular_type_and_private_modes_are_required(self) -> None:
        """Root ownership regular type and private modes are required."""
        path = self.directory / "protected"
        _ = path.write_bytes(b"{}")
        observed = path.lstat()
        with patch.object(
            Path, "lstat", return_value=metadata(uid=0, mode=observed.st_mode, size=2)
        ):
            fetch.protected(path, limit=2)
        for uid, mode, size in (
            (501, stat.S_IFREG | 0o600, 2),
            (0, stat.S_IFREG | 0o644, 2),
            (0, stat.S_IFLNK | 0o600, 2),
            (0, stat.S_IFREG | 0o600, 3),
        ):
            with (
                self.subTest(uid=uid, mode=mode, size=size),
                patch.object(Path, "lstat", return_value=metadata(uid=uid, mode=mode, size=size)),
                self.assertRaises(fetch.FetchError),
            ):
                fetch.protected(path, limit=2)


@final
class FetchZipTests(FetchTestCase):
    """Bound ZIP metadata and extraction before publication can occur."""

    def test_central_directory_is_bounded_before_zip_metadata_is_opened(self) -> None:
        """Central directory is bounded before zip metadata is opened."""
        path = self.directory / "valid.zip"
        data = make_zip()
        _ = path.write_bytes(data)
        fetch.validate_zip_directory(path)
        end = data.rfind(b"PK\x05\x06")
        for field_offset, format_, replacement in (
            (4, "<H", 1),
            (6, "<H", 1),
            (10, "<H", 257),
            (10, "<H", 65535),
            (12, "<L", fetch.CHUNK + 1),
            (16, "<L", 0xFFFFFFFF),
            (20, "<H", 1),
        ):
            changed = bytearray(data)
            struct.pack_into(format_, changed, end + field_offset, replacement)
            candidate = self.directory / f"bad-{field_offset}-{replacement}.zip"
            _ = candidate.write_bytes(changed)
            with (
                self.subTest(field_offset=field_offset, replacement=replacement),
                self.assertRaises(fetch.FetchError),
            ):
                fetch.validate_zip_directory(candidate)

    def test_extracts_only_four_exact_regular_top_level_files(self) -> None:
        """Extracts only four exact regular top level files."""
        files = fixture_files()
        with zipfile.ZipFile(
            io.BytesIO(make_zip(files, [("build.log", b"private build log")]))
        ) as archive:
            selected = fetch.selected_members(archive)
            self.assertEqual(set(selected), set(fetch.SELECTED))
            for name, member in selected.items():
                fetch.extract_member(archive, member, self.directory / name)
            self.assertEqual({path.name for path in self.directory.iterdir()}, set(fetch.SELECTED))
            for name, contents in files.items():
                self.assertEqual((self.directory / name).read_bytes(), contents)
                self.assertEqual(stat.S_IMODE((self.directory / name).stat().st_mode), 0o600)
            with self.assertRaises(FileExistsError):
                fetch.extract_member(archive, selected["image.tar"], self.directory / "image.tar")

    def test_duplicates_traversal_aliases_symlinks_and_unsupported_compression_are_rejected(
        self,
    ) -> None:
        """Duplicates traversal aliases symlinks and unsupported compression are rejected."""
        link = zipfile.ZipInfo("link")
        link.external_attr = (stat.S_IFLNK | 0o777) << 16
        unsupported = zipfile.ZipInfo("compressed")
        unsupported.compress_type = zipfile.ZIP_BZIP2
        for name in (
            "image.tar",
            "../outside",
            "/absolute",
            "a//b",
            "a/./b",
            "image.tar/",
            "directory//",
            "back\\slash",
            "line\nfeed",
            link,
            unsupported,
        ):
            with (
                self.subTest(name=str(name)),
                zipfile.ZipFile(io.BytesIO(make_zip(extras=[(name, b"bad")]))) as archive,
                self.assertRaises(fetch.FetchError),
            ):
                _ = fetch.selected_members(archive)

    def test_zip_entry_count_total_bytes_and_member_sizes_are_bounded(self) -> None:
        """Zip entry count total bytes and member sizes are bounded."""
        data = make_zip()
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            with patch.object(fetch, "MAX_EXTRACTED_BYTES", 1), self.assertRaises(fetch.FetchError):
                _ = fetch.selected_members(archive)
            with patch.object(fetch, "MAX_IMAGE_BYTES", 1), self.assertRaises(fetch.FetchError):
                _ = fetch.selected_members(archive)
            with patch.object(fetch, "MAX_METADATA_BYTES", 1), self.assertRaises(fetch.FetchError):
                _ = fetch.selected_members(archive)
        with (
            zipfile.ZipFile(
                io.BytesIO(make_zip(extras=[(f"file-{index}", b"") for index in range(253)]))
            ) as archive,
            self.assertRaises(fetch.FetchError),
        ):
            _ = fetch.selected_members(archive)


@final
class FetchValidationTests(FetchTestCase):
    """Require complete successful provenance and an unmodified image archive."""

    def write_files(self, files: dict[str, bytes] | None = None) -> None:
        """Retain the supplied artifact bytes beneath this test's private directory."""
        for name, data in (files or fixture_files()).items():
            _ = (self.directory / name).write_bytes(data)

    def test_original_build_source_manifest_and_archive_are_validated_together(self) -> None:
        """Original build source manifest and archive are validated together."""
        self.write_files()
        manifest = fetch.validate_build_evidence(self.directory, envelope())
        self.assertEqual(manifest["revision"], REVISION)
        self.assertEqual(manifest["archiveSha256"], sha256_file(self.directory / "image.tar"))

    def test_failed_build_or_changed_source_evidence_cannot_be_published(self) -> None:
        """Failed build or changed source evidence cannot be published."""
        replacements: tuple[tuple[str, JsonObject], ...] = (
            ("outcome.json", {"passed": False}),
            ("outcome.json", {"schemaVersion": True}),
            ("outcome.json", {"error": "build failed"}),
            ("outcome.json", {"revision": "b" * 40}),
            ("source.json", {"revision": "b" * 40}),
            ("source.json", {"inputsSha256": {}}),
            ("source.json", {"extra": "unexpected"}),
        )
        for filename, update in replacements:
            with self.subTest(filename=filename, update=update):
                files = fixture_files()
                value = json_object(files[filename])
                value.update(update)
                files[filename] = json.dumps(value).encode()
                self.write_files(files)
                with self.assertRaises(fetch.FetchError):
                    _ = fetch.validate_build_evidence(self.directory, envelope())

    def test_modified_image_archive_is_rejected_by_the_shared_validator(self) -> None:
        """Modified image archive is rejected by the shared validator."""
        self.write_files()
        with (self.directory / "image.tar").open("ab") as output:
            _ = output.write(b"modified")
        with self.assertRaises(ArtifactError):
            _ = fetch.validate_build_evidence(self.directory, envelope())


@final
class FetchDownloadTests(FetchTestCase):
    """Exercise bounded response validation without a network transport."""

    def run_download(
        self,
        data: bytes,
        metadata: JsonObject | None = None,
        *,
        status: int = 200,
        headers: dict[str, str] | None = None,
    ) -> tuple[str | None, JsonObject]:
        """Capture typed request policy and return the download's retained outcome."""
        response = Response(data, status=status, headers=headers)
        opener = Opener(response=lambda: response)
        report: JsonObject = {"downloadedBytes": 0}
        handlers: list[BaseHandler] = []
        proxy_settings: list[dict[str, str]] = []

        def proxy_factory(value: dict[str, str]) -> ProxyHandler:
            proxy_settings.append(dict(value))
            return ProxyHandler(value)

        def factory(*values: BaseHandler) -> Opener:
            handlers.extend(values)
            return opener

        with (
            patch.object(fetch, "build_opener", side_effect=factory),
            patch.object(fetch, "ProxyHandler", side_effect=proxy_factory),
            patch.object(ssl, "create_default_context", return_value=None) as tls,
        ):
            error = None
            try:
                fetch.download(
                    URL, self.directory / "artifact.zip", metadata or envelope(data), report
                )
            except fetch.FetchError as caught:
                error = str(caught)
        self.assertTrue(response.closed)
        self.assertIsInstance(handlers[0], ProxyHandler)
        self.assertEqual(proxy_settings, [{}])
        self.assertIsInstance(handlers[2], fetch.NoRedirect)
        self.assertEqual(opener.calls[0][1], 15)
        tls.assert_called_once_with(cafile="/etc/ssl/certs/ca-certificates.crt")
        self.assertEqual(opener.calls[0][0].get_header("Accept-encoding"), "identity")
        return error, report

    def test_size_hash_and_normal_tls_are_required_with_no_proxy_or_redirect(self) -> None:
        """Size hash and normal tls are required with no proxy or redirect."""
        error, report = self.run_download(b"abc")
        self.assertIsNone(error)
        self.assertEqual(report["downloadedBytes"], 3)
        self.assertEqual((self.directory / "artifact.zip").read_bytes(), b"abc")

    def test_oversize_download_is_stopped_without_retaining_extra_bytes(self) -> None:
        """Oversize download is stopped without retaining extra bytes."""
        error, report = self.run_download(b"abcd", envelope(b"abc"), headers={})
        self.assertEqual(error, "download_size_mismatch")
        self.assertLessEqual(integer_value(report["downloadedBytes"]), 3)
        self.assertLessEqual((self.directory / "artifact.zip").stat().st_size, 3)

    def test_incomplete_or_wrong_checksum_download_fails(self) -> None:
        """Incomplete or wrong checksum download fails."""
        error, _ = self.run_download(b"ab", envelope(b"abc"), headers={})
        self.assertEqual(error, "download_size_mismatch")
        (self.directory / "artifact.zip").unlink()
        error, _ = self.run_download(b"abc", dict(envelope(b"abc"), zipSha256="0" * 64))
        self.assertEqual(error, "zip_checksum_mismatch")

    def test_encoded_or_nonmatching_http_response_is_rejected(self) -> None:
        """Encoded or nonmatching http response is rejected."""
        for status, headers in (
            (206, {}),
            (200, {"Content-Encoding": "gzip"}),
            (200, {"Content-Length": "999"}),
        ):
            with self.subTest(status=status, headers=headers):
                path = self.directory / "artifact.zip"
                if path.exists():
                    path.unlink()
                error, _ = self.run_download(b"abc", status=status, headers=headers)
                self.assertEqual(error, "http_response_rejected")


@final
class FetchLifecycleTests(FetchTestCase):
    """Prove lock ownership, private evidence, and failure receipts across the handshake."""

    def __init__(self, methodName: str = "runTest") -> None:  # noqa: N803 - unittest's public constructor keyword.
        """Initialize typed, resource-free fixture state before unittest setup."""
        super().__init__(methodName)
        self.root = Path()
        self.config = Path()
        self.work = Path()
        self.data = b""
        self.patches: list[PatchHandle] = []
        self.opener = Opener(response=self.response)
        self.alarms: list[int] = []
        self.signals: list[tuple[int, Callable[[int, FrameType | None], object]]] = []

    def response(self) -> Response:
        """Read the current test's archive only when a download is requested."""
        return Response(self.data)

    def alarm(self, seconds: int) -> int:
        """Record deadline arming and cancellation without process-wide alarms."""
        self.alarms.append(seconds)
        return 0

    def register_signal(
        self, number: int, handler: Callable[[int, FrameType | None], object]
    ) -> None:
        """Retain typed handlers for deterministic interruption tests."""
        self.signals.append((number, handler))

    @override
    def setUp(self) -> None:
        """Create isolated fixtures and register reversible test substitutions."""
        super().setUp()
        self.root, self.config, self.work = [
            self.directory / name for name in ("public", "config", "work")
        ]
        for path in (self.root, self.config, self.work):
            path.mkdir(mode=0o700)
        for name in ("releases", "results"):
            (self.root / name).mkdir(mode=0o700)
        self.data = make_zip()
        self.patches = [
            patch.object(fetch, "ROOT", self.root),
            patch.object(fetch, "CONFIG", self.config),
            patch.object(fetch, "WORK", self.work),
            patch.object(os, "geteuid", return_value=0),
            patch.object(signal, "signal", side_effect=self.register_signal),
            patch.object(signal, "alarm", side_effect=self.alarm),
            patch.object(fetch, "protected", side_effect=self.protected_fixture),
            patch.object(shutil, "disk_usage", return_value=DiskUsage(free=10 * 1024**3)),
            patch.object(ssl, "create_default_context", return_value=None),
        ]
        for patcher in self.patches:
            _ = patcher.start()
            self.addCleanup(patcher.stop)
        self.opener = Opener(response=self.response)
        opener_patch = patch.object(fetch, "build_opener", return_value=self.opener)
        _ = opener_patch.start()
        self.addCleanup(opener_patch.stop)

    @staticmethod
    def protected_fixture(
        path: Path, *, directory: bool = False, mode: int = 0o600, limit: int | None = None
    ) -> None:
        """Replace only root ownership while exercising real mode, type, and size checks."""
        # Model root ownership only; real mode/type/size safety stays exercised.
        metadata = path.lstat()
        fetch.require(stat.S_IMODE(metadata.st_mode) == mode, "unsafe_filesystem")
        fetch.require(
            stat.S_ISDIR(metadata.st_mode) if directory else stat.S_ISREG(metadata.st_mode),
            "unsafe_filesystem",
        )
        if limit is not None:
            fetch.require(0 < metadata.st_size <= limit, "unsafe_filesystem")

    def execute(
        self, metadata: JsonObject | None = None, *, url: str = URL
    ) -> tuple[int, list[JsonObject]]:
        """Drive the two-line handshake and require redacted typed receipts."""
        output = io.StringIO()
        lines = [
            json.dumps(metadata or envelope(self.data)).encode() + b"\n",
            json.dumps({"url": url}).encode() + b"\n",
        ]
        parent = self

        class Input:
            """Verify the ready receipt is visible before supplying the signed URL."""

            def readline(self, limit: int = -1, /) -> bytes:
                """Return one bounded protocol message after checking handshake order."""
                parent.assertEqual(limit, fetch.LINE_BYTES + 1)
                if len(lines) == 1:
                    parent.assertEqual(
                        json_object(output.getvalue()), {"schemaVersion": 1, "status": "ready"}
                    )
                return lines.pop(0)

        code = fetch.main(Input(), output)
        receipts = [json_object(line) for line in output.getvalue().splitlines()]
        self.assertNotIn("PRIVATE_SIGNED_URL", output.getvalue())
        return code, receipts

    def test_handshake_precedes_url_read_and_success_is_durable_before_unlock(self) -> None:
        """Handshake precedes url read and success is durable before unlock."""
        original_write = fetch.write_json

        def checked_write(path: Path, value: JsonValue) -> None:
            if path.name == "fetch-outcome.json":
                with (
                    (self.work / "workload.lock").open("rb") as concurrent,
                    self.assertRaises(BlockingIOError),
                ):
                    fcntl.flock(concurrent, fcntl.LOCK_EX | fcntl.LOCK_NB)
            return original_write(path, value)

        with patch.object(fetch, "write_json", side_effect=checked_write):
            code, receipts = self.execute()
        self.assertEqual(code, 0)
        self.assertEqual(len(receipts), 2)
        report = receipts[-1]
        self.assertEqual(report["status"], "complete")
        self.assertIs(report["passed"], expr2=True)
        self.assertIs(report["settled"], expr2=True)
        self.assertIs(report["finalized"], expr2=True)
        attempt = Path(string(report, "evidence"))
        self.assertEqual(json_object((attempt / "fetch-outcome.json").read_text()), report)
        self.assertTrue(json_object((attempt / "outcome.json").read_text())["passed"])
        destination = self.root / "releases" / REVISION
        for name in ("image.tar", "release.json"):
            self.assertEqual((destination / name).stat().st_ino, (attempt / name).stat().st_ino)
            self.assertEqual(stat.S_IMODE((destination / name).stat().st_mode), 0o600)
        with (self.work / "workload.lock").open("rb") as concurrent:
            fcntl.flock(concurrent, fcntl.LOCK_EX | fcntl.LOCK_NB)
        self.assertNotIn(
            "PRIVATE_SIGNED_URL",
            "\n".join(path.read_text(errors="replace") for path in attempt.glob("*.json")),
        )
        self.assertIn(300, self.alarms)
        self.assertEqual(self.alarms[-1], 0)

    def test_refetch_retains_identical_destinations_and_all_original_attempts(self) -> None:
        """Refetch retains identical destinations and all original attempts."""
        first_code, first = self.execute()
        destination = self.root / "releases" / REVISION
        identity = {
            name: (destination / name).stat().st_ino for name in ("image.tar", "release.json")
        }
        second_code, second = self.execute()
        self.assertEqual((first_code, second_code), (0, 0))
        self.assertNotEqual(first[-1]["evidence"], second[-1]["evidence"])
        self.assertEqual(identity, {name: (destination / name).stat().st_ino for name in identity})
        self.assertEqual(len(self.opener.calls), 2)

    def test_changed_retained_file_is_not_overwritten_or_reported_successful(self) -> None:
        """Changed retained file is not overwritten or reported successful."""
        destination = self.root / "releases" / REVISION
        destination.mkdir(mode=0o700)
        retained = destination / "release.json"
        _ = retained.write_bytes(b"original different bytes")
        code, receipts = self.execute()
        self.assertEqual(code, 1)
        self.assertEqual(receipts[-1]["failureClass"], "retained_artifact_differs")
        self.assertIs(receipts[-1]["passed"], expr2=False)
        self.assertIs(receipts[-1]["settled"], expr2=True)
        self.assertEqual(retained.read_bytes(), b"original different bytes")
        self.assertFalse((destination / "image.tar").exists())

    def test_unfinished_operation_or_insufficient_disk_never_requests_a_url(self) -> None:
        """Unfinished operation or insufficient disk never requests a url."""
        for location in (self.work / "current.json", self.root / "release-state.json"):
            _ = location.write_text('{"schemaVersion":1,"finalized":false}')
            code, receipts = self.execute()
            self.assertEqual(code, 1)
            self.assertEqual(len(receipts), 1)
            self.assertEqual(receipts[-1]["failureClass"], "unfinished_operation")
            self.assertEqual(self.opener.calls, [])
            location.unlink()
        with patch.object(shutil, "disk_usage", return_value=DiskUsage(free=1)):
            code, receipts = self.execute()
        self.assertEqual(code, 1)
        self.assertEqual(len(receipts), 1)
        self.assertEqual(receipts[-1]["failureClass"], "insufficient_disk")
        self.assertEqual(self.opener.calls, [])

    def test_failed_build_download_hash_or_http_failure_preserves_failed_receipt(self) -> None:
        """Failed build download hash or http failure preserves failed receipt."""
        files = fixture_files()
        files["outcome.json"] = json.dumps(
            {"schemaVersion": 1, "revision": REVISION, "passed": False, "error": "failed"}
        ).encode()
        self.data = make_zip(files)
        code, receipts = self.execute()
        self.assertEqual(code, 1)
        self.assertEqual(receipts[-1]["failureClass"], "build_outcome_rejected")
        self.assertIs(receipts[-1]["passed"], expr2=False)
        self.assertFalse((self.root / "releases" / REVISION / "image.tar").exists())
        self.opener.error = URLError(URL)
        code, receipts = self.execute()
        self.assertEqual(code, 1)
        self.assertEqual(receipts[-1]["failureClass"], "transport_error")
        self.assertIs(receipts[-1]["settled"], expr2=True)

    def test_receipt_failure_can_never_return_a_successful_fetch(self) -> None:
        """Receipt failure can never return a successful fetch."""
        original = fetch.write_json

        def write(path: Path, value: JsonValue) -> None:
            if path.name == "fetch-outcome.json":
                message = "private storage error"
                raise OSError(message)
            return original(path, value)

        with patch.object(fetch, "write_json", side_effect=write):
            code, receipts = self.execute()
        self.assertEqual(code, 1)
        self.assertIs(receipts[-1]["passed"], expr2=False)
        self.assertIs(receipts[-1]["settled"], expr2=False)
        self.assertEqual(receipts[-1]["failureClass"], "receipt_write_failed")
        self.assertNotIn("private storage error", str(receipts))

    def test_url_wait_is_covered_by_the_global_deadline_and_retains_failure(self) -> None:
        """Url wait is covered by the global deadline and retains failure."""
        output = io.StringIO()
        first = json.dumps(envelope(self.data)).encode() + b"\n"

        class Input:
            """Simulate a deadline while the private signed URL has not yet arrived."""

            def readline(self, _limit: int = -1, /) -> bytes:
                """Provide the envelope once, then invoke the installed alarm handler."""
                if not output.getvalue():
                    return first
                handler = next(
                    handler for number, handler in parent.signals if number == signal.SIGALRM
                )
                _ = handler(signal.SIGALRM, None)
                message = "The alarm handler must terminate URL input"
                raise AssertionError(message)

        parent = self
        code = fetch.main(Input(), output)
        report = json_object(output.getvalue().splitlines()[-1])
        self.assertEqual(code, 1)
        self.assertEqual(report["phase"], "await_url")
        self.assertEqual(report["failureClass"], "deadline_exceeded")
        self.assertIs(report["passed"], expr2=False)
        self.assertIs(report["settled"], expr2=True)
        self.assertEqual(
            json_object((Path(string(report, "evidence")) / "fetch-outcome.json").read_text()),
            report,
        )
        self.assertEqual(self.opener.calls, [])

    def test_busy_workload_lock_refuses_before_ready_or_network(self) -> None:
        """Busy workload lock refuses before ready or network."""
        (self.work / "workload.lock").touch(mode=0o600)
        with (self.work / "workload.lock").open("rb") as existing:
            fcntl.flock(existing, fcntl.LOCK_EX | fcntl.LOCK_NB)
            code, receipts = self.execute()
        self.assertEqual(code, 1)
        self.assertEqual(len(receipts), 1)
        self.assertIs(receipts[-1]["passed"], expr2=False)
        self.assertEqual(self.opener.calls, [])

    def test_malicious_zip_metadata_is_refused_before_constructing_zipfile(self) -> None:
        """Malicious zip metadata is refused before constructing zipfile."""
        data = bytearray(self.data)
        end = data.rfind(b"PK\x05\x06")
        struct.pack_into("<L", data, end + 12, fetch.CHUNK + 1)
        self.data = bytes(data)
        with patch.object(
            zipfile, "ZipFile", side_effect=AssertionError("Unbounded ZIP metadata read")
        ) as constructor:
            code, receipts = self.execute()
        self.assertEqual(code, 1)
        self.assertEqual(receipts[-1]["failureClass"], "zip_shape_rejected")
        constructor.assert_not_called()


if __name__ == "__main__":
    _ = unittest.main()
