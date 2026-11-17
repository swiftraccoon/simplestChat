//! The Rust extractor's dependency build, reused across runs without ever
//! reusing an analysis or a policy verdict. Its key binds the platform, the
//! runner image, the trust of the run, the fixed source path, the exact
//! compilers and every tracked build input; its receipt binds only that
//! key. A restored build must be a private directory of the caller's that
//! holds the receipt and the build and nothing else, within bounds, and
//! Cargo still checks its fingerprints against the current sources.

use super::alike;
use crate::{
    context::{self, Failure, Outcome},
    document::Document,
    files, paths, require,
};
use rustix::fs::{AtFlags, CWD, Mode, OFlags, Timespec, Timestamps};
use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    fs::{DirBuilder, File},
    io::{Read as _, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

/// The most bytes a reused build may hold, a file linked twice counted once.
pub const MAX_BYTES: u64 = 1024 * 1024 * 1024;
/// The most entries a reused build may hold.
pub const MAX_ENTRIES: usize = 100_000;
/// The time every vendored input is stamped with.
pub const DEPENDENCY_MTIME: i64 = 946_684_800;
/// The most bytes of a receipt.
const MAX_RECEIPT: u64 = 4096;
/// The most bytes of one hashed input.
const MAX_INPUT: u64 = 1024 * 1024 * 1024;
/// The manifests of a Rust build, by their file names.
const CODEQL_RUST_MANIFESTS: [&str; 5] = [
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain",
    "rust-toolchain.toml",
    "rust-project.json",
];
/// The tracked files of the native analysis, which every CodeQL key binds.
const CODEQL_NATIVE_FILES: [&str; 23] = [
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
    "build/security_codeql_rust_sources.py",
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
];
/// The modes a tracked input may have.
const TRACKED_MODES: [&str; 2] = ["100644", "100755"];
/// A listing or an identity that is no text, or not of its fields.
const UNREAD: Failure = Failure::Fixed("codeql_cargo_unread");

/// What a key is taken from besides the sources: the run's variables,
/// this host's system and machine as its kernel names them, and the search
/// path that the compilers and `git` are found on. Their commands keep the
/// caller's own environment.
#[derive(Clone, Copy)]
pub struct Host<'a> {
    /// The caller's environment variable `name`, where it is set.
    pub variable: &'a dyn Fn(&str) -> Option<OsString>,
    /// The kernel's name for this system.
    pub system: &'a str,
    /// The kernel's name for this machine.
    pub machine: &'a str,
    /// The caller's search path.
    pub search: Option<&'a OsStr>,
}

impl Host<'_> {
    /// Whether the variable `name` is set to `value`.
    fn set(&self, name: &str, value: &str) -> bool {
        (self.variable)(name).is_some_and(|held| held == value)
    }

    /// The variable `name` as text, nothing for one that is not set.
    fn text(&self, name: &str) -> Option<String> {
        let held = (self.variable)(name).unwrap_or_default();
        held.into_string().ok()
    }

    /// Whether the run is of the main branch and of no pull request.
    fn trusted(&self) -> bool {
        self.set("GITHUB_REF", "refs/heads/main") && !self.set("GITHUB_EVENT_NAME", "pull_request")
    }
}

/// What the command `argv` printed in `root`, in the caller's environment,
/// once it ended with 0 within `within` and the `room` of its two streams,
/// or `refused` for any other status.
fn printed(
    root: &Path,
    argv: &[&OsStr],
    within: Duration,
    room: (u64, u64),
    refused: &'static str,
) -> Outcome<Vec<u8>> {
    let (program, arguments) = argv.split_first().ok_or(Failure::Fixed(refused))?;
    let mut command = Command::new(program);
    command.args(arguments).current_dir(root);
    let (status, printed) = context::asked(&mut command, &[], within, room)?;
    require(status == 0, refused)?;
    Ok(printed)
}

/// What the compiler driver `name` says of its exact version, once the
/// pinned toolchain has had the time to install itself.
pub fn tool_identity(root: &Path, host: &Host<'_>, name: &'static str) -> Outcome<String> {
    let program = context::executable(name, host.search)?;
    let argv = [
        program.as_os_str(),
        "--version".as_ref(),
        "--verbose".as_ref(),
    ];
    let within = Duration::from_secs(180);
    let said = printed(
        root,
        &argv,
        within,
        (65536, 65536),
        "codeql_cargo_toolchain",
    )?;
    String::from_utf8(said).map_err(|_| UNREAD)
}

/// Whether the tracked file `name` is an input of the dependency build:
/// a manifest, a vendored file, Cargo's configuration, a build script, or
/// a file of the native analysis.
pub fn dependency_input(name: &str) -> bool {
    let file = name.rsplit('/').next().unwrap_or(name);
    CODEQL_RUST_MANIFESTS.contains(&file)
        || name.starts_with("vendor/")
        || name.starts_with(".cargo/")
        || name == "build.rs"
        || name.ends_with("/build.rs")
        || CODEQL_NATIVE_FILES.contains(&name)
}

/// The SHA-256 of the regular file `path` of at most a gibibyte, read
/// through no final link.
fn input_digest(path: &Path) -> Outcome<String> {
    use sha2::Digest as _;
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let opened = rustix::fs::open(path, flags, Mode::empty());
    let mut file = opened
        .map(File::from)
        .map_err(|error| files::unusable(&error.into()))?;
    let metadata = file.metadata().map_err(|error| files::unusable(&error))?;
    require(metadata.is_file(), "ci_cache_regular_file")?;
    require(metadata.len() <= MAX_INPUT, "ci_cache_file_size")?;
    let mut hashed = sha2::Sha256::new();
    let mut chunk = vec![0; 1024 * 1024];
    loop {
        let read = file
            .read(&mut chunk)
            .map_err(|error| files::unusable(&error))?;
        if read == 0 {
            break;
        }
        hashed.update(&chunk[..read]);
    }
    Ok(crate::hexadecimal(&hashed.finalize()))
}

/// The runner this build is made on: the exact local image of a local run,
/// or the hosted image's system and version.
fn runner_identity(host: &Host<'_>) -> Outcome<String> {
    if host.set("ACT", "true") {
        let image = host.text("LOCAL_CI_RUNNER_IMAGE").unwrap_or_default();
        let named = |byte: u8| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"./_-".contains(&byte)
        };
        let pinned = image.split_once('@').is_some_and(|(name, digest)| {
            !name.is_empty()
                && name.bytes().all(named)
                && digest.strip_prefix("sha256:").is_some_and(hex)
        });
        require(pinned, "ci_cache_local_image")?;
        return Ok(format!("local:{image}"));
    }
    let plain = |text: &Option<String>| {
        text.as_deref().is_some_and(|text| {
            let allowed = |byte: u8| byte.is_ascii_alphanumeric() || b"._-".contains(&byte);
            (1..=100).contains(&text.len()) && text.bytes().all(allowed)
        })
    };
    let (system, version) = (host.text("ImageOS"), host.text("ImageVersion"));
    require(plain(&system) && plain(&version), "ci_cache_hosted_image")?;
    let (system, version) = (system.unwrap_or_default(), version.unwrap_or_default());
    Ok(format!("hosted:{system}:{version}"))
}

/// Whether `text` is sixty-four small hexadecimal digits.
fn hex(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The namespace of a key: the run's trust, the system and the machine.
fn cache_namespace(host: &Host<'_>) -> Outcome<String> {
    let trust = if host.trusted() { "main" } else { "untrusted" };
    let (system, machine) = (host.system.to_lowercase(), host.machine.to_lowercase());
    let named = |text: &str| {
        let allowed = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_';
        (1..=32).contains(&text.len()) && text.bytes().all(allowed)
    };
    require(named(&system) && named(&machine), "ci_cache_namespace")?;
    Ok(format!("{trust}-{system}-{machine}"))
}

/// The key of the dependency build of the checkout `root` for the fixed
/// source path `source`: the namespace, and the SHA-256 of the build's
/// identity and of every tracked input's name, mode and bytes.
pub fn cache_key(root: &Path, source: &Path, host: &Host<'_>) -> Outcome<String> {
    use sha2::Digest as _;
    require(paths::canonical(source), "codeql_cargo_source_path")?;
    let spelled = paths::path(source.as_os_str());
    let source_root = spelled.to_str().ok_or(UNREAD)?;
    let runner = runner_identity(host)?;
    let identity = Document::record([
        ("runner", runner.into()),
        ("system", host.system.into()),
        ("architecture", host.machine.into()),
        (
            "trust",
            (if host.trusted() { "main" } else { "untrusted" }).into(),
        ),
        ("sourceRoot", source_root.into()),
        ("rustc", tool_identity(root, host, "rustc")?.into()),
        ("cargo", tool_identity(root, host, "cargo")?.into()),
    ]);
    let mut hashed = sha2::Sha256::new();
    hashed.update(identity.sorted().spaced().as_bytes());
    let git = context::executable("git", host.search)?;
    let checkout = paths::path(root.as_os_str());
    let argv = [
        git.as_os_str(),
        "-C".as_ref(),
        checkout.as_os_str(),
        "ls-files".as_ref(),
        "--stage".as_ref(),
        "-z".as_ref(),
    ];
    let room = (4 * 1024 * 1024, 65536);
    let within = Duration::from_secs(30);
    let listing = printed(root, &argv, within, room, "codeql_cache_environment")?;
    let mut count = 0_u64;
    for entry in listing.split(|byte| *byte == 0) {
        if entry.is_empty() {
            continue;
        }
        let tab = entry.iter().position(|byte| *byte == b'\t').ok_or(UNREAD)?;
        let (metadata, raw_name) = (&entry[..tab], &entry[tab + 1..]);
        let metadata = std::str::from_utf8(metadata).map_err(|_| UNREAD)?;
        let [mode, _, stage] = super::words(metadata)[..] else {
            return Err(UNREAD);
        };
        let tracked = stage == "0" && TRACKED_MODES.contains(&mode);
        require(tracked, "codeql_cargo_tracked_type")?;
        let name = std::str::from_utf8(raw_name).map_err(|_| UNREAD)?;
        if !dependency_input(name) {
            continue;
        }
        // The reference's paths drop a final separator and current-directory
        // steps, so the file is named as it is spelled.
        let path = paths::path(root.join(name).as_os_str());
        require(
            paths::canonical(&path) && path.is_file(),
            "codeql_cargo_source_missing",
        )?;
        hashed.update(raw_name);
        hashed.update(b"\0");
        hashed.update(mode.as_bytes());
        hashed.update(b"\0");
        hashed.update(input_digest(&path)?.as_bytes());
        hashed.update(b"\0");
        count += 1;
    }
    require(count > 0, "codeql_cargo_empty_inputs")?;
    let namespace = cache_namespace(host)?;
    let digest = crate::hexadecimal(&hashed.finalize());
    Ok(format!("codeql-rust-cargo-v1-{namespace}-{digest}"))
}

/// Whether `metadata` is of the caller's own.
fn mine(metadata: &std::fs::Metadata) -> bool {
    metadata.uid() == rustix::process::getuid().as_raw()
}

/// Require that `path` is named as it resolves and is a real directory of
/// the caller's that nobody else may enter.
pub fn private_directory(path: &Path) -> Outcome<()> {
    require(paths::canonical(path), "codeql_cargo_directory_path")?;
    let metadata = path
        .symlink_metadata()
        .map_err(|error| files::unusable(&error))?;
    let private = metadata.is_dir() && mine(&metadata) && metadata.mode() & 0o077 == 0;
    Ok(require(private, "codeql_cargo_directory_permissions")?)
}

/// The receipt that binds a build of the source path `source` to `key`.
fn receipt(source: &Path, key: &str) -> Outcome<Document> {
    let spelled = paths::path(source.as_os_str());
    let source_root = spelled.to_str().ok_or(UNREAD)?;
    Ok(Document::record([
        ("schemaVersion", 1_u64.into()),
        ("key", key.into()),
        ("sourceRoot", source_root.into()),
    ]))
}

/// The build below the existing restored directory `directory`, once that
/// holds exactly its receipt and the build, the receipt binds `key` and the
/// source path `source`, and both directories are private.
pub fn validate(source: &Path, directory: &Path, key: &str) -> Outcome<PathBuf> {
    let expected = receipt(source, key)?;
    private_directory(directory)?;
    let mut names = BTreeSet::new();
    let listed = directory
        .read_dir()
        .map_err(|error| files::unusable(&error))?;
    for entry in listed {
        names.insert(entry.map_err(|error| files::unusable(&error))?.file_name());
    }
    let wanted: BTreeSet<OsString> = ["receipt.json", "target"].map(OsString::from).into();
    require(names == wanted, "codeql_cargo_inventory")?;
    let held = files::bounded(&directory.join("receipt.json"), MAX_RECEIPT, true)?;
    let held = Document::read(super::unmarked(&held)).map_err(|_| UNREAD)?;
    held.fields().ok_or(UNREAD)?;
    require(alike(&held, &expected), "codeql_cargo_receipt")?;
    let target = directory.join("target");
    private_directory(&target)?;
    Ok(target)
}

/// Every entry below `directory`, as the reference's recursive listing
/// found them: a directory that cannot be listed is passed over, and no
/// link is followed below it.
fn below(directory: &Path, visit: &mut dyn FnMut(&Path) -> Outcome<()>) -> Outcome<()> {
    let mut pending = vec![directory.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(entries) = next.read_dir() else {
            continue;
        };
        for entry in entries {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            visit(&path)?;
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                pending.push(path);
            }
        }
    }
    Ok(())
}

/// The bytes the build `target` holds, a file linked twice counted once as
/// the cache's archive counts it, once every entry is a directory or a
/// regular file of the caller's that nobody else may write.
pub fn payload_bytes(target: &Path) -> Outcome<u64> {
    let (mut total, mut count) = (0_u64, 0_usize);
    let mut seen = BTreeSet::new();
    below(target, &mut |path| {
        count += 1;
        require(count <= MAX_ENTRIES, "codeql_cargo_entries")?;
        let metadata = path
            .symlink_metadata()
            .map_err(|error| files::unusable(&error))?;
        let kind = metadata.is_dir() || metadata.is_file();
        let owned = kind && mine(&metadata) && metadata.mode() & 0o022 == 0;
        require(owned, "codeql_cargo_entry_type")?;
        if metadata.is_file() && seen.insert((metadata.dev(), metadata.ino())) {
            total += metadata.len();
        }
        Ok(())
    })?;
    Ok(total)
}

/// Stamp every vendored input of the source tree `source` and its pinned
/// interpreter constraints with one fixed time, so that Cargo keeps the
/// fingerprints of identical dependencies; nothing else is touched.
pub fn stabilize_dependencies(source: &Path) -> Outcome<()> {
    require(
        source.is_dir() && paths::canonical(source),
        "codeql_cargo_source_path",
    )?;
    let vendor = source.join("vendor");
    let mut stamped = Vec::new();
    if vendor.exists() {
        stamped.push(vendor.clone());
        below(&vendor, &mut |path| {
            stamped.push(path.to_path_buf());
            Ok(())
        })?;
    }
    let constraints = source.join("build/pip-constraints.txt");
    if constraints.exists() {
        stamped.push(constraints);
    }
    let fixed = Timespec {
        tv_sec: DEPENDENCY_MTIME,
        tv_nsec: 0,
    };
    let times = Timestamps {
        last_access: fixed,
        last_modification: fixed,
    };
    for path in stamped {
        require(!path.is_symlink(), "codeql_cargo_dependency_type")?;
        let set = rustix::fs::utimensat(CWD, &path, &times, AtFlags::SYMLINK_NOFOLLOW);
        set.map_err(|error| files::unusable(&error.into()))?;
    }
    Ok(())
}

/// The build below the restored or new directory `directory` for the
/// source path `source` of the checkout `root`: a new one with its receipt
/// when nothing is there, and in either case the receipt and the build
/// held to the current key and bounds. Only the build is reused: the
/// receipt names no analysis and no verdict.
pub fn prepare(root: &Path, source: &Path, directory: &Path, host: &Host<'_>) -> Outcome<PathBuf> {
    require(paths::canonical(directory), "codeql_cargo_path")?;
    let key = cache_key(root, source, host)?;
    if !directory.exists() {
        if let Some(parent) = directory.parent() {
            let made = DirBuilder::new().recursive(true).mode(0o777).create(parent);
            made.map_err(|error| files::unusable(&error))?;
        }
        for path in [directory.to_path_buf(), directory.join("target")] {
            let made = DirBuilder::new().mode(0o700).create(&path);
            made.map_err(|error| files::unusable(&error))?;
        }
        let written = format!("{}\n", receipt(source, &key)?.spaced());
        files::write_private(&directory.join("receipt.json"), written.as_bytes(), 0o600)?;
    }
    let target = validate(source, directory, &key)?;
    require(payload_bytes(&target)? <= MAX_BYTES, "codeql_cargo_size")?;
    stabilize_dependencies(source)?;
    Ok(target)
}

/// Whether `key` is spelled as a key of the dependency build is.
fn well_formed(key: &str) -> bool {
    let named = |text: &str| {
        !text.is_empty()
            && text
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    };
    let Some(rest) = key.strip_prefix("codeql-rust-cargo-v1-") else {
        return false;
    };
    let rest = rest
        .strip_prefix("main-")
        .or_else(|| rest.strip_prefix("untrusted-"));
    let Some((namespace, digest)) = rest.and_then(|rest| rest.rsplit_once('-')) else {
        return false;
    };
    let Some((system, machine)) = namespace.split_once('-') else {
        return false;
    };
    named(system) && named(machine) && hex(digest)
}

/// What `codeql cargo-key` asks for.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Asked {
    /// The fixed source path of the extraction.
    pub source_root: PathBuf,
    /// A restored build to judge for publication instead of printing the key.
    pub publication_check: Option<PathBuf>,
}

/// What `codeql cargo-key` prints: the key, or whether the build at the
/// publication check may be kept, with its bytes and the bound.
pub fn key_report(root: &Path, asked: &Asked, host: &Host<'_>) -> Outcome<String> {
    let key = cache_key(root, &asked.source_root, host)?;
    require(well_formed(&key), "codeql_cargo_key")?;
    let Some(directory) = &asked.publication_check else {
        return Ok(format!("{key}\n"));
    };
    let target = validate(&asked.source_root, directory, &key)?;
    let size = payload_bytes(&target)?;
    let cacheable = size <= MAX_BYTES;
    Ok(format!(
        "cacheable={cacheable}\nbytes={size}\nlimit-bytes={MAX_BYTES}\n"
    ))
}

/// The exit status of `codeql cargo-key`: its report on `out`, or one
/// fixed line on `err`.
pub fn command(
    root: &Path,
    asked: &Asked,
    host: &Host<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    match key_report(root, asked, host) {
        Ok(report) => u8::from(out.write_all(report.as_bytes()).is_err()),
        Err(_) => {
            let _ = err.write_all(b"Rust extraction Cargo cache validation failed.\n");
            1
        }
    }
}

#[cfg(test)]
#[path = "cargo_tests.rs"]
mod tests;
