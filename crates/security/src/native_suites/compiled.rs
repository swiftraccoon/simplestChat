//! A compiled suite kept for a later run: an ordinary native executable of
//! this host's architecture, and the receipt that binds it to its suite,
//! image and inputs.

use super::{MIB, read_regular};
use crate::{Fault, Result, document::Document, require};
use rustix::fs::{Mode, OFlags};
use sha2::{Digest as _, Sha256};
use std::{
    collections::{BTreeSet, HashMap},
    ffi::OsStr,
    fs::File,
    io::{Read, Seek, SeekFrom},
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::MetadataExt,
    },
    path::Path,
};

/// The bytes of an executable's ELF header.
pub const ELF_HEADER_BYTES: u64 = 64;
/// The most bytes of an executable.
pub const MAX_BINARY: u64 = 512 * MIB;
/// A receipt that is no JSON record.
const UNREAD: Fault = Fault("native_receipt_unread");
/// A directory that cannot be listed, which failed the reference in its
/// interpreter's words.
const UNLISTED: Fault = Fault("invalid_native_security_input");
/// The fields of a receipt.
const FIELDS: [&str; 9] = [
    "schemaVersion",
    "status",
    "mode",
    "imageId",
    "inputsSha256",
    "architecture",
    "binaryBytes",
    "binarySha256",
    "worker",
];

/// The ELF machine of an architecture.
pub(crate) fn machine(architecture: &str) -> Result<u16> {
    match architecture {
        "amd64" => Ok(62),
        "arm64" => Ok(183),
        _ => Err(Fault("native_host_architecture_unsupported")),
    }
}

/// `path` as the reference's paths spell one: without repeated separators,
/// current-directory steps or a final separator, with exactly two leading
/// separators kept, and `.` for nothing.
fn spelled(path: &[u8]) -> Vec<u8> {
    let root: &[u8] = if !path.starts_with(b"/") {
        b""
    } else if path.starts_with(b"//") && !path.starts_with(b"///") {
        b"//"
    } else {
        b"/"
    };
    let steps = path.split(|byte| *byte == b'/');
    let steps: Vec<&[u8]> = steps
        .filter(|step| !step.is_empty() && *step != b".")
        .collect();
    let mut text = root.to_vec();
    text.extend(steps.join(&b'/'));
    if text.is_empty() {
        text.push(b'.');
    }
    text
}

/// What the reference's resolution makes of the absolute path `spelled`,
/// step by step as its interpreter takes one: a parent step goes back, a
/// link is replaced by its target, a link met again while it is resolved is
/// kept as it is, and a step that cannot be looked at is kept as it is
/// spelled.
fn resolved(spelled: &[u8]) -> Vec<u8> {
    // A step of nothing marks a resolved link, which is the step under it.
    let mut rest: Vec<Option<Vec<u8>>> = spelled
        .split(|byte| *byte == b'/')
        .rev()
        .map(|step| Some(step.to_vec()))
        .collect();
    let mut steps = rest.len();
    let mut path = b"/".to_vec();
    let mut seen: HashMap<Vec<u8>, Option<Vec<u8>>> = HashMap::new();
    while steps > 0 {
        let Some(step) = rest.pop() else { break };
        let Some(name) = step else {
            if let Some(Some(link)) = rest.pop() {
                seen.insert(link, Some(path.clone()));
            }
            continue;
        };
        steps -= 1;
        if name.is_empty() || name == b"." {
            continue;
        }
        if name == b".." {
            let cut = path.iter().rposition(|byte| *byte == b'/').unwrap_or(0);
            path.truncate(cut);
            if path.is_empty() {
                path.push(b'/');
            }
            continue;
        }
        let mut next = path.clone();
        if next != b"/" {
            next.push(b'/');
        }
        next.extend_from_slice(&name);
        let looked = std::fs::symlink_metadata(OsStr::from_bytes(&next));
        if !looked.is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            path = next;
            continue;
        }
        match seen.get(&next) {
            Some(Some(target)) => {
                path.clone_from(target);
                continue;
            }
            Some(None) => {
                path = next;
                continue;
            }
            None => {}
        }
        let Ok(target) = std::fs::read_link(OsStr::from_bytes(&next)) else {
            path = next;
            continue;
        };
        let target = target.into_os_string().into_vec();
        if target.starts_with(b"/") {
            path = b"/".to_vec();
        }
        seen.insert(next.clone(), None);
        rest.push(Some(next));
        rest.push(None);
        let parts = target.split(|byte| *byte == b'/');
        let parts: Vec<Option<Vec<u8>>> = parts.map(|part| Some(part.to_vec())).collect();
        steps += parts.len();
        rest.extend(parts.into_iter().rev());
    }
    path
}

/// Whether `directory` is absolute and already what the reference's
/// resolution makes of it, as the directory of a kept build must be: no
/// parent step and no link on the way to it, however much of it is there.
pub fn canonical(directory: &Path) -> bool {
    let spelled = spelled(directory.as_os_str().as_bytes());
    directory.is_absolute() && resolved(&spelled) == spelled
}

/// The size and SHA-256 of the ordinary 64-bit little-endian executable
/// `path` for `architecture`, read without following a final link, and
/// refused if it changes while it is read.
pub fn binary_identity(path: &Path, architecture: &str) -> Result<(u64, String)> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let opened = rustix::fs::open(path, flags, Mode::empty());
    let mut file = opened
        .map(File::from)
        .map_err(|_| Fault("native_input_unreadable"))?;
    let metadata = file
        .metadata()
        .map_err(|_| Fault("native_input_unreadable"))?;
    require(metadata.is_file(), "compiled_binary_not_regular")?;
    require(
        (ELF_HEADER_BYTES..=MAX_BINARY).contains(&metadata.len()),
        "compiled_binary_size",
    )?;
    let mut header = [0; 20];
    file.read_exact(&mut header)
        .map_err(|_| Fault("compiled_binary_changed"))?;
    let elf = header[..6] == *b"\x7fELF\x02\x01"
        && u16::from_le_bytes([header[18], header[19]]) == machine(architecture)?;
    require(elf, "compiled_binary_architecture")?;
    file.seek(SeekFrom::Start(0))
        .map_err(|_| Fault("compiled_binary_changed"))?;
    let mut hashed = Sha256::new();
    let mut chunk = vec![0; MIB as usize];
    loop {
        let read = file
            .read(&mut chunk)
            .map_err(|_| Fault("compiled_binary_changed"))?;
        if read == 0 {
            break;
        }
        hashed.update(&chunk[..read]);
    }
    let after = file
        .metadata()
        .map_err(|_| Fault("compiled_binary_changed"))?;
    let unchanged = (after.len(), after.mtime(), after.mtime_nsec())
        == (metadata.len(), metadata.mtime(), metadata.mtime_nsec());
    require(unchanged, "compiled_binary_changed")?;
    Ok((metadata.len(), crate::hexadecimal(&hashed.finalize())))
}

/// The receipt of the compiled suite in `directory`, which must hold that
/// receipt and the executable alone, built for the suite `mode` in the image
/// `image` of the inputs `digest` on `architecture`, with the executable's
/// exact bytes.
pub fn compiled_receipt(
    directory: &Path,
    mode: &str,
    image: &str,
    inputs: &str,
    architecture: &str,
) -> Result<Document> {
    require(canonical(directory), "compiled_path")?;
    let entries = std::fs::read_dir(directory).map_err(|_| UNLISTED)?;
    let mut names = BTreeSet::new();
    for entry in entries {
        let entry = entry.map_err(|_| UNLISTED)?;
        names.insert(entry.file_name().to_string_lossy().into_owned());
    }
    let wanted: BTreeSet<String> = ["binary", "receipt.json"].map(str::to_owned).into();
    require(names == wanted, "compiled_files")?;
    let receipt = read_regular(&directory.join("receipt.json"), MIB)?;
    let receipt = Document::read(&receipt).map_err(|_| UNREAD)?;
    let fields = receipt.fields().ok_or(UNREAD)?;
    let held: BTreeSet<&str> = fields.iter().map(|(name, _)| name.as_str()).collect();
    let text = |name: &str| receipt.get(name).and_then(Document::text);
    let matching = held == FIELDS.iter().copied().collect()
        && receipt.get("schemaVersion") == Some(&Document::from(1_u64))
        && text("status") == Some("built")
        && text("mode") == Some(mode)
        && text("imageId") == Some(image)
        && text("inputsSha256") == Some(inputs)
        && text("architecture") == Some(architecture);
    require(matching, "compiled_receipt_mismatch")?;
    let (size, checksum) = binary_identity(&directory.join("binary"), architecture)?;
    let bytes = receipt.get("binaryBytes").and_then(Document::whole);
    let same = bytes == Some(i128::from(size)) && text("binarySha256") == Some(&checksum);
    require(same, "compiled_binary_mismatch")?;
    Ok(receipt)
}
