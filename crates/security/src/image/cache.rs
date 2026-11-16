//! The cache of the vulnerability scanner's database: the bytes that the
//! scanner's own update fetched and checked, kept between runs only to save
//! their download. The update, the freshness check and every scan still run;
//! a cache never supplies a verdict. Its keys separate the scanner's pins and
//! the trust of the run, and name each generation of the database by the
//! checksum of its archive.

use super::policy::decoded;
use crate::{
    Fault, Result, digest,
    document::Document,
    files, require,
    tools::transport::{Answer, Curl, Transport},
};
use std::{
    fs::File,
    io::{ErrorKind, Read, Write},
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
};

/// The descriptor of the newest generation, as the scanner's site publishes it.
pub const LATEST: &str = "https://grype.anchore.io/databases/v6/latest.json";
/// The files of a cached database, in the order they are copied.
const FILES: [&str; 2] = ["vulnerability.db", "import.json"];
/// A descriptor or a source that is no generation of the sixth schema.
const SOURCE: Fault = Fault("image_database_generation_source");
/// A cached file that cannot be read or copied, or a receipt that is no
/// JSON record.
const UNREAD: Fault = Fault("image_database_cache_unread");

/// The bounds of a cached database.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// The most bytes of the database: the scanner's own bound of four
    /// gibibytes, less room for the import record.
    pub database: u64,
    /// The most bytes of the import record.
    pub import: u64,
    /// The most bytes of a receipt.
    pub receipt: u64,
    /// The most bytes of a descriptor.
    pub descriptor: u64,
}

impl Limits {
    /// The reviewed bounds.
    pub const REVIEWED: Self = Self {
        database: 4 * 1024 * 1024 * 1024 - 65_536,
        import: 65_536,
        receipt: 65_536,
        descriptor: 65_536,
    };

    fn of(&self, name: &str) -> u64 {
        if name == "vulnerability.db" {
            self.database
        } else {
            self.import
        }
    }
}

/// What separates the downloads of one pinned scanner from another's: the
/// digest of its record in the tool lock, written as the reference wrote it.
pub fn pin(grype: &Document) -> String {
    digest(grype.clone().sorted().spaced().as_bytes())
}

/// Whether a run may publish to the trusted cache: a push to the main branch.
pub fn trusted(reference: Option<&str>, event: Option<&str>) -> bool {
    reference == Some("refs/heads/main") && event != Some("pull_request")
}

/// The prefix of every key of the scanner pinned as `pin`, by the trust of
/// the run.
pub fn prefix(trusted: bool, pin: &str) -> String {
    let trust = if trusted { "main" } else { "untrusted" };
    format!("grype-db-v2-{trust}-{pin}-")
}

/// The key of the generation whose archive has the checksum `checksum`.
fn generation(checksum: Option<&Document>, prefix: &str) -> Result<String> {
    let hexadecimal = checksum
        .and_then(Document::text)
        .and_then(|checksum| checksum.strip_prefix("sha256:"))
        .filter(|hexadecimal| super::small_hex(hexadecimal, 64));
    let hexadecimal = hexadecimal.ok_or(Fault("image_database_generation_checksum"))?;
    Ok(format!("{prefix}{hexadecimal}"))
}

/// The key of the generation that the site's descriptor `value` names: an
/// active database of the sixth schema. Its status is only a hint.
pub fn descriptor_key(value: &Document, prefix: &str) -> Result<String> {
    let numbered =
        |part: &str| !part.is_empty() && part.bytes().all(|digit| digit.is_ascii_digit());
    let schema = value.get("schemaVersion").and_then(Document::text);
    let schema = schema.and_then(|schema| schema.strip_prefix("v6."));
    let schema = schema.and_then(|schema| schema.split_once('.'));
    let sixth = schema.is_some_and(|(minor, patch)| numbered(minor) && numbered(patch));
    let active = value.get("status") == Some(&Document::from("active"));
    require(active && sixth, "image_database_generation_descriptor")?;
    generation(value.get("checksum"), prefix)
}

/// The fields of a query as the reference read it strictly: every field
/// holds a sign, a blank value is kept, and both sides are decoded.
fn fields(query: &str) -> Result<Vec<(String, String)>> {
    let mut fields = Vec::new();
    for field in query.split('&') {
        let (name, value) = field.split_once('=').ok_or(SOURCE)?;
        fields.push((decoded(name), decoded(value)));
    }
    Ok(fields)
}

/// The key of the generation that the scanner's own update fetched, from
/// the status it reports: an archive of the sixth schema from the
/// scanner's site, with its one checksum.
pub fn key_from_status(status: &Document, prefix: &str) -> Result<String> {
    let source = status.get("from").and_then(Document::text).ok_or(SOURCE)?;
    // The address is read as it is written, as the database's status is:
    // the reference's URL library dropped tabs, returns and line ends,
    // took a scheme in any case and an empty fragment.
    let plain = !source.contains(['\t', '\r', '\n', '#']);
    let rest = source.strip_prefix("https://grype.anchore.io/databases/v6/");
    let rest = rest.filter(|_| plain).ok_or(SOURCE)?;
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    require(path.ends_with(".tar.zst"), SOURCE.0)?;
    // One field, of the checksum: neither twice nor beside another.
    let fields = fields(query)?;
    let [(name, checksum)] = fields.as_slice() else {
        return Err(SOURCE);
    };
    require(name == "checksum", SOURCE.0)?;
    generation(Some(&checksum.as_str().into()), prefix)
}

/// Copy the regular file `source` and nothing else, no link and no more
/// than `limit` bytes, to the new file `destination`, which only its owner
/// may read: the size and digest of exactly the bytes copied.
fn copy_checked(source: &Path, destination: &Path, limit: u64) -> Result<Document> {
    use rustix::fs::{Mode, OFlags};
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let incoming = rustix::fs::open(source, flags, Mode::empty()).map_err(|_| UNREAD)?;
    let mut incoming = File::from(incoming);
    let entry = incoming.metadata().map_err(|_| UNREAD)?;
    require(!entry.is_dir(), UNREAD.0)?;
    let mut outgoing = File::options()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|_| UNREAD)?;
    require(entry.is_file(), "image_database_cache_file")?;
    let size = entry.len();
    require(0 < size && size <= limit, "image_database_cache_size")?;
    let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
    let mut buffer = vec![0; 1024 * 1024];
    let mut copied: u64 = 0;
    loop {
        let read = incoming.read(&mut buffer).map_err(|_| UNREAD)?;
        if read == 0 {
            break;
        }
        copied += read as u64;
        require(copied <= limit, "image_database_cache_size")?;
        sha2::Digest::update(&mut hasher, &buffer[..read]);
        outgoing.write_all(&buffer[..read]).map_err(|_| UNREAD)?;
    }
    require(copied == size, "image_database_cache_changed")?;
    std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o600))
        .map_err(|_| UNREAD)?;
    let hexadecimal = crate::hexadecimal(&sha2::Digest::finalize(hasher));
    Ok(Document::record([
        ("bytes", copied.into()),
        ("sha256", hexadecimal.into()),
    ]))
}

/// A directory of the run's own below a parent, removed with what it holds
/// when it is dropped.
struct Staged(PathBuf);

impl Staged {
    fn new(parent: &Path, prefix: &str) -> Result<Self> {
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        for attempt in 0..1000 {
            let path = parent.join(format!("{prefix}{}-{attempt}", std::process::id()));
            match builder.create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(_) => return Err(UNREAD),
            }
        }
        Err(UNREAD)
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Make the directory `path`, which only its owner may use.
fn private(path: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.mode(0o700);
    builder.create(path).map_err(|_| UNREAD)
}

/// Seed the scanner's own update in `output` from the cache at `directory`,
/// when there is one: its receipt must be of the scanner pinned as `pin`,
/// and every file the bytes it names. The files are handed to `owner` when
/// one is given. Answers whether there was a cache.
pub fn restore(
    directory: &Path,
    output: &Path,
    owner: Option<(u32, u32)>,
    pin: &str,
    limits: &Limits,
) -> Result<bool> {
    require(directory.is_absolute(), "image_database_cache_path")?;
    if directory.symlink_metadata().is_err() {
        return Ok(false);
    }
    files::private_directory(directory)?;
    let receipt = {
        let staged = Staged::new(output, "database-receipt-")?;
        let copy = staged.0.join("receipt.json");
        copy_checked(&directory.join("receipt.json"), &copy, limits.receipt)?;
        let content = std::fs::read(&copy).map_err(|_| UNREAD)?;
        let receipt = Document::read(&content).ok();
        receipt
            .filter(|receipt| receipt.fields().is_some())
            .ok_or(UNREAD)?
    };
    let fields = std::collections::BTreeSet::from(["schemaVersion", "scannerPin", "files"]);
    let current = receipt.names() == fields
        && receipt.get("schemaVersion").and_then(Document::whole) == Some(1)
        && receipt.get("scannerPin") == Some(&pin.into());
    require(current, "image_database_cache_receipt")?;
    let identities = receipt
        .get("files")
        .filter(|files| files.fields().is_some())
        .ok_or(UNREAD)?;
    require(
        identities.names() == std::collections::BTreeSet::from(FILES),
        "image_database_cache_inventory",
    )?;
    let cache = output.join("cache");
    private(&cache)?;
    let schema = cache.join("6");
    private(&schema)?;
    let hand = |path: &Path| -> Result<()> {
        match owner {
            Some((uid, gid)) => {
                std::os::unix::fs::chown(path, Some(uid), Some(gid)).map_err(|_| UNREAD)
            }
            None => Ok(()),
        }
    };
    for name in FILES {
        let target = schema.join(name);
        let copied = copy_checked(&directory.join(name), &target, limits.of(name))?;
        let expected = identities.get(name).cloned().unwrap_or(Document::Null);
        require(
            copied.sorted() == expected.sorted(),
            "image_database_cache_digest",
        )?;
        hand(&target)?;
    }
    hand(&cache)?;
    hand(&schema)?;
    Ok(true)
}

/// Publish to the cache at `directory` the database that the scanner's own
/// update just made in `database`, with a receipt of the scanner pinned as
/// `pin` and of every file's bytes; nothing of a verdict.
pub fn save(directory: &Path, database: &Path, pin: &str, limits: &Limits) -> Result<()> {
    require(directory.is_absolute(), "image_database_cache_path")?;
    match private(directory) {
        Ok(()) => {}
        Err(_) if directory.is_dir() => {}
        Err(fault) => return Err(fault),
    }
    files::private_directory(directory)?;
    let staged = Staged::new(directory, "database-publish-")?;
    let mut identities = Vec::new();
    for name in FILES {
        let copied = copy_checked(
            &database.join("cache/6").join(name),
            &staged.0.join(name),
            limits.of(name),
        )?;
        identities.push((name.to_owned(), copied));
    }
    let receipt = Document::record([
        ("schemaVersion", 1_u64.into()),
        ("scannerPin", pin.into()),
        ("files", Document::Record(identities)),
    ]);
    let written = staged.0.join("receipt.json");
    let mut text = receipt.sorted().spaced();
    text.push('\n');
    std::fs::write(&written, text).map_err(|_| UNREAD)?;
    std::fs::set_permissions(&written, std::fs::Permissions::from_mode(0o600))
        .map_err(|_| UNREAD)?;
    for name in FILES.into_iter().chain(["receipt.json"]) {
        std::fs::rename(staged.0.join(name), directory.join(name)).map_err(|_| UNREAD)?;
    }
    Ok(())
}

/// The key of the newest generation, from the descriptor that the site
/// publishes at [`LATEST`]: fetched by `transport` within ten seconds, from
/// that address and no other, and no longer than its bound.
pub fn key(transport: &mut dyn Transport, prefix: &str, limits: &Limits) -> Result<String> {
    let answer = transport.fetch(
        LATEST,
        limits.descriptor,
        std::time::Duration::from_secs(10),
    );
    let bytes = match answer {
        Answer::Body { bytes, address } => {
            require(address == LATEST, "image_database_generation_redirect")?;
            bytes
        }
        Answer::Oversized => return Err(Fault("image_database_generation_size")),
        _ => return Err(Fault("image_database_generation_fetch")),
    };
    let bounded = !bytes.is_empty() && bytes.len() as u64 <= limits.descriptor;
    require(bounded, "image_database_generation_size")?;
    let read = Document::read(&bytes).ok();
    let descriptor = read
        .filter(|value| value.fields().is_some())
        .ok_or(SOURCE)?;
    descriptor_key(&descriptor, prefix)
}

/// The pin of the scanner that the tool lock below `root` names.
fn locked_pin(root: &Path) -> Result<String> {
    let lock = files::bounded(
        &root.join("build/security-tools.lock.json"),
        1024 * 1024,
        false,
    )?;
    let lock = Document::read(&lock).map_err(|_| UNREAD)?;
    let grype = lock.get("tools").and_then(|tools| tools.get("grype"));
    let grype = grype
        .filter(|grype| grype.fields().is_some())
        .ok_or(UNREAD)?;
    Ok(pin(grype))
}

const USAGE: &str = "usage: simplestchat-security image cache [--prefix | --status FILE]";

/// `simplestchat-security image cache`: the key of the newest generation,
/// the prefix of every key of this run, or the key of the generation that
/// the update reported in the status at `FILE`; one line, and nothing of a
/// verdict.
pub fn run(
    root: &Path,
    arguments: &[String],
    ambient: &dyn Fn(&str) -> Option<std::ffi::OsString>,
) -> std::process::ExitCode {
    /// What the command line asks for.
    enum Asked<'a> {
        Latest,
        Prefix,
        Status(&'a str),
    }
    let asked = match arguments {
        [] => Asked::Latest,
        [flag] if flag == "--prefix" => Asked::Prefix,
        [flag, path] if flag == "--status" => Asked::Status(path),
        _ => {
            eprintln!("{USAGE}");
            return std::process::ExitCode::from(2);
        }
    };
    let named = |name: &str| ambient(name).and_then(|value| value.into_string().ok());
    let selected = || -> Result<String> {
        let reference = named("GITHUB_REF");
        let event = named("GITHUB_EVENT_NAME");
        let prefix = prefix(
            trusted(reference.as_deref(), event.as_deref()),
            &locked_pin(root)?,
        );
        match asked {
            Asked::Latest => key(&mut Curl::default(), &prefix, &Limits::REVIEWED),
            Asked::Prefix => Ok(prefix),
            Asked::Status(path) => {
                let status = files::bounded(Path::new(path), Limits::REVIEWED.descriptor, false)?;
                let read = Document::read(&status).ok();
                let status = read
                    .filter(|value| value.fields().is_some())
                    .ok_or(SOURCE)?;
                key_from_status(&status, &prefix)
            }
        }
    };
    match selected() {
        Ok(selected) => {
            println!("{selected}");
            std::process::ExitCode::SUCCESS
        }
        Err(fault) => {
            eprintln!("Image database cache failed: {fault}");
            std::process::ExitCode::from(1)
        }
    }
}

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;
