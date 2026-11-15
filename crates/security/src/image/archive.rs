//! The saved image as evidence: every regular file of every layer is kept
//! for the secret scan, files that a later layer removes included, and the
//! final file system is built beside them. Nothing the archive says of
//! owners, permissions or devices reaches the host, and no link of the
//! archive is ever followed.

use crate::{Fault, Result, document::Document, hexadecimal, require};
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeMap,
    fs::{DirBuilder, File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

/// The bounds of an extraction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The most bytes the saved image may have.
    pub archive: u64,
    /// The most bytes one layer may unpack to.
    pub layer: u64,
    /// The most bytes every layer together may unpack to.
    pub expanded: u64,
    /// The most bytes one file of a layer may have.
    pub file: u64,
    /// The most members a layer, and every layer together, may have.
    pub members: usize,
    /// The most layers.
    pub layers: usize,
    /// The most characters a path or a link's target may have.
    pub path: usize,
    /// The most bytes one header of names or attributes may have.
    pub metadata: u64,
    /// The most bytes such headers may have together.
    pub metadata_total: u64,
    /// The most hard links in a chain.
    pub links: usize,
    /// The most members the saved image itself may have.
    pub outer: usize,
}

impl Limits {
    /// The reviewed bounds.
    pub const REVIEWED: Self = Self {
        archive: 8 * 1024 * 1024 * 1024,
        layer: 2 * 1024 * 1024 * 1024,
        expanded: 4 * 1024 * 1024 * 1024,
        file: 256 * 1024 * 1024,
        members: 200_000,
        layers: 128,
        path: 4096,
        metadata: 16384,
        metadata_total: 8 * 1024 * 1024,
        links: 40,
        outer: 100_000,
    };
}

const BLOCK: usize = 512;
const WHITEOUT: &str = ".wh.";
const OPAQUE: &str = ".wh..wh..opq";
/// What the host refuses: a name it cannot hold, a full disk.
const HOST: Fault = Fault("image_filesystem_error");

/// A path of the image as its tar members spell it: one leading `./` is
/// dropped, and a directory's slashes at its end; what is left has no
/// empty step, no step that is a dot or two, no backslash and no control
/// character, and does not begin at the root.
pub fn canonical<'a>(name: &'a str, directory: bool, limits: &Limits) -> Result<&'a str> {
    let value = name.strip_prefix("./").unwrap_or(name);
    let value = if directory {
        value.trim_end_matches('/')
    } else {
        value
    };
    // An empty name is one empty step.
    let sized = value.chars().count() <= limits.path;
    let stepped = value
        .split('/')
        .all(|step| !matches!(step, "" | "." | ".."));
    let plain = !value
        .chars()
        .any(|character| character == '\\' || u32::from(character) < 32);
    require(sized && stepped && plain, "image_member_path")?;
    Ok(value)
}

/// A relative path without steps that are empty or a dot, and with every
/// step up taken where there is something to leave.
fn normalized(path: &str) -> String {
    let mut steps: Vec<&str> = Vec::new();
    for step in path.split('/') {
        match step {
            "" | "." => {}
            ".." if steps.last().is_some_and(|last| *last != "..") => {
                steps.pop();
            }
            _ => steps.push(step),
        }
    }
    if steps.is_empty() {
        ".".to_owned()
    } else {
        steps.join("/")
    }
}

/// The directory of an entry, from the image's root.
fn parent(name: &str) -> &str {
    name.rsplit_once('/').map_or(".", |(parent, _)| parent)
}

/// Where the link `name` points, from the image's root: a target that
/// begins at the root, and that of any hard link, is taken from there;
/// another from the link's own directory. One that leaves the image is
/// refused.
pub fn link_target(name: &str, target: &str, hard: bool) -> Result<String> {
    // The slashes at its beginning are empty steps, which are dropped.
    let relative = if hard || target.starts_with('/') {
        target.to_owned()
    } else {
        format!("{}/{target}", parent(name))
    };
    let path = normalized(&relative);
    let inside = path != ".." && !path.starts_with("../") && !path.contains('\\');
    require(inside, "image_link_escape")?;
    Ok(path)
}

/// The number that a field of a tar header holds: octal text up to a NUL,
/// or the bytes themselves when the first has its high bit set.
fn number(field: &[u8]) -> Option<i128> {
    if let [first @ (0o200 | 0o377), rest @ ..] = field {
        let whole = rest
            .iter()
            .fold(0_i128, |whole, byte| (whole << 8) | i128::from(*byte));
        let negative = *first == 0o377;
        return Some(if negative {
            whole - (1_i128 << (8 * rest.len()))
        } else {
            whole
        });
    }
    let text = field.split(|byte| *byte == 0).next().unwrap_or_default();
    // Nothing but ASCII is a space or a digit below.
    let text = std::str::from_utf8(text).ok()?;
    let space = |character: char| {
        character.is_ascii_whitespace()
            || ('\u{1c}'..='\u{1f}').contains(&character)
            || character == '\u{b}'
    };
    let text = text.trim_matches(space);
    if text.is_empty() {
        return Some(0);
    }
    let (negative, digits) = match text.strip_prefix('-') {
        Some(digits) => (true, digits),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    // An underscore may follow the mark of the base, but not begin a number.
    let marked = digits
        .strip_prefix("0o")
        .or_else(|| digits.strip_prefix("0O"));
    let digits = match marked {
        Some(marked) => marked.strip_prefix('_').unwrap_or(marked),
        None => digits,
    };
    // No sign may follow; what is empty or no octal number is refused below.
    let spelled = !digits.starts_with('_')
        && !digits.ends_with('_')
        && !digits.contains("__")
        && digits
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'_');
    if !spelled {
        return None;
    }
    let plain: String = digits
        .chars()
        .filter(|character| *character != '_')
        .collect();
    let whole = i128::from_str_radix(&plain, 8).ok()?;
    Some(if negative { -whole } else { whole })
}

/// What a raw tar header says of its member: its kind and its size, when
/// its check and every one of its fields can be read.
fn header(block: &[u8; BLOCK]) -> Option<(u8, i128)> {
    let said = number(&block[148..156])?;
    let unsigned: i128 = block
        .iter()
        .enumerate()
        .map(|(at, byte)| {
            if (148..156).contains(&at) {
                32
            } else {
                i128::from(*byte)
            }
        })
        .sum();
    let signed: i128 = block
        .iter()
        .enumerate()
        .map(|(at, byte)| {
            if (148..156).contains(&at) {
                32
            } else {
                i128::from(*byte as i8)
            }
        })
        .sum();
    if said != unsigned && said != signed {
        return None;
    }
    let text = |field: &[u8]| {
        let field = field.split(|byte| *byte == 0).next().unwrap_or_default();
        std::str::from_utf8(field).is_ok()
    };
    let fields = [100..108, 108..116, 116..124, 136..148, 329..337, 337..345];
    let read = fields
        .into_iter()
        .all(|field| number(&block[field]).is_some())
        && [0..100, 157..257, 265..297, 297..329, 345..500]
            .into_iter()
            .all(|field| text(&block[field]));
    let size = number(&block[124..136])?;
    read.then_some((block[156], size))
}

/// Walk the raw headers of a tar archive before anything interprets it:
/// only files, directories, links and the headers that carry long names
/// and attributes may be there, each within its bound, and the archive
/// must end with its two empty blocks and nothing but zero bytes after
/// them. The outer archive of a saved image has no links.
pub fn physical_headers<R: Read + Seek>(
    source: &mut R,
    outer: bool,
    limits: &Limits,
) -> Result<()> {
    let unreadable = |_| HOST;
    let total = source.seek(SeekFrom::End(0)).map_err(unreadable)?;
    source.seek(SeekFrom::Start(0)).map_err(unreadable)?;
    let (mut at, mut count, mut metadata) = (0_u64, 0_usize, 0_u64);
    let mut block = [0_u8; BLOCK];
    while at < total {
        require(total - at >= BLOCK as u64, "image_tar_truncated")?;
        source.read_exact(&mut block).map_err(unreadable)?;
        at += BLOCK as u64;
        if block.iter().all(|byte| *byte == 0) {
            let mut rest = Vec::new();
            (&mut *source)
                .take(BLOCK as u64)
                .read_to_end(&mut rest)
                .map_err(unreadable)?;
            let ended = rest.len() == BLOCK && rest.iter().all(|byte| *byte == 0);
            require(ended, "image_tar_terminator")?;
            let mut chunk = vec![0_u8; 65536];
            loop {
                let read = source.read(&mut chunk).map_err(unreadable)?;
                if read == 0 {
                    break;
                }
                require(
                    chunk[..read].iter().all(|byte| *byte == 0),
                    "image_tar_trailing_data",
                )?;
            }
            source.seek(SeekFrom::Start(0)).map_err(unreadable)?;
            return Ok(());
        }
        let (kind, size) = header(&block).ok_or(Fault("image_tar_header"))?;
        count += 1;
        // A file of the old kind may be a directory; both are members.
        let named = matches!(kind, b'x' | b'g' | b'L' | b'K');
        let member = matches!(kind, b'0' | 0 | b'5' | b'2' | b'1') || named;
        require(count <= limits.members && member, "image_tar_header")?;
        require(!outer || !matches!(kind, b'2' | b'1'), "image_outer_link")?;
        let limit = if named {
            limits.metadata
        } else if outer {
            limits.archive
        } else {
            limits.file
        };
        let size = u64::try_from(size).ok().filter(|size| *size <= limit);
        let size = size.ok_or(Fault("image_tar_body_size"))?;
        if named {
            metadata += size;
            require(
                metadata <= limits.metadata_total,
                "image_tar_metadata_total",
            )?;
        }
        let end = at + size.div_ceil(BLOCK as u64) * BLOCK as u64;
        require(end <= total, "image_tar_truncated")?;
        source.seek(SeekFrom::Start(end)).map_err(unreadable)?;
        at = end;
    }
    Err(Fault("image_tar_terminator"))
}

/// Create the new private file `path`. Where anything is already, a link
/// to nowhere included, nothing is made.
fn created(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    options.open(path).map_err(|_| HOST)
}

/// Publish only a new private regular file at a path of the caller's.
pub fn new_file(path: &Path, data: &[u8]) -> Result<()> {
    created(path)?.write_all(data).map_err(|_| HOST)
}

/// A reader that keeps the digest of everything read through it.
struct Hashed<R> {
    source: R,
    hash: Sha256,
}

impl<R: Read> Read for Hashed<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.source.read(buffer)?;
        self.hash.update(&buffer[..read]);
        Ok(read)
    }
}

/// Write the layer that `source` holds, unpacked when it is one gzip
/// member, as the new private file `destination`, and answer its size. Its
/// unpacked bytes must have the digest `expected`, and a blob that is
/// named by a digest must have that digest as it is stored.
pub fn layer_file<R: Read>(
    source: R,
    destination: &Path,
    expected: &str,
    blob: &str,
    limits: &Limits,
) -> Result<u64> {
    const PACKED: Fault = Fault("image_layer_compression");
    let hashed = Hashed {
        source,
        hash: Sha256::new(),
    };
    let mut raw = BufReader::with_capacity(65536, hashed);
    let packed = raw.fill_buf().map_err(|_| HOST)?.starts_with(&[0x1f, 0x8b]);
    let mut output = created(destination)?;
    let mut unpacked = Sha256::new();
    let mut total = 0_u64;
    let mut chunk = vec![0_u8; 65536];
    let mut copy = |from: &mut dyn Read, refusal: Fault| -> Result<()> {
        loop {
            let read = from.read(&mut chunk).map_err(|_| refusal)?;
            if read == 0 {
                return Ok(());
            }
            total += read as u64;
            require(total <= limits.layer, "image_layer_expansion")?;
            unpacked.update(&chunk[..read]);
            output.write_all(&chunk[..read]).map_err(|_| HOST)?;
        }
    };
    if packed {
        let mut member = flate2::bufread::GzDecoder::new(&mut raw);
        copy(&mut member, PACKED)?;
        drop(member);
        // One member and nothing after it.
        require(raw.fill_buf().map_err(|_| HOST)?.is_empty(), PACKED.0)?;
    } else {
        copy(&mut raw, HOST)?;
    }
    let stored = hexadecimal(&raw.into_inner().hash.finalize());
    let said = format!("sha256:{}", hexadecimal(&unpacked.finalize()));
    require(said == expected, "image_layer_diff_id")?;
    let named = blob.strip_prefix("blobs/sha256/").filter(|name| {
        let digit = |byte: u8| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte);
        name.len() == 64 && name.bytes().all(digit)
    });
    require(
        named.is_none_or(|name| name == stored),
        "image_layer_blob_digest",
    )?;
    Ok(total)
}

/// What a path of the image is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// A directory.
    Directory,
    /// A regular file, by where its bytes are kept on the host.
    File(PathBuf),
    /// A symbolic link, by its target as the image spells it.
    Symlink(String),
    /// A hard link, by its target.
    Hardlink(String),
    /// A marker that removes what an earlier layer had.
    Whiteout,
}

/// What one layer holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layer {
    /// Its entries, by their paths.
    pub entries: BTreeMap<String, Entry>,
    /// The paths it removes.
    pub whiteouts: Vec<String>,
    /// The directories whose earlier contents it hides.
    pub opaque: Vec<String>,
    /// How many members it has.
    pub count: usize,
}

/// A private directory with every missing directory above it.
fn directories(path: &Path) -> Result<()> {
    let made = DirBuilder::new().recursive(true).mode(0o700).create(path);
    made.map_err(|_| HOST)
}

/// Read the layer at `path`: its raw headers are walked first, every
/// regular file is copied below `destination` under its own path, and
/// what it says of the file system is answered without touching the host.
pub fn read_layer(path: &Path, destination: &Path, limits: &Limits) -> Result<Layer> {
    let mut source = File::open(path).map_err(|_| HOST)?;
    physical_headers(&mut source, false, limits)?;
    let mut layer = Layer::default();
    let mut archive = tar::Archive::new(source);
    let unread = |_| Fault("image_tar_header");
    for member in archive.entries().map_err(unread)? {
        let mut member = member.map_err(unread)?;
        let kind = member.header().entry_type();
        if kind.is_pax_global_extensions() {
            // What would rename or resize every member after it is read by
            // some readers and not by others: no layer may say it.
            let mut records = Vec::new();
            member.read_to_end(&mut records).map_err(|_| HOST)?;
            for record in tar::PaxExtensions::new(&records) {
                let record = record.map_err(unread)?;
                let key = record.key_bytes();
                let applied = matches!(key, b"path" | b"linkpath" | b"size");
                let applied = applied || key.starts_with(b"GNU.sparse");
                require(!applied, "image_global_header")?;
            }
            continue;
        }
        // The walk of the raw headers held their number to its bound.
        layer.count += 1;
        let raw = member.path_bytes().into_owned();
        let spelled = std::str::from_utf8(&raw).map_err(|_| Fault("image_member_path"))?;
        let old = member.header().as_bytes()[156] == 0 && spelled.ends_with('/');
        let directory = kind.is_dir() || old;
        let file = !directory && kind.is_file();
        if directory && matches!(spelled, "." | "./") {
            continue;
        }
        let name = canonical(spelled, directory, limits)?.to_owned();
        require(!layer.entries.contains_key(&name), "image_member_duplicate")?;
        // The walk of the raw headers let no sparse file of the old kind by.
        let mut sparse = false;
        if let Some(extensions) = member.pax_extensions().map_err(unread)? {
            for extension in extensions {
                let key = extension
                    .map_err(unread)?
                    .key_bytes()
                    .starts_with(b"GNU.sparse");
                sparse |= key;
            }
        }
        require(!sparse, "image_sparse_member")?;
        let size = member.size();
        let link = member
            .link_name_bytes()
            .map(std::borrow::Cow::into_owned)
            .unwrap_or_default();
        let link = String::from_utf8(link).map_err(|_| Fault("image_link_target"))?;
        let filename = name.rsplit('/').next().unwrap_or(&name);
        let entry = if let Some(hidden) = filename.strip_prefix(WHITEOUT) {
            require(file && size == 0, "image_whiteout_shape")?;
            if filename == OPAQUE {
                layer.opaque.push(parent(&name).to_owned());
            } else {
                require(!matches!(hidden, "" | "." | ".."), "image_whiteout_name")?;
                let within = parent(&name);
                let removed = if within == "." {
                    hidden.to_owned()
                } else {
                    format!("{within}/{hidden}")
                };
                layer.whiteouts.push(removed);
            }
            Entry::Whiteout
        } else if file {
            require(
                size <= limits.file && link.is_empty(),
                "image_regular_shape",
            )?;
            let content = destination.join(&name);
            directories(content.parent().unwrap_or(destination))?;
            let mut output = created(&content)?;
            let copied = std::io::copy(&mut member, &mut output).map_err(|_| HOST)?;
            require(copied == size, "image_member_truncated")?;
            Entry::File(content)
        } else if directory {
            require(size == 0 && link.is_empty(), "image_directory_shape")?;
            Entry::Directory
        } else if kind.is_symlink() || kind.is_hard_link() {
            let sized = size == 0 && !link.is_empty() && link.chars().count() <= limits.path;
            require(sized, "image_link_shape")?;
            let plain = !link.chars().any(|character| u32::from(character) < 32);
            require(plain, "image_link_target")?;
            if kind.is_symlink() {
                Entry::Symlink(link)
            } else {
                Entry::Hardlink(link)
            }
        } else {
            return Err(Fault("image_member_kind"));
        };
        layer.entries.insert(name, entry);
    }
    Ok(layer)
}

/// Take `name` away, with everything below it, or only what is below it.
/// The paths below a directory lie together in the order of the paths, so
/// that a layer of many files is not laid over many files one by one.
fn remove_tree(entries: &mut BTreeMap<String, Entry>, name: &str, children_only: bool) {
    if name == "." {
        entries.clear();
        return;
    }
    // The character after the slash ends the paths below the directory.
    let (first, after) = (format!("{name}/"), format!("{name}0"));
    let below = entries.range(first..after).map(|(path, _)| path.clone());
    for path in below.collect::<Vec<String>>() {
        entries.remove(&path);
    }
    if !children_only {
        entries.remove(name);
    }
}

/// The directories above `name`, nearest first, without the root.
fn ancestors(name: &str) -> impl Iterator<Item = &str> {
    let mut rest = name;
    std::iter::from_fn(move || {
        let (above, _) = rest.rsplit_once('/')?;
        rest = above;
        Some(above)
    })
}

/// Lay a layer over what the layers before it came to: its whiteouts hide
/// only what was there before; anything but a directory replaces what was
/// at its path with everything below; every entry's ancestors must be
/// directories; and a hard link becomes the file it names, as it is now.
fn overlay(current: &mut BTreeMap<String, Entry>, layer: Layer, limits: &Limits) -> Result<()> {
    for name in &layer.whiteouts {
        remove_tree(current, name, false);
    }
    for name in &layer.opaque {
        remove_tree(current, name, true);
    }
    // In the order of their paths a directory comes before what is below
    // it. Nothing is below what was no directory, so that a directory
    // replaces it by taking its path.
    for (name, entry) in layer.entries {
        if entry == Entry::Whiteout {
            continue;
        }
        if entry != Entry::Directory {
            remove_tree(current, &name, false);
        }
        current.insert(name, entry);
    }
    for name in current.keys() {
        let held = |above: &str| {
            current
                .get(above)
                .is_none_or(|held| *held == Entry::Directory)
        };
        require(ancestors(name).all(held), "image_parent_not_directory")?;
    }
    let linked: Vec<String> = current
        .iter()
        .filter(|(_, entry)| matches!(entry, Entry::Hardlink(_)))
        .map(|(name, _)| name.clone())
        .collect();
    for name in linked {
        let mut selected = current.get(&name);
        let mut links = 0;
        while let Some(Entry::Hardlink(target)) = selected {
            links += 1;
            require(links <= limits.links, "image_hardlink_cycle")?;
            selected = current.get(&link_target(&name, target, true)?);
            require(selected.is_some(), "image_hardlink_missing")?;
        }
        let Some(file @ Entry::File(_)) = selected.cloned() else {
            return Err(Fault("image_hardlink_kind"));
        };
        current.insert(name, file);
    }
    Ok(())
}

/// How `target` is reached from the directory `start`, both from the
/// image's root.
fn relative(target: &str, start: &str) -> String {
    let steps = |path: &str| -> Vec<String> {
        let steps = path.split('/').filter(|step| *step != ".");
        steps.map(str::to_owned).collect()
    };
    let (target, start) = (steps(target), steps(start));
    let common = target
        .iter()
        .zip(&start)
        .take_while(|(left, right)| left == right)
        .count();
    let mut path = vec!["..".to_owned(); start.len() - common];
    path.extend_from_slice(&target[common..]);
    if path.is_empty() {
        ".".to_owned()
    } else {
        path.join("/")
    }
}

/// Build the final file system as the new directory `root`: directories
/// of the host's own, files as further names of the kept copies, and
/// every symbolic link relative, so that none leads out of `root`.
fn materialize(entries: &BTreeMap<String, Entry>, root: &Path) -> Result<()> {
    DirBuilder::new()
        .mode(0o700)
        .create(root)
        .map_err(|_| HOST)?;
    for (name, entry) in entries {
        let path = root.join(name);
        directories(path.parent().unwrap_or(root))?;
        match entry {
            Entry::Directory => directories(&path)?,
            Entry::File(content) => std::fs::hard_link(content, &path).map_err(|_| HOST)?,
            _ => {}
        }
    }
    for (name, entry) in entries {
        if let Entry::Symlink(link) = entry {
            let target = link_target(name, link, false)?;
            let target = relative(&target, parent(name));
            std::os::unix::fs::symlink(target, root.join(name)).map_err(|_| HOST)?;
        }
    }
    Ok(())
}

/// Where the bytes of a file of the outer archive begin and how many
/// they are; a directory has none.
type Place = Option<(u64, u64)>;

/// The members of the outer archive by their names, which only files and
/// directories may be, each under a plain name of its own.
fn members(archive: &mut File, limits: &Limits) -> Result<BTreeMap<String, Place>> {
    const REFUSED: Fault = Fault("image_outer_member");
    let unread = |_| Fault("image_tar_header");
    archive.seek(SeekFrom::Start(0)).map_err(|_| HOST)?;
    let mut found = BTreeMap::new();
    let mut outer = tar::Archive::new(&mut *archive);
    for member in outer.entries_with_seek().map_err(unread)? {
        let member = member.map_err(unread)?;
        let kind = member.header().entry_type();
        let name = String::from_utf8(member.path_bytes().into_owned()).map_err(|_| REFUSED)?;
        let name = name.trim_end_matches('/');
        let stepped = name.split('/').all(|step| !matches!(step, "" | "." | ".."));
        let plain = !name
            .chars()
            .any(|character| character == '\\' || u32::from(character) < 32);
        require(
            stepped && plain && (kind.is_file() || kind.is_dir()),
            REFUSED.0,
        )?;
        let place = kind
            .is_file()
            .then(|| (member.raw_file_position(), member.size()));
        require(found.insert(name.to_owned(), place).is_none(), REFUSED.0)?;
        require(found.len() <= limits.outer, REFUSED.0)?;
    }
    Ok(found)
}

/// The small member `name` of the outer archive, whole.
fn metadata(archive: &mut File, members: &BTreeMap<String, Place>, name: &str) -> Result<Vec<u8>> {
    const MAX_JSON: u64 = 1024 * 1024;
    let found = members.get(name).copied().flatten();
    let found = found.filter(|(_, size)| (1..=MAX_JSON).contains(size));
    let (at, size) = found.ok_or(Fault("image_metadata_missing"))?;
    archive.seek(SeekFrom::Start(at)).map_err(|_| HOST)?;
    let mut body = Vec::new();
    let read = (&mut *archive)
        .take(size)
        .read_to_end(&mut body)
        .map_err(|_| HOST)?;
    require(read as u64 == size, "image_tar_truncated")?;
    Ok(body)
}

/// The first image of the outer archive's own list: the name of its
/// configuration and the names of its layers, and how many images are
/// listed.
fn listed(
    archive: &mut File,
    members: &BTreeMap<String, Place>,
) -> Result<(String, Vec<String>, usize)> {
    let invalid = Fault("image_metadata_invalid");
    let list = metadata(archive, members, "manifest.json")?;
    let list = Document::read(&list).map_err(|_| invalid)?;
    let images = list.items().ok_or(invalid)?;
    let image = images.first().ok_or(invalid)?;
    let config = image
        .get("Config")
        .and_then(Document::text)
        .ok_or(invalid)?;
    let layers = image
        .get("Layers")
        .and_then(Document::items)
        .ok_or(invalid)?;
    let layers: Option<Vec<String>> = layers
        .iter()
        .map(|layer| layer.text().map(str::to_owned))
        .collect();
    Ok((config.to_owned(), layers.ok_or(invalid)?, images.len()))
}

/// The identity of the one image that the saved archive holds: the digest
/// of the bytes of its configuration.
pub fn image_identity(archive: &Path, limits: &Limits) -> Result<String> {
    let mut archive = File::open(archive).map_err(|_| HOST)?;
    let members = members(&mut archive, limits)?;
    let (config, _, images) = listed(&mut archive, &members)?;
    require(images == 1, "image_config_selection")?;
    let config = metadata(&mut archive, &members, &config)?;
    Ok(format!("sha256:{}", crate::digest(&config)))
}

/// Unpack the layers of the saved image `archive`, whose configuration
/// was verified as the bytes `config`, into the new directory `output`:
/// every layer's regular files below `layers/NNN`, the configuration
/// beside them, and the final file system as `rootfs`. The report says
/// what was found.
pub fn materialize_layers(
    archive: &Path,
    config: &[u8],
    output: &Path,
    limits: &Limits,
) -> Result<Vec<(String, Document)>> {
    let invalid = Fault("image_metadata_invalid");
    let described = Document::read(config).map_err(|_| invalid)?;
    let rootfs = described.get("rootfs").ok_or(invalid)?;
    let diff_ids = rootfs
        .get("diff_ids")
        .and_then(Document::items)
        .ok_or(invalid)?;
    require(
        !diff_ids.is_empty() && diff_ids.len() <= limits.layers,
        "image_layer_count",
    )?;
    DirBuilder::new()
        .mode(0o700)
        .create(output)
        .map_err(|_| HOST)?;
    let layers = output.join("layers");
    DirBuilder::new()
        .mode(0o700)
        .create(&layers)
        .map_err(|_| HOST)?;
    let mut archive = File::open(archive).map_err(|_| HOST)?;
    let members = members(&mut archive, limits)?;
    let (name, names, _) = listed(&mut archive, &members)?;
    let raw_config = metadata(&mut archive, &members, &name)?;
    require(raw_config == config, "image_config_changed")?;
    let config_sha256 = crate::digest(&raw_config);
    new_file(&layers.join("image-config.json"), &raw_config)?;
    require(names.len() == diff_ids.len(), "image_layer_count_mismatch")?;
    let mut entries = BTreeMap::new();
    let (mut expanded, mut counted) = (0_u64, 0_usize);
    let mut records = Vec::new();
    for (index, (name, expected)) in names.iter().zip(diff_ids).enumerate() {
        let expected = expected.text().ok_or(invalid)?;
        let digit = |byte: u8| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte);
        let shaped = expected.strip_prefix("sha256:");
        let shaped = shaped.is_some_and(|hex| hex.len() == 64 && hex.bytes().all(digit));
        require(shaped, "image_diff_id_shape")?;
        let found = members.get(name).copied().flatten();
        let (at, size) = found.ok_or(Fault("image_layer_missing"))?;
        archive.seek(SeekFrom::Start(at)).map_err(|_| HOST)?;
        let raw = output.join(format!("layer-{index:03}.tar"));
        let size = layer_file((&mut archive).take(size), &raw, expected, name, limits)?;
        expanded += size;
        require(expanded <= limits.expanded, "image_total_expansion")?;
        let destination = layers.join(format!("{index:03}"));
        DirBuilder::new()
            .mode(0o700)
            .create(&destination)
            .map_err(|_| HOST)?;
        let layer = read_layer(&raw, &destination, limits)?;
        let count = layer.count;
        counted += count;
        require(counted <= limits.members, "image_total_members")?;
        overlay(&mut entries, layer, limits)?;
        records.push(Document::record([
            ("index", index.into()),
            ("diffId", expected.into()),
            ("expandedBytes", size.into()),
            ("members", count.into()),
        ]));
        std::fs::remove_file(&raw).map_err(|_| HOST)?;
    }
    materialize(&entries, &output.join("rootfs"))?;
    let field = |name: &str, value: Document| (name.to_owned(), value);
    Ok(vec![
        field("passed", true.into()),
        field("expandedBytes", expanded.into()),
        field("members", counted.into()),
        field("layers", Document::List(records)),
        field("regularLayerFilesPreserved", true.into()),
        field("configIncluded", true.into()),
        field("configSha256", config_sha256.as_str().into()),
        field("imageId", format!("sha256:{config_sha256}").into()),
    ])
}

/// Extract the saved image `archive` that the release manifest at
/// `manifest` names into the new directory `output`, and write the report
/// there. The archive must be a regular file that is no link; its raw
/// headers are walked before anything reads it, and the release verifier
/// authenticates it and its configuration first.
pub fn extract(
    archive: &Path,
    manifest: &Path,
    output: &Path,
    limits: &Limits,
) -> Result<Document> {
    let held = archive.symlink_metadata();
    let regular = held.is_ok_and(|held| held.is_file() && held.len() <= limits.archive);
    require(regular, "image_archive_file")?;
    let mut source = File::open(archive).map_err(|_| HOST)?;
    physical_headers(&mut source, true, limits)?;
    drop(source);
    let manifest = simplestchat_ops::release::manifest::Manifest::read(manifest);
    let manifest = manifest.map_err(|_| Fault("release_manifest_invalid"))?;
    let verified = simplestchat_ops::release::archive::verify_archive(archive, &manifest);
    let verified = verified.map_err(|refusal| Fault(refusal.code()))?;
    let mut report = materialize_layers(archive, verified.config_bytes(), output, limits)?;
    let fields = manifest.fields();
    for (name, value) in [
        ("archiveSha256", &fields.archive_sha256),
        ("revision", &fields.revision),
        ("platform", &fields.platform),
    ] {
        report.push((name.to_owned(), value.as_str().into()));
    }
    let report = Document::Record(report);
    new_file(&output.join("report.json"), report.pretty().as_bytes())?;
    Ok(report)
}

/// The saved image, its release manifest and the new directory of the
/// evidence that a command line names, each once.
pub(super) fn asked(arguments: &[String]) -> Option<[PathBuf; 3]> {
    let options = ["--archive", "--manifest", "--output"];
    let mut named: [Option<PathBuf>; 3] = Default::default();
    let mut rest = arguments.iter();
    while let Some(option) = rest.next() {
        let value = rest.next();
        let value = value.filter(|value| !value.is_empty() && !value.starts_with('-'))?;
        let at = options.iter().position(|held| held == option)?;
        if named[at].replace(value.into()).is_some() {
            return None;
        }
    }
    let [image, manifest, output] = named;
    Some([image?, manifest?, output?])
}

#[cfg(test)]
#[path = "archive_tests.rs"]
mod tests;
