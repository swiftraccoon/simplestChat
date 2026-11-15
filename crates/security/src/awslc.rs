//! The external AWS-LC build that the unmodified Rust wrapper links: its
//! source, its binding generator and its symbol namespace are bound to the
//! reviewed installer, and an installation is bound to its receipt, to the
//! authenticated source and to what Cargo linked.

use crate::{
    digest,
    document::Document,
    vendor::{
        Bounds, Problem, Verdict,
        archive::{self, Kind},
        compare::Files,
        diff,
        manifest::{self, Source},
        tree,
    },
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

/// The bounds of the release archive, which is larger than a vendored
/// source may be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Limits {
    /// The most bytes it, or a file of an installation, may have.
    archive: usize,
    /// The most bytes it may unpack to.
    expanded: u64,
    /// The most bytes one of its members may have.
    member: u64,
    /// The most members it may have.
    entries: usize,
}

impl Limits {
    const REVIEWED: Self = Self {
        archive: 192 * 1024 * 1024,
        expanded: 384 * 1024 * 1024,
        member: 64 * 1024 * 1024,
        entries: Bounds::REVIEWED.entries,
    };
}

/// The reviewed options of the build.
pub const OPTIONS: [(&str, &str); 13] = [
    ("CMAKE_BUILD_TYPE", "Release"),
    ("BUILD_SHARED_LIBS", "OFF"),
    ("BUILD_TESTING", "OFF"),
    ("BUILD_TOOL", "OFF"),
    ("BUILD_LIBSSL", "OFF"),
    ("ENABLE_SOURCE_MODIFICATION", "OFF"),
    ("BUILD_AWSLC_PROVIDER", "OFF"),
    ("ENABLE_CRYPTO_POLICIES", "OFF"),
    ("GENERATE_RUST_BINDINGS", "ON"),
    ("RUST_BINDINGS_TARGET_VERSION", "1.70"),
    ("BORINGSSL_PREFIX", "simplestchat_awslc_5_11_0"),
    ("CMAKE_POSITION_INDEPENDENT_CODE", "ON"),
    ("CMAKE_INSTALL_LIBDIR", "lib"),
];
/// The namespace of the build's symbols.
const PREFIX: &str = "simplestchat_awslc_5_11_0";
/// The files of an installation that its receipt names.
pub const FILES: [&str; 8] = [
    "lib/libcrypto-awslc.a",
    "share/rust/aws_lc_bindings.rs",
    "include/openssl/base.h",
    "include/openssl/boringssl_prefix_symbols.h",
    "share/simplestchat/aws-lc-source.tar.gz",
    "share/simplestchat/bindgen-cli.crate",
    "share/simplestchat/CMakeCache.txt",
    "share/simplestchat/symbols.txt",
];
const ARCHIVE: &str = "share/simplestchat/aws-lc-source.tar.gz";
const GENERATOR: &str = "share/simplestchat/bindgen-cli.crate";
const HEADER: &str = "include/openssl/base.h";
const LIBRARY: &str = "lib/libcrypto-awslc.a";
const BINDINGS: &str = "share/rust/aws_lc_bindings.rs";

fn problem(text: &str) -> Problem {
    Problem(text.to_owned())
}

/// The field `name` of a record, which must have it.
fn field<'a>(record: &'a Document, name: &str) -> Verdict<&'a Document> {
    let held = record.get(name);
    held.ok_or_else(|| Problem(format!("Missing field: {name}")))
}

/// Whether two values are one, with the fields of a record in any order.
fn same(left: &Document, right: &Document) -> bool {
    match (left, right) {
        (Document::List(left), Document::List(right)) => {
            let alike = |(left, right): (&Document, &Document)| same(left, right);
            left.len() == right.len() && left.iter().zip(right).all(alike)
        }
        (Document::Record(left), Document::Record(right)) => {
            let found = |(name, value): &(String, Document)| {
                let other = right.iter().find(|(other, _)| other == name);
                other.is_some_and(|(_, other)| same(value, other))
            };
            left.len() == right.len() && left.iter().all(found)
        }
        _ => left == right,
    }
}

/// Whether `options` are a record of exactly the reviewed options.
fn reviewed(options: &Document) -> Verdict<bool> {
    let held = options.fields();
    let held = held.ok_or_else(|| problem("Expected a JSON object"))?;
    let named =
        |(name, value): &(&str, &str)| options.get(name).and_then(Document::text) == Some(*value);
    Ok(held.len() == OPTIONS.len() && OPTIONS.iter().all(named))
}

/// Three numbers with dots between them.
fn version(text: &str) -> bool {
    let number = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    text.split('.').count() == 3 && text.split('.').all(number)
}

fn contains(text: &[u8], words: &str) -> bool {
    let words = words.as_bytes();
    text.windows(words.len()).any(|window| window == words)
}

/// Hold the reviewed record of the component to the installer of the
/// checkout `root`: the source and the generator are the releases of the
/// versions that the installer pins, by their names, addresses and digests.
pub fn validate(root: &Path, component: &Document) -> Verdict<()> {
    let names = [
        "source",
        "source_license_sha256",
        "bindgen_source",
        "installer_sha256",
        "symbol_prefix",
        "configure_options",
    ];
    let system = manifest::fields(field(component, "system")?, &names)?;
    let part = |name: &str| system.get(name).unwrap_or(&Document::Null);
    manifest::digest(part("source_license_sha256"))?;
    let source = manifest::source(part("source"))?;
    let generator = manifest::source(part("bindgen_source"))?;
    let release = manifest::string(field(component, "version")?)?;
    let installer = tree::read_regular(
        &root.join("build/install-aws-lc.sh"),
        Bounds::REVIEWED.record as u64,
    )?;
    let differs = || problem("AWS-LC external source or installer identity differs");
    let generated = generator.identifier.strip_prefix("bindgen-cli-");
    let generated = generated.filter(|generated| version(generated));
    let (Some(generated), true) = (generated, version(release)) else {
        return Err(differs());
    };
    let pinned = source.identifier == format!("aws-lc-{release}")
        && source.url
            == format!("https://github.com/aws/aws-lc/archive/refs/tags/v{release}.tar.gz")
        && source.format == "tar.gz"
        && generator.url
            == format!("https://static.crates.io/crates/bindgen-cli/bindgen-cli-{generated}.crate")
        && generator.format == "tar.gz";
    if !pinned || digest(&installer) != manifest::digest(part("installer_sha256"))? {
        return Err(differs());
    }
    if part("symbol_prefix").text() != Some(PREFIX) || !reviewed(part("configure_options"))? {
        return Err(differs());
    }
    let pins = [
        ("awslc_version", release),
        ("awslc_sha256", source.sha256.as_str()),
        ("awslc_prefix", PREFIX),
        ("bindgen_version", generated),
        ("bindgen_sha256", generator.sha256.as_str()),
    ];
    if !pins
        .iter()
        .all(|(name, value)| contains(&installer, &format!("{name}='{value}'\n")))
    {
        return Err(problem(
            "AWS-LC installer pin differs from reviewed metadata",
        ));
    }
    Ok(())
}

/// The members of the release archive `data` that `source` pins. Upstream
/// ships packed test vectors: every member is named, and only the bytes of
/// the version header and of the licences are kept.
fn release_files(source: &Source, data: &[u8], limits: &Limits) -> Verdict<Files> {
    if data.len() > limits.archive || digest(data) != source.sha256 {
        return Err(problem("AWS-LC source SHA-256 or size differs"));
    }
    let budget = "AWS-LC archive expansion budget exceeded";
    let expanded = archive::unpacked(data, limits.expanded, budget)?;
    let mut files = Files::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    archive::tar_each(&expanded, &mut |name, kind, size, body| {
        if kind == Kind::Other {
            return Err(problem("AWS-LC unsupported archive member"));
        }
        let key = manifest::relative_path(name.strip_suffix('/').unwrap_or(name))?.to_owned();
        if seen.contains(&key) || seen.len() >= limits.entries {
            return Err(problem("AWS-LC duplicate or excessive archive members"));
        }
        let above = key
            .match_indices('/')
            .any(|(at, _)| files.contains_key(&key[..at]));
        let below = format!("{key}/");
        let inside = kind == Kind::File && seen.iter().any(|other| other.starts_with(&below));
        if above || inside {
            return Err(problem("AWS-LC archive file/directory collision"));
        }
        seen.insert(key.clone());
        if size > limits.member {
            return Err(problem("AWS-LC archive member exceeds its budget"));
        }
        if kind == Kind::Directory {
            if size != 0 {
                return Err(problem("AWS-LC nonempty archive directory"));
            }
            return Ok(());
        }
        let mut content = Vec::new();
        let read = body.read_to_end(&mut content);
        read.map_err(|_| problem("Unreadable tar archive"))?;
        if content.len() as u64 != size {
            return Err(problem("AWS-LC archive member size differs"));
        }
        let kept = key.ends_with("/include/openssl/base.h") || key.ends_with("/LICENSE");
        files.insert(key, if kept { content } else { Vec::new() });
        Ok(())
    })?;
    Ok(files)
}

/// The files of a source below the directory of its name, by their paths
/// from there.
fn source_files(source: &Source, data: &[u8], release: bool) -> Verdict<Files> {
    let files = if release {
        release_files(source, data, &Limits::REVIEWED)?
    } else {
        archive::files(source, data, &Bounds::REVIEWED)?
    };
    let prefix = format!("{}/", source.identifier);
    let below =
        |(name, body): (String, Vec<u8>)| Some((name.strip_prefix(&prefix)?.to_owned(), body));
    let whole = files.len();
    let files: Files = files.into_iter().filter_map(below).collect();
    if files.len() != whole {
        return Err(problem("AWS-LC source archive prefix differs"));
    }
    Ok(files)
}

/// Hold the options that the build was configured with, as its own cache
/// has them, to the reviewed ones.
fn verify_options(cache: &[u8]) -> Verdict<()> {
    let text = std::str::from_utf8(cache);
    let text = text.map_err(|_| problem("AWS-LC build cache is no text"))?;
    let mut options: Vec<(&str, &str)> = Vec::new();
    for line in diff::lines(text) {
        // A line that begins as a comment names no reviewed option, so it
        // is read as any other.
        let line = line.trim_end_matches(diff::ends);
        let option = line.split_once('=').filter(|_| line.contains(':'));
        let Some((key, value)) = option else {
            continue;
        };
        let key = key.split(':').next().unwrap_or(key);
        options.retain(|(held, _)| *held != key);
        options.push((key, value));
    }
    if !OPTIONS.iter().all(|option| options.contains(option)) {
        return Err(problem("AWS-LC actual CMake build options differ"));
    }
    Ok(())
}

/// The evidence of the installation at `prefix`: its receipt names the
/// reviewed options and installer and the digest of every file; its source
/// is the authenticated release with the reviewed licence and the header
/// that is installed; its generator has its lock; its headers and its
/// build cache say the reviewed version, namespace and options; and Cargo
/// generated the installed bindings and linked the installed library.
pub fn evidence(
    prefix: &Path,
    component: &Document,
    archives: &[Document],
    generated: &[PathBuf],
) -> Verdict<Document> {
    let bounds = Bounds::REVIEWED;
    let object = || problem("Expected a JSON object");
    let system = field(component, "system")?;
    system.fields().ok_or_else(object)?;
    let receipt = tree::read_regular(
        &prefix.join("share/simplestchat/build.json"),
        bounds.record as u64,
    )?;
    let receipt = Document::read(&receipt).map_err(manifest::unread)?;
    let names = [
        "configure_options",
        "installer_sha256",
        "bindgen_sha256",
        "files",
    ];
    let receipt = manifest::fields(&receipt, &names)?;
    let noted = |name: &str| receipt.get(name).unwrap_or(&Document::Null);
    let hashes = noted("files");
    let listed = hashes.fields().ok_or_else(object)?;
    let named = listed.len() == FILES.len() && FILES.iter().all(|name| hashes.get(name).is_some());
    let reviewed = |name: &str| Ok::<bool, Problem>(same(noted(name), field(system, name)?));
    if !named || !reviewed("configure_options")? || !reviewed("installer_sha256")? {
        return Err(problem("AWS-LC installed build receipt differs"));
    }
    manifest::digest(noted("bindgen_sha256"))?;
    let mut contents = Vec::new();
    for name in FILES {
        contents.push((
            name,
            tree::read_regular(&prefix.join(name), Limits::REVIEWED.archive as u64)?,
        ));
    }
    let hash = |name: &str| hashes.get(name).unwrap_or(&Document::Null);
    for (name, body) in &contents {
        if digest(body) != manifest::digest(hash(name))? {
            return Err(problem(
                "AWS-LC installed artifact differs from its build receipt",
            ));
        }
    }
    let content = |name: &str| {
        let held = contents.iter().find(|(held, _)| *held == name);
        held.map_or(&[][..], |(_, body)| body.as_slice())
    };
    let source = manifest::source(field(system, "source")?)?;
    let files = source_files(&source, content(ARCHIVE), true)?;
    let license = field(system, "source_license_sha256")?;
    let licensed = files.get("LICENSE").map(|body| digest(body));
    if !licensed.is_some_and(|licensed| license.text() == Some(&licensed)) {
        return Err(problem("AWS-LC native source license differs"));
    }
    let generator = manifest::source(field(system, "bindgen_source")?)?;
    if !source_files(&generator, content(GENERATOR), false)?.contains_key("Cargo.lock") {
        return Err(problem(
            "AWS-LC binding generator lacks an authenticated lockfile",
        ));
    }
    let header = content(HEADER);
    let differs = || problem("AWS-LC installed version header differs from authenticated source");
    if files.get(HEADER).map(Vec::as_slice) != Some(header) {
        return Err(differs());
    }
    let release = field(component, "version")?.text();
    let release = release.ok_or_else(differs)?;
    let versioned = format!("#define AWSLC_VERSION_NUMBER_STRING \"{release}\"\n");
    if !contains(header, &versioned) || !contains(header, "#define OPENSSL_IS_AWSLC") {
        return Err(differs());
    }
    let namespace = manifest::string(field(system, "symbol_prefix")?)?;
    let spaced = format!("#define BORINGSSL_PREFIX {namespace}\n");
    if !contains(
        content("include/openssl/boringssl_prefix_symbols.h"),
        &spaced,
    ) {
        return Err(problem("AWS-LC installed symbol namespace differs"));
    }
    verify_options(content("share/simplestchat/CMakeCache.txt"))?;
    let bindings = content(BINDINGS);
    let bound = match generated {
        [one] => tree::read_regular(one, bounds.source as u64)? == bindings,
        _ => false,
    };
    let functions = ["EVP_Digest", "OpenSSL_version"];
    let spoken = |function: &&str| contains(bindings, &format!("{namespace}_{function}"));
    if !bound || !functions.iter().all(spoken) {
        return Err(problem(
            "Cargo AWS-LC bindings differ from the installed namespace",
        ));
    }
    let mut linked = Vec::new();
    for archive in archives {
        archive.fields().ok_or_else(object)?;
        if archive.get("provider").and_then(Document::text) == Some("aws-lc-sys") {
            linked.push(archive);
        }
    }
    let differs = || problem("Cargo AWS-LC archive differs from the installed static library");
    let [library] = linked[..] else {
        return Err(differs());
    };
    let installed = field(library, "library")?.text() == Some("crypto-awslc")
        && field(library, "sha256")?.text() == hash(LIBRARY).text()
        && content(LIBRARY).starts_with(b"!<arch>\n");
    if !installed {
        return Err(differs());
    }
    Ok(Document::record([
        ("native_source", field(system, "source")?.clone()),
        ("native_verified_files", files.len().into()),
        ("native_license_sha256", license.clone()),
        ("bindgen_source", field(system, "bindgen_source")?.clone()),
        ("build", receipt.clone()),
        ("bindings_sha256", hash(BINDINGS).clone()),
    ]))
}

#[cfg(test)]
mod tests;
