//! What the image is bound to: the external AWS-LC and the OpenSSL that
//! the build's native record names, held to the reviewed native record and
//! to every archive that was linked; and every shared library that the
//! binary loads, bound to the one rpm of the image that installed exactly
//! those bytes there.

use super::{canonical, small_hex, stopped};
use crate::{Fault, Result, awslc, digest, document::Document, elf, require};
use std::{
    collections::BTreeSet,
    ffi::OsStr,
    path::{Path, PathBuf},
};

/// A native record, a review, a library or an rpm that lacks a field or
/// has one of another kind.
const SHAPE: Fault = Fault("image_binding_shape");
/// The most files that the build of AWS-LC verified.
const MAX_VERIFIED: i128 = 20_000;
/// The most bytes of a header, a record or a static library of OpenSSL.
const MAX_ARTIFACT: i128 = 512 * 1024 * 1024;
/// The most archives of each static library of OpenSSL that were linked.
const MAX_INSTANCES: usize = 20_000;
/// The options that OpenSSL is configured with.
const CONFIGURED: [&str; 5] = ["no-shared", "no-dso", "no-module", "no-engine", "no-tests"];
/// What its configuration disables.
const DISABLED: [&str; 4] = ["shared", "dso", "module", "engine"];
/// The record of that, as the build writes it.
const DISABLED_RECORD: &[u8] = b"shared\ndso\nmodule\nengine\n";
/// The fields of the build's record of OpenSSL.
const BUILD: [&str; 5] = [
    "version_header",
    "configuration_header",
    "disabled_options_record",
    "disabled_options",
    "static_libraries",
];
/// The static libraries of OpenSSL, in their order.
const LIBRARIES: [&str; 2] = ["ssl", "crypto"];
/// Who links them.
const PROVIDERS: [&str; 2] = ["mediasoup-sys", "openssl-sys"];
/// A library of the binary that cannot be read once it was found.
const UNREAD: &str = "image_runtime_library_unread";

fn record(value: Option<&Document>) -> Result<&Document> {
    value.filter(|value| value.fields().is_some()).ok_or(SHAPE)
}

fn records(value: Option<&Document>) -> Result<&[Document]> {
    let items = value.and_then(Document::items).ok_or(SHAPE)?;
    require(items.iter().all(|item| item.fields().is_some()), SHAPE.0)?;
    Ok(items)
}

/// The field `name` of `item`, which must be there.
fn field<'a>(item: &'a Document, name: &str) -> Result<&'a Document> {
    item.get(name).ok_or(SHAPE)
}

/// The field `name` of `item`, and nothing where it has none, as the
/// reference read a field that may be missing.
fn held<'a>(item: &'a Document, name: &str) -> &'a Document {
    item.get(name).unwrap_or(&Document::Null)
}

fn words(value: &Document) -> Result<&str> {
    value.text().ok_or(SHAPE)
}

fn names(value: &Document) -> BTreeSet<&str> {
    let fields = value.fields().unwrap_or_default();
    fields.iter().map(|(name, _)| name.as_str()).collect()
}

/// Whether two values are the same whatever the order of their records'
/// fields, as the reference compared records.
fn same(left: &Document, right: &Document) -> bool {
    left.clone().sorted() == right.clone().sorted()
}

/// Hold the external AWS-LC of the native record `native` to the reviewed
/// component `expected`: its release, its sources and its licence, the
/// options that it was built with, every file that it installed, and the
/// one archive of it that was linked.
pub fn aws_lc_build_binding(native: &Document, expected: &Document) -> Result<()> {
    let component = record(native.get("registry_component"))?;
    let system = record(expected.get("system"))?;
    let mut bound = true;
    for name in [
        "name",
        "version",
        "revision",
        "crate",
        "crate_version",
        "license",
    ] {
        bound = bound && same(held(component, name), field(expected, name)?);
    }
    bound = bound
        && same(
            held(component, "source_sha256"),
            field(record(expected.get("source"))?, "sha256")?,
        );
    bound = bound && same(held(component, "native_source"), field(system, "source")?);
    bound = bound
        && same(
            held(component, "native_license_sha256"),
            field(system, "source_license_sha256")?,
        );
    bound = bound
        && same(
            held(component, "bindgen_source"),
            field(system, "bindgen_source")?,
        );
    let count = held(component, "native_verified_files").whole();
    bound = bound && count.is_some_and(|count| (1..=MAX_VERIFIED).contains(&count));
    require(bound, "image_awslc_source_binding")?;
    let build = record(component.get("build"))?;
    let files = record(build.get("files"))?;
    let options = awslc::OPTIONS.map(|(name, value)| (name.to_owned(), Document::from(value)));
    let options = Document::Record(options.to_vec());
    let fields = BTreeSet::from([
        "configure_options",
        "installer_sha256",
        "bindgen_sha256",
        "files",
    ]);
    let mut installed = names(build) == fields;
    installed = installed
        && same(
            field(build, "configure_options")?,
            field(system, "configure_options")?,
        );
    installed = installed && same(field(build, "configure_options")?, &options);
    installed = installed
        && same(
            field(build, "installer_sha256")?,
            field(system, "installer_sha256")?,
        );
    installed = installed && small_hex(words(field(build, "bindgen_sha256")?)?, 64);
    installed = installed && names(files) == BTreeSet::from(awslc::FILES);
    for (_, value) in files.fields().unwrap_or_default() {
        installed = installed && small_hex(words(value)?, 64);
    }
    let file = |name: &str| held(files, name);
    installed = installed
        && same(
            file("share/simplestchat/aws-lc-source.tar.gz"),
            field(record(system.get("source"))?, "sha256")?,
        );
    installed = installed
        && same(
            file("share/simplestchat/bindgen-cli.crate"),
            field(record(system.get("bindgen_source"))?, "sha256")?,
        );
    installed = installed
        && same(
            field(component, "bindings_sha256")?,
            file("share/rust/aws_lc_bindings.rs"),
        );
    require(installed, "image_awslc_build_binding")?;
    let mut linked = Vec::new();
    for item in records(native.get("static_archives"))? {
        if held(item, "provider") == &Document::from("aws-lc-sys") {
            linked.push(item);
        }
    }
    let [archive] = linked[..] else {
        return Err(Fault("image_awslc_archive_binding"));
    };
    let bound = held(archive, "library") == &Document::from("crypto-awslc")
        && same(held(archive, "sha256"), file("lib/libcrypto-awslc.a"))
        && words(field(archive, "path")?)?.ends_with("/lib/libcrypto-awslc.a");
    require(bound, "image_awslc_archive_binding")
}

/// Hold the OpenSSL of the native record `native` to the reviewed record
/// `expected`: its release, source and options; its headers and the record
/// of what it disables, under one prefix; and both of its static
/// libraries, each linked by both providers from the same directories with
/// exactly the bytes that the build installed.
pub fn openssl_build_binding(native: &Document, expected: &Document) -> Result<()> {
    let component = record(native.get("openssl"))?;
    let mut wanted = names(expected);
    wanted.insert("build");
    let mut bound = names(component) == wanted;
    for (name, value) in expected.fields().unwrap_or_default() {
        bound = bound && same(field(component, name)?, value);
    }
    let configured = Document::List(CONFIGURED.map(Document::from).to_vec());
    bound = bound && field(component, "configure_options")? == &configured;
    require(bound, "image_openssl_source_binding")?;
    let build = record(component.get("build"))?;
    let disabled = Document::List(DISABLED.map(Document::from).to_vec());
    let configuration =
        names(build) == BTreeSet::from(BUILD) && field(build, "disabled_options")? == &disabled;
    require(configuration, "image_openssl_configuration")?;
    let mut headers = Vec::new();
    for name in [
        "version_header",
        "configuration_header",
        "disabled_options_record",
    ] {
        headers.push(record(build.get(name))?);
    }
    let libraries = records(build.get("static_libraries"))?;
    let named = libraries.len() == LIBRARIES.len()
        && libraries
            .iter()
            .zip(LIBRARIES)
            .all(|(item, name)| held(item, "library") == &Document::from(name));
    require(named, "image_openssl_archives")?;
    // A header is no library, whatever its fields; the reference took one
    // that was the same record as a library for that library.
    let artifacts = headers.iter().map(|header| (*header, false));
    for (item, library) in artifacts.chain(libraries.iter().map(|item| (item, true))) {
        let path = words(field(item, "path")?)?;
        let mut fields = BTreeSet::from(["path", "sha256", "size"]);
        if library {
            fields.insert("library");
        }
        let identity = names(item) == fields
            && canonical(path)
            && small_hex(words(field(item, "sha256")?)?, 64)
            && field(item, "size")?
                .whole()
                .is_some_and(|size| (1..=MAX_ARTIFACT).contains(&size));
        require(identity, "image_openssl_artifact_identity")?;
    }
    let suffix = "/include/openssl/opensslv.h";
    let first = words(field(headers[0], "path")?)?;
    let prefix = first.strip_suffix(suffix).unwrap_or(first);
    let placed = |item: &Document, end: &str| {
        held(item, "path").text() == Some(format!("{prefix}{end}").as_str())
    };
    let recorded = held(headers[2], "sha256").text() == Some(digest(DISABLED_RECORD).as_str())
        && held(headers[2], "size").whole() == i128::try_from(DISABLED_RECORD.len()).ok();
    let configured = placed(headers[0], suffix)
        && placed(headers[1], "/include/openssl/configuration.h")
        && placed(headers[2], "/share/simplestchat/openssl-disabled.txt")
        && recorded;
    require(configured, "image_openssl_configuration_identity")?;
    let linked = records(native.get("static_archives"))?;
    let mut previous: Option<BTreeSet<(&str, &str)>> = None;
    for library in libraries {
        let name = words(field(library, "library")?)?;
        let mut matches = Vec::new();
        for item in linked {
            if field(item, "library")?.text() == Some(name) {
                matches.push(item);
            }
        }
        let mut instances = BTreeSet::new();
        for item in &matches {
            let provider = words(field(item, "provider")?)?;
            instances.insert((provider, words(field(item, "out_dir")?)?));
        }
        let providers: BTreeSet<&str> = instances.iter().map(|(provider, _)| *provider).collect();
        let mut bound = placed(library, &format!("/lib/lib{name}.a"))
            && (1..=MAX_INSTANCES).contains(&matches.len())
            && instances.len() == matches.len()
            && providers == BTreeSet::from(PROVIDERS)
            && previous
                .as_ref()
                .is_none_or(|previous| *previous == instances)
            && instances.iter().all(|(_, directory)| canonical(directory));
        for item in &matches {
            bound = bound && field(item, "size")?.whole().is_some() && {
                let mut alike = true;
                for key in ["path", "sha256", "size"] {
                    alike = alike && field(item, key)? == field(library, key)?;
                }
                alike
            };
        }
        require(bound, "image_openssl_archive_binding")?;
        previous = Some(instances);
    }
    Ok(())
}

/// The file that the image path `name` leads to below `rootfs`, through
/// the image's own links: `name` must be absolute and spelled the one way
/// it can be, and whatever keeps it from leading to a regular file of the
/// image refuses it as a path of the image's rpms.
fn runtime_file_path(rootfs: &Path, name: &str) -> Result<PathBuf> {
    const PATH: Fault = Fault("image_runtime_rpm_path");
    require(canonical(name), PATH.0)?;
    elf::image_path(rootfs, name).map_err(|_| PATH)
}

/// The last step of a path, as the reference named it.
fn last_step(path: &str) -> &str {
    let mut steps = path.rsplit('/');
    steps
        .find(|step| !matches!(*step, "" | "."))
        .unwrap_or_default()
}

/// Whether `mode` is that of a regular file, in the sixteen bits in which
/// rpm keeps a file's mode.
fn regular(mode: &Document) -> bool {
    let mode = mode.whole();
    mode.is_some_and(|mode| (0..=0o177_777).contains(&mode) && mode & 0o170_000 == 0o100_000)
}

/// Every shared library of `elf` that the binary loads, bound to the one
/// regular file of an rpm of `packages` that the image has at that path,
/// through its own links, with exactly the library's bytes.
pub fn runtime_rpm_bindings(
    packages: &[Document],
    elf: &Document,
    rootfs: &Path,
) -> Result<Vec<Document>> {
    let root = rootfs.symlink_metadata().is_ok_and(|entry| entry.is_dir());
    require(root, "image_runtime_root")?;
    let mut bound = Vec::new();
    for library in field(elf, "libraries")?.items().ok_or(SHAPE)? {
        let library = record(Some(library))?;
        let name = words(field(library, "path")?)?;
        let resolved = runtime_file_path(rootfs, name)?;
        let content = elf::read_binary(&resolved).map_err(|stop| stopped(stop, UNREAD, UNREAD))?;
        let below = resolved.strip_prefix(rootfs).ok().and_then(Path::to_str);
        let identity = below.is_some_and(|below| format!("/{below}") == name)
            && field(library, "bytes")?.whole() == i128::try_from(content.len()).ok()
            && field(library, "sha256")?.text() == Some(digest(&content).as_str());
        require(identity, "image_runtime_library_identity")?;
        let file_name = resolved
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or_default();
        let mut owners = Vec::new();
        for package in packages {
            if field(package, "type")? != &Document::from("rpm") {
                continue;
            }
            let metadata = record(package.get("metadata"))?;
            for file in field(metadata, "files")?.items().ok_or(SHAPE)? {
                let file = record(Some(file))?;
                let recorded = words(field(file, "path")?)?;
                // Only a file of the library's name may own it: the other
                // files of an rpm need not be in the image.
                if last_step(recorded) != file_name
                    || runtime_file_path(rootfs, recorded)? != resolved
                {
                    continue;
                }
                let checksum = record(file.get("digest"))?;
                let owned = regular(field(file, "mode")?)
                    && {
                        let size = field(file, "size")?;
                        size.whole().is_some() && size == field(library, "bytes")?
                    }
                    && field(checksum, "algorithm")? == &Document::from("sha256")
                    && field(checksum, "value")? == field(library, "sha256")?;
                require(owned, "image_runtime_rpm_digest")?;
                owners.push(Document::record([
                    ("path", recorded.into()),
                    ("resolvedPath", name.into()),
                    ("bytes", field(library, "bytes")?.clone()),
                    ("sha256", field(library, "sha256")?.clone()),
                    ("package", field(package, "purl")?.clone()),
                    ("sourceRpm", field(metadata, "sourceRpm")?.clone()),
                ]));
            }
        }
        let [owner] =
            <[Document; 1]>::try_from(owners).map_err(|_| Fault("image_runtime_rpm_owner"))?;
        bound.push(owner);
    }
    require(!bound.is_empty(), "image_runtime_rpm_empty")?;
    Ok(bound)
}

#[cfg(test)]
#[path = "bindings_tests.rs"]
mod tests;
