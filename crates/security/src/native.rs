//! The native evidence of the production build: the reviewed record of
//! every native component is held to the vendored sources, to the
//! installers and to the lock of the checkout before anything that was
//! built is believed.

use crate::{
    awslc, digest,
    document::Document,
    vendor::{
        Bounds, Problem, Verdict,
        manifest::{self, Manifest},
        tree,
    },
};
use std::{collections::BTreeSet, path::Path};

pub mod artifact;
pub mod build;
pub mod licenses;
pub mod links;
pub mod openssl;
pub mod receipt;
pub mod registry;
pub mod toolchain;

/// Where a crate of the lock comes from when it comes from crates.io.
pub(crate) const SOURCE_REGISTRY: &str = "registry+https://github.com/rust-lang/crates.io-index";
/// The reviewed options of the OpenSSL build, in their order.
const OPENSSL_OPTIONS: [&str; 5] = ["no-shared", "no-dso", "no-module", "no-engine", "no-tests"];
/// Where the adapted component is vendored.
const ADAPTED: &str = "vendor/mediasoup-sys-0.19.0/deps/libwebrtc";
/// The crates whose build scripts link native libraries.
const PROVIDERS: [&str; 3] = ["mediasoup-sys", "openssl-sys", "aws-lc-sys"];

pub(crate) fn problem(text: &str) -> Problem {
    Problem(text.to_owned())
}

/// The field `name` of a record, which must have it.
pub(crate) fn field<'a>(record: &'a Document, name: &str) -> Verdict<&'a Document> {
    let held = record.get(name);
    held.ok_or_else(|| Problem(format!("Missing field: {name}")))
}

/// A record.
pub(crate) fn object(value: &Document) -> Verdict<&Document> {
    let held = value.fields().map(|_| value);
    held.ok_or_else(|| problem("Expected a JSON object"))
}

/// The field `name` of a record that was held to its fields.
pub(crate) fn part<'a>(record: &'a Document, name: &str) -> &'a Document {
    record.get(name).unwrap_or(&Document::Null)
}

pub(crate) fn text<'a>(record: &'a Document, name: &str) -> Verdict<&'a str> {
    manifest::string(part(record, name))
}

/// A list of nothing but text.
pub(crate) fn strings(value: &Document) -> Verdict<Vec<&str>> {
    manifest::array(value)?
        .iter()
        .map(manifest::string)
        .collect()
}

/// The whole of a small file of the checkout.
pub(crate) fn read(path: &Path) -> Verdict<Vec<u8>> {
    tree::read_regular(path, Bounds::REVIEWED.record as u64)
}

/// The whole of a small file of the checkout that is text.
pub(crate) fn readable(path: &Path) -> Verdict<String> {
    let text = String::from_utf8(read(path)?);
    text.map_err(|_| Problem(format!("No text: {}", path.display())))
}

fn contains(text: &[u8], words: &str) -> bool {
    let words = words.as_bytes();
    text.windows(words.len()).any(|window| window == words)
}

/// A whole revision: forty lowercase hexadecimal digits.
pub(crate) fn revision(text: &str) -> bool {
    let digit = |byte: u8| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte);
    text.len() == 40 && text.bytes().all(digit)
}

/// The reviewed licence of a component: an expression of at most 4096
/// letters, digits, dots, plus signs, brackets, hyphens and spaces, with no
/// space at either end.
pub(crate) fn reviewed_license(component: &Document) -> Verdict<&str> {
    let expression = text(component, "license")?;
    let plain = |byte: u8| byte.is_ascii_alphanumeric() || b".+() -".contains(&byte);
    let sized = (1..=4096).contains(&expression.len());
    if !sized || !expression.bytes().all(plain) || expression.trim_matches(' ') != expression {
        return Err(problem(
            "Missing or invalid reviewed native license expression",
        ));
    }
    Ok(expression)
}

/// A value of a TOML file as a value of a document. A date is none.
fn converted(value: toml::Value) -> Verdict<Document> {
    let unsupported = || problem("Unsupported JSON value");
    Ok(match value {
        toml::Value::String(text) => Document::Text(text),
        toml::Value::Integer(number) => Document::Number(number.into()),
        toml::Value::Float(number) => {
            let number = serde_json::Number::from_f64(number);
            Document::Number(number.ok_or_else(unsupported)?)
        }
        toml::Value::Boolean(held) => Document::Bool(held),
        toml::Value::Datetime(_) => return Err(unsupported()),
        toml::Value::Array(items) => {
            let items: Verdict<Vec<Document>> = items.into_iter().map(converted).collect();
            Document::List(items?)
        }
        toml::Value::Table(fields) => {
            let field = |(name, value): (String, toml::Value)| Ok((name, converted(value)?));
            let fields: Verdict<Vec<(String, Document)>> = fields.into_iter().map(field).collect();
            Document::Record(fields?)
        }
    })
}

/// The bytes `data` of the TOML file called `name` as a document.
pub(crate) fn table(data: &[u8], name: &str) -> Verdict<Document> {
    let text = std::str::from_utf8(data);
    let text = text.map_err(|_| Problem(format!("No text: {name}")))?;
    let parsed = toml::from_str::<toml::Table>(text);
    let parsed = parsed.map_err(|_| Problem(format!("Invalid TOML: {name}")))?;
    converted(toml::Value::Table(parsed))
}

/// A small TOML file of the checkout as a document.
pub(crate) fn toml(path: &Path) -> Verdict<Document> {
    table(&read(path)?, &path.display().to_string())
}

/// The paths of one production build and of the evidence of its sources.
#[derive(Clone, Copy, Debug)]
pub struct Build<'a> {
    /// The checkout, as it really is.
    pub root: &'a Path,
    /// The receipt of the vendor verification.
    pub vendor_report: &'a Path,
    /// Cargo's messages of the build.
    pub cargo_messages: &'a Path,
    /// Cargo's home, as it really is.
    pub cargo_home: &'a Path,
    /// Where OpenSSL was installed.
    pub openssl_prefix: &'a Path,
    /// Where AWS-LC was installed.
    pub aws_lc_prefix: &'a Path,
    /// The new file of the evidence.
    pub output: &'a Path,
}

/// Hold the adapted component to its place and to the notes that the
/// vendored source keeps of its upstream branch and commit.
fn validate_adapted(root: &Path, value: &Document) -> Verdict<()> {
    let names = ["name", "version", "revision", "path", "license"];
    let component = manifest::fields(value, &names)?;
    let path = manifest::relative_path(text(component, "path")?)?;
    if path != ADAPTED || text(component, "name")? != "libwebrtc" {
        return Err(problem("Unsupported adapted native component"));
    }
    let notes = readable(&root.join(path).join("README.md"))?;
    let differs = || problem("Adapted native source identity differs");
    let commit = text(component, "revision")?;
    if !revision(commit) {
        return Err(differs());
    }
    let branch = format!("- libwebrtc branch: {}\n", text(component, "version")?);
    if !notes.contains(&branch) || !notes.contains(&format!("- libwebrtc commit: {commit}\n")) {
        return Err(differs());
    }
    Ok(())
}

/// Hold the record of OpenSSL to the installer that checks its digest.
fn validate_openssl(root: &Path, value: &Document) -> Verdict<()> {
    let names = [
        "version",
        "source",
        "installer_sha256",
        "license",
        "configure_options",
    ];
    let component = manifest::fields(value, &names)?;
    let source = manifest::source(part(component, "source"))?;
    let version = text(component, "version")?;
    let installer = read(&root.join("build/install-openssl.sh"))?;
    let differs = || problem("OpenSSL source or installer identity differs");
    if digest(&installer) != manifest::digest(part(component, "installer_sha256"))? {
        return Err(differs());
    }
    let release = format!("openssl-{version}");
    let releases = "https://github.com/openssl/openssl/releases/download";
    let released = format!("{releases}/{release}/{release}.tar.gz");
    let pinned = source.identifier == release
        && source.format == "tar.gz"
        && source.url == released
        && contains(&installer, &format!("openssl_version='{version}'\n"))
        && contains(&installer, &format!("openssl_sha256='{}'\n", source.sha256));
    if !pinned || strings(part(component, "configure_options"))? != OPENSSL_OPTIONS {
        return Err(differs());
    }
    Ok(())
}

/// Hold the unchanged wrapper crate to its one entry of the lock.
fn validate_registry_lock(root: &Path, value: &Document) -> Verdict<()> {
    let names = [
        "name",
        "version",
        "revision",
        "crate",
        "crate_version",
        "source",
        "license",
        "system",
    ];
    let component = manifest::fields(value, &names)?;
    let source = manifest::source(part(component, "source"))?;
    let (name, version) = (text(component, "crate")?, text(component, "crate_version")?);
    let lock = toml(&root.join("Cargo.lock"))?;
    let packages = lock.get("package");
    let packages = packages.ok_or_else(|| problem("Missing field: package"))?;
    let mut matches = Vec::new();
    for package in manifest::array(packages)? {
        let fields = package.fields();
        fields.ok_or_else(|| problem("Expected a JSON object"))?;
        let held = |field: &str| package.get(field).and_then(Document::text);
        if held("name") == Some(name) && held("version") == Some(version) {
            matches.push(package);
        }
    }
    let differs = || problem("Bundled native registry source differs from Cargo.lock");
    if name != "aws-lc-sys" || text(component, "name")? != "AWS-LC" {
        return Err(differs());
    }
    if !revision(text(component, "revision")?) {
        return Err(differs());
    }
    let [locked] = matches[..] else {
        return Err(differs());
    };
    let held = |field: &str| locked.get(field).and_then(Document::text);
    let pinned = held("source") == Some(SOURCE_REGISTRY)
        && held("checksum") == Some(&source.sha256)
        && source.identifier == format!("{name}-{version}")
        && source.url == format!("https://static.crates.io/crates/{name}/{name}-{version}.crate")
        && source.format == "tar.gz";
    if !pinned {
        return Err(differs());
    }
    Ok(())
}

/// Hold one component of a wrap to its classifications and to its source.
fn validate_component(component: &Document) -> Verdict<()> {
    reviewed_license(component)?;
    let classified = ["static", "header-only"].contains(&text(component, "linkage")?)
        && ["production", "native-test", "windows-only"].contains(&text(component, "usage")?);
    if !classified {
        return Err(problem("Unsupported native component classification"));
    }
    let versioned = !text(component, "name")?.is_empty()
        && !text(component, "version")?.is_empty()
        && text(component, "source")?.contains(text(component, "version")?);
    if !versioned {
        return Err(problem(
            "Native component version differs from its pinned source",
        ));
    }
    Ok(())
}

/// Hold the libraries that the build scripts may link to their form: the
/// three providers, each with its static and its system libraries, none of
/// them twice.
fn validate_links(value: &Document) -> Verdict<()> {
    let providers = value.fields();
    let providers = providers.ok_or_else(|| problem("Expected a JSON object"))?;
    let named: BTreeSet<&str> = providers.iter().map(|(name, _)| name.as_str()).collect();
    if named != PROVIDERS.into_iter().collect() {
        return Err(problem("Missing or stale native link provider"));
    }
    for (_, provider) in providers {
        let provider = manifest::fields(provider, &["static", "system"])?;
        let mut libraries = strings(part(provider, "static"))?;
        libraries.extend(strings(part(provider, "system"))?);
        manifest::unique(libraries.iter().copied(), "native library")?;
        let plain = |byte: u8| byte.is_ascii_alphanumeric() || b"_+.-".contains(&byte);
        let named = |name: &&str| (1..=128).contains(&name.len()) && name.bytes().all(plain);
        if !libraries.iter().all(named) {
            return Err(problem("Invalid native library name"));
        }
    }
    Ok(())
}

/// The reviewed record of the native components of the checkout `root`
/// and the record of its vendored sources, with the first held to the
/// second, to the installers and to the lock.
pub fn validate_manifest(root: &Path) -> Verdict<(Document, Manifest)> {
    let names = [
        "wrap_components",
        "adapted_component",
        "openssl",
        "registry_component",
        "native_links",
    ];
    let record = read(&root.join("vendor/native-components.json"))?;
    let record = Document::read(&record).map_err(manifest::unread)?;
    manifest::fields(&record, &names)?;
    let integrity = manifest::parse(&read(&root.join("vendor/integrity.json"))?)?;
    let names = ["source", "name", "version", "linkage", "usage", "license"];
    let mut components = Vec::new();
    for component in manifest::array(part(&record, "wrap_components"))? {
        components.push(manifest::fields(component, &names)?);
    }
    let mut sources = Vec::new();
    for component in &components {
        sources.push(text(component, "source")?);
    }
    manifest::unique(sources.iter().copied(), "native component source")?;
    let wrapped: BTreeSet<&str> = integrity
        .wraps
        .iter()
        .map(|wrap| wrap.source.as_str())
        .collect();
    if sources.into_iter().collect::<BTreeSet<&str>>() != wrapped {
        return Err(problem("Missing or stale native wrap component"));
    }
    for component in components {
        validate_component(component)?;
    }
    let wrapper = part(&record, "registry_component");
    validate_adapted(root, part(&record, "adapted_component"))?;
    validate_openssl(root, part(&record, "openssl"))?;
    validate_registry_lock(root, wrapper)?;
    awslc::validate(root, wrapper)?;
    for name in ["adapted_component", "openssl", "registry_component"] {
        reviewed_license(part(&record, name))?;
    }
    validate_links(part(&record, "native_links"))?;
    Ok((record, integrity))
}

/// Where the static C++ runtime of the host belongs.
const SYSTEM_ROOT: &str = "/usr";
/// The most bytes the report may have.
const MAX_REPORT: usize = 8 * 1024 * 1024;
/// What the evidence does not show, said in the report itself.
const LIMITS: [&str; 6] = [
    "Static archive identity does not prove every member survives final linking.",
    "Header-only and adapted components are authenticated source inputs.",
    "Rust license evidence covers compiler events and the exact embedded dependency graph.",
    "Embedded metadata can include optional packages without compiler-artifact events.",
    "The unpublished first-party application has no inferred distribution license.",
    "Runtime-loaded libraries and deployed protections need separate checks.",
];

/// The whole report of the native evidence of one production build, as it
/// is written.
fn report(build: &Build<'_>, system: &Path, host: &dyn toolchain::Host) -> Verdict<Vec<u8>> {
    let (record, integrity) = validate_manifest(build.root)?;
    let receipt = receipt::validate_vendor_receipt(build.root, build.vendor_report, &integrity)?;
    let messages = tree::read_regular(build.cargo_messages, build::MAX_MESSAGES as u64)?;
    let graph = build::cargo_build(build.root, &messages)?;
    let (binary, embedded) = artifact::binary_dependencies(&graph.binary)?;
    let wrapper = part(&record, "registry_component");
    let registry = registry::registry_component(build, &graph, wrapper)?;
    let licences = licenses::rust_licenses(build, &graph, &integrity, &embedded)?;
    let (archives, runtime) = links::link_archives(build, &graph, &record, system)?;
    let scripts = build.root.join("target/release/build");
    let mut bindings = Vec::new();
    for ((identifier, _), event) in &graph.scripts {
        if build::package_name(identifier)? == "aws-lc-sys" {
            let directory = manifest::string(field(event, "out_dir")?)?;
            bindings.push(build::beneath(Path::new(directory), &scripts)?.join("bindings.rs"));
        }
    }
    let installed = awslc::evidence(build.aws_lc_prefix, wrapper, &archives, &bindings)?;
    let mut registry = registry.fields().unwrap_or_default().to_vec();
    for (name, value) in installed.fields().unwrap_or_default() {
        registry.retain(|(held, _)| held != name);
        registry.push((name.clone(), value.clone()));
    }
    let toolchain = toolchain::toolchain_evidence(build.root, &runtime, host)?;
    let openssl =
        openssl::openssl_build_evidence(build.openssl_prefix, part(&record, "openssl"), &archives)?;
    let own = |name: &str| read(&build.root.join(name)).map(|body| Document::from(digest(&body)));
    let graphed = ["format", "sha256", "compressedSha256", "packageCount"];
    let mut metadata = Vec::new();
    for name in graphed {
        metadata.push((name.to_owned(), field(&embedded, name)?.clone()));
    }
    let source = |source: &manifest::Source| {
        Document::record([
            ("id", source.identifier.as_str().into()),
            ("url", source.url.as_str().into()),
            ("sha256", source.sha256.as_str().into()),
            ("format", source.format.as_str().into()),
        ])
    };
    let change = |change: &manifest::Change| {
        Document::record([
            ("path", change.path.as_str().into()),
            ("upstream_sha256", change.upstream.clone().into()),
            ("vendored_sha256", change.vendored.clone().into()),
        ])
    };
    let patch = |tree: &manifest::Tree| {
        Document::record([
            ("path", tree.path.as_str().into()),
            ("source", tree.source.as_str().into()),
            (
                "changes",
                Document::List(tree.changes.iter().map(change).collect()),
            ),
        ])
    };
    let report = Document::record([
        (
            "native_manifest_sha256",
            own("vendor/native-components.json")?,
        ),
        ("vendor_report_sha256", receipt.into()),
        ("cargo_messages_sha256", digest(&messages).into()),
        ("cargo_lock_sha256", own("Cargo.lock")?),
        ("binary", binary.clone()),
        ("wrap_components", part(&record, "wrap_components").clone()),
        (
            "adapted_component",
            part(&record, "adapted_component").clone(),
        ),
        ("openssl", openssl),
        ("registry_component", Document::Record(registry)),
        ("rust_licenses", Document::List(licences)),
        ("rust_dependency_metadata", Document::Record(metadata)),
        (
            "vendor_sources",
            Document::List(integrity.sources.iter().map(source).collect()),
        ),
        (
            "vendor_patches",
            Document::List(integrity.trees.iter().map(patch).collect()),
        ),
        ("static_archives", Document::List(archives)),
        ("toolchain", toolchain),
        (
            "limits",
            Document::List(LIMITS.into_iter().map(Document::from).collect()),
        ),
    ]);
    if artifact::record_hash(&graph.binary, false)? != binary {
        return Err(problem(
            "Production binary changed before receipt publication",
        ));
    }
    let written = report.sorted().pretty().into_bytes();
    if written.len() > MAX_REPORT {
        return Err(problem("Native provenance report exceeds its byte budget"));
    }
    Ok(written)
}

/// Write the report of the native evidence of the build as its new
/// private file, only after the sources and what was really linked agree.
pub fn produce(build: &Build<'_>, system: &Path, host: &dyn toolchain::Host) -> Verdict<()> {
    let written = report(build, system, host)?;
    let kept = crate::files::write_private(build.output, &written, 0o600);
    kept.map_err(|_| {
        Problem(format!(
            "Report cannot be written: {}",
            build.output.display()
        ))
    })
}

const USAGE: &str = concat!(
    "usage: simplestchat-security native --vendor-report FILE --cargo-messages FILE\n",
    "       --cargo-home DIR --openssl-prefix DIR --aws-lc-prefix DIR --output FILE [--root DIR]",
);

/// The paths that a command line names, in the order of `Build`'s fields.
fn asked(root: &Path, arguments: &[String]) -> Option<[std::path::PathBuf; 7]> {
    let options = [
        "--root",
        "--vendor-report",
        "--cargo-messages",
        "--cargo-home",
        "--openssl-prefix",
        "--aws-lc-prefix",
        "--output",
    ];
    let mut named: [Option<std::path::PathBuf>; 7] = Default::default();
    let mut rest = arguments.iter();
    while let Some(option) = rest.next() {
        let value = rest.next();
        let value = value.filter(|value| !value.is_empty() && !value.starts_with('-'))?;
        let at = options.iter().position(|held| held == option)?;
        if named[at].replace(value.into()).is_some() {
            return None;
        }
    }
    named[0].get_or_insert_with(|| root.to_path_buf());
    let mut paths = named.into_iter();
    let mut next = || paths.next().flatten();
    Some([
        next()?,
        next()?,
        next()?,
        next()?,
        next()?,
        next()?,
        next()?,
    ])
}

/// Run `native` for the checkout `root`: status 0 when the report was
/// written, and otherwise one line on standard error and status 1.
pub fn run(root: &Path, arguments: &[String]) -> std::process::ExitCode {
    let Some(
        [
            root,
            vendor_report,
            cargo_messages,
            cargo_home,
            openssl,
            aws_lc,
            output,
        ],
    ) = asked(root, arguments)
    else {
        eprintln!("{USAGE}");
        return std::process::ExitCode::from(2);
    };
    let real = |path: std::path::PathBuf| build::resolved(&path);
    let (root, cargo_home) = (real(root), real(cargo_home));
    let (openssl, aws_lc) = (real(openssl), real(aws_lc));
    let build = Build {
        root: &root,
        vendor_report: &vendor_report,
        cargo_messages: &cargo_messages,
        cargo_home: &cargo_home,
        openssl_prefix: &openssl,
        aws_lc_prefix: &aws_lc,
        output: &output,
    };
    match produce(&build, Path::new(SYSTEM_ROOT), &toolchain::Machine) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(Problem(complaint)) => {
            eprintln!("Native provenance failed: {complaint}");
            std::process::ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests;
