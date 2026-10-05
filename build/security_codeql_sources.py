"""Bind generated native locations to exact maintained wrap archives and overlays.

This reader never downloads, extracts or executes upstream content. The caller
supplies the authenticated cache populated by the vendor integrity gate. Missing
pins, absent cache entries and a disagreeing extracted source all fail closed.
"""

from __future__ import annotations

import configparser
import gzip
import hashlib
import io
import json
import re
import tarfile
from pathlib import Path, PurePosixPath
from typing import TYPE_CHECKING

import security_vendor as vendor
from security_tools import bounded_file, require

# isort: split
import bounded_process

if TYPE_CHECKING:
    from release_json import JsonObject

MANIFEST = "vendor/integrity.json"
SUBPROJECTS = "vendor/mediasoup-sys-0.19.0/subprojects"
MAX_SOURCE = 4 * 1024**2


def tracked_bytes(root: Path, revision: str, name: str, limit: int = MAX_SOURCE) -> bytes:
    """Read a regular source only when its complete bytes match the selected revision."""
    _ = vendor.relative_path(name)
    require(re.fullmatch(r"[a-f0-9]{40}", revision), "codeql_revision")
    path = root / name
    require(path.resolve() == path.absolute(), "codeql_source_symlink")
    data = bounded_file(path, limit)
    status, expected, _ = bounded_process.run(
        ["git", "show", f"{revision}:{name}"],
        cwd=root,
        limits=bounded_process.Limits(timeout=20, stdout=limit, stderr=65536),
    )
    require(status == 0 and expected == data, "codeql_source_revision_mismatch")
    return data


def archive_files(source: vendor.Source, data: bytes) -> dict[str, bytes]:
    """Retain regular members while treating authenticated tar symlinks as unavailable.

    The FlatBuffers source release contains documentation and Java-test symlinks.
    They are never followed or exposed as readable source; a member beneath any
    such path also fails. Other non-regular kinds remain unsupported.
    """
    require(
        len(data) <= vendor.MAX_DOWNLOAD and vendor.sha256(data) == source.sha256,
        "codeql_archive_digest",
    )
    if source.format != "tar.gz":
        return vendor.archive_files(source, data)
    budget = vendor.ArchiveBudget(set(), {})
    links: set[str] = set()
    with gzip.GzipFile(fileobj=io.BytesIO(data)) as compressed:
        expanded = compressed.read(vendor.MAX_CONTENT + 1)
    require(len(expanded) <= vendor.MAX_CONTENT, "codeql_archive_expansion")
    with tarfile.open(fileobj=io.BytesIO(expanded), mode="r|") as archive:
        for member in archive:
            require(
                (member.isfile() or member.isdir() or member.issym()) and member.sparse is None,
                "codeql_archive_kind",
            )
            key = budget.reserve(member.name, member.size, directory=member.isdir())
            require(
                not any(str(parent) in links for parent in PurePosixPath(key).parents),
                "codeql_archive_link_ancestor",
            )
            if member.issym():
                require(member.size == 0, "codeql_archive_link_size")
                links.add(key)
            elif member.isdir():
                require(member.size == 0, "codeql_archive_directory_size")
            else:
                stream = archive.extractfile(member)
                require(stream is not None, "codeql_archive_member_missing")
                if stream is None:
                    continue
                with stream:
                    body = stream.read(vendor.MAX_MEMBER + 1)
                require(len(body) == member.size, "codeql_archive_member_size")
                budget.files[key] = body
    return budget.files


class NativeSources:
    """Resolve only declared Meson source roots with bounded authenticated archive reads."""

    def __init__(self, root: Path, revision: str, cache: Path) -> None:
        """Bind the manifest, wrap files and private offline cache before source resolution."""
        self.root: Path = root
        self.revision: str = revision
        self.cache: Path = vendor.private_directory(cache, create=False)
        manifest_bytes = tracked_bytes(root, revision, MANIFEST, vendor.MAX_MANIFEST)
        self.manifest_sha: str = vendor.sha256(manifest_bytes)
        self.manifest: vendor.Manifest = vendor.parse_manifest(manifest_bytes)
        self.wraps: dict[str, tuple[vendor.Wrap, bytes]] = {}
        self.archives: dict[str, dict[str, bytes]] = {}
        self.contents: dict[str, tuple[dict[str, bytes], JsonObject]] = {}
        self.total_bytes: int = 0
        for wrap in self.manifest.wraps:
            require(str(PurePosixPath(wrap.path).parent) == SUBPROJECTS, "codeql_wrap_root")
            data = tracked_bytes(root, revision, wrap.path)
            parser = configparser.ConfigParser(interpolation=None, strict=True)
            parser.read_string(data.decode())
            directory = vendor.relative_path(parser["wrap-file"]["directory"])
            require(
                "/" not in directory and directory not in {"packagefiles", "packagecache"},
                "codeql_wrap_directory",
            )
            selected = SUBPROJECTS + "/" + directory
            require(selected not in self.wraps, "codeql_duplicate_wrap_directory")
            self.wraps[selected] = wrap, data

    def archive(self, source: vendor.Source) -> dict[str, bytes]:
        """Authenticate complete inputs and bound aggregate retained decoded content."""
        if source.identifier not in self.archives:
            data = vendor.source_bytes(source, self.cache, offline=True)
            files = archive_files(source, data)
            self.total_bytes += sum(len(body) for body in files.values())
            require(self.total_bytes <= vendor.MAX_CONTENT, "codeql_native_content_limit")
            self.archives[source.identifier] = files
        return self.archives[source.identifier]

    def overlay(self, wrap: vendor.Wrap) -> dict[str, bytes]:
        """Verify every local packagefile against both Git and the maintained archive delta."""
        if wrap.patch_directory is None:
            return {}
        prefix = SUBPROJECTS + "/packagefiles/" + wrap.patch_directory
        selected = [
            tree
            for tree in self.manifest.trees
            if prefix == tree.path or prefix.startswith(tree.path + "/")
        ]
        require(bool(selected), "codeql_overlay_manifest")
        tree = max(selected, key=lambda item: len(item.path))
        relative = prefix.removeprefix(tree.path).removeprefix("/")
        upstream_prefix = tree.prefix + ("/" + relative if relative else "")
        changes = tuple(
            vendor.Change(
                item.path.removeprefix(relative + "/") if relative else item.path,
                item.upstream_sha256,
                item.vendored_sha256,
            )
            for item in tree.changes
            if not relative or item.path.startswith(relative + "/")
        )
        files = vendor.tree_files(self.root / prefix)
        for name, data in files.items():
            require(
                data == tracked_bytes(self.root, self.revision, prefix + "/" + name),
                "codeql_overlay_revision",
            )
        original = self.archive(self.manifest.sources[tree.source])
        _, _, issues = vendor.compare_tree(
            vendor.Tree(prefix, tree.source, upstream_prefix, changes),
            {
                name: data
                for name, data in original.items()
                if name.startswith(upstream_prefix + "/")
            },
            files,
        )
        require(not issues, "codeql_overlay_deviation")
        return files

    def content(self, prefix: str) -> tuple[dict[str, bytes], JsonObject]:
        """Apply only the declared remote patch or verified packagefiles overlay."""
        if prefix in self.contents:
            return self.contents[prefix]
        wrap, wrap_data = self.wraps[prefix]
        overlays = self.overlay(wrap)
        local = {wrap.path: wrap_data}
        local.update(
            {
                SUBPROJECTS + "/packagefiles/" + str(wrap.patch_directory) + "/" + name: data
                for name, data in overlays.items()
            }
        )
        vendor.validate_wrap(wrap, self.manifest, local)
        source = self.manifest.sources[wrap.source]
        directory = PurePosixPath(prefix).name + "/"
        original = self.archive(source)
        require(bool(original), "codeql_empty_archive")
        require(all(name.startswith(directory) for name in original), "codeql_archive_root")
        files = {name.removeprefix(directory): data for name, data in original.items()}
        evidence: JsonObject = {
            "sourceArchiveSha256": source.sha256,
            "sourceWrapSha256": vendor.sha256(wrap_data),
            "sourceManifestSha256": self.manifest_sha,
        }
        if wrap.patch_source is not None:
            patch_source = self.manifest.sources[wrap.patch_source]
            patches = self.archive(patch_source)
            require(
                bool(patches) and all(name.startswith(directory) for name in patches),
                "codeql_patch_root",
            )
            files.update({name.removeprefix(directory): data for name, data in patches.items()})
            evidence["sourcePatchSha256"] = patch_source.sha256
        if overlays:
            files.update(overlays)
            evidence["sourceOverlaySha256"] = hashlib.sha256(
                json.dumps(
                    {name: vendor.sha256(data) for name, data in overlays.items()},
                    sort_keys=True,
                    separators=(",", ":"),
                ).encode()
            ).hexdigest()
        self.contents[prefix] = files, evidence
        return files, evidence

    def identity(self, name: str) -> JsonObject | None:
        """Use archive members for API reads and also verify extracted bytes when present."""
        _ = vendor.relative_path(name)
        prefixes = [prefix for prefix in self.wraps if name.startswith(prefix + "/")]
        require(len(prefixes) <= 1, "codeql_native_ambiguous")
        if not prefixes:
            return None
        prefix = prefixes[0]
        member = name.removeprefix(prefix + "/")
        files, evidence = self.content(prefix)
        require(member in files and len(files[member]) <= MAX_SOURCE, "codeql_native_member")
        expected = files[member]
        path = self.root / name
        require(path.resolve() == path.absolute(), "codeql_source_symlink")
        if path.exists() or path.is_symlink():
            require(
                bounded_file(path, MAX_SOURCE) == expected,
                "codeql_extracted_source_mismatch",
            )
        return {**evidence, "sourceMember": member, "sourceSha256": vendor.sha256(expected)}
