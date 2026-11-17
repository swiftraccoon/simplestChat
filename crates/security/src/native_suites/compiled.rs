//! A compiled suite kept for a later run: an ordinary native executable of
//! this host's architecture, and the receipt that binds it to its suite,
//! image and inputs.

use super::{MIB, read_regular};
use crate::{Fault, Result, document::Document, paths::canonical, require};
use rustix::fs::{Mode, OFlags};
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeSet,
    fs::File,
    io::{Read, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
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
