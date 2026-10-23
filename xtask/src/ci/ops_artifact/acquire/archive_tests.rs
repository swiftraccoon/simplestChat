//! Real mature ZIP codecs over tiny owned files; no artifact acquisition,
//! credential, network, candidate execution or installed tool is involved.

use super::archive::{self, Inventory, Member, Sink};
use crate::tooling::Result;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "ops-acquire-zip-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst),
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
    fn file(&self, bytes: &[u8]) -> File {
        let path = self
            .0
            .join(format!("{}.zip", NEXT.fetch_add(1, Ordering::SeqCst)));
        let mut file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .unwrap();
        file.write_all(bytes).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
#[derive(Default)]
struct Memory {
    values: BTreeMap<String, Vec<u8>>,
    current: Option<String>,
    begun: usize,
    failure: Option<(&'static str, u8)>,
}
impl Memory {
    fn check(&self, phase: &str) -> Result<()> {
        if let Some((at, code)) = self.failure
            && at == phase
        {
            return Err(super::failure(code));
        }
        Ok(())
    }
}
impl Sink for Memory {
    fn begin(&mut self, member: &Member) -> Result<()> {
        self.begun += 1;
        assert!(self.current.is_none());
        assert!(
            self.values
                .insert(member.name().into(), Vec::new())
                .is_none()
        );
        self.current = Some(member.name().into());
        self.check("begin")
    }
    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.check("write")?;
        self.values
            .get_mut(self.current.as_ref().unwrap())
            .unwrap()
            .extend_from_slice(bytes);
        Ok(())
    }
    fn finish(&mut self, member: &Member, sha256: &str) -> Result<()> {
        self.check("finish")?;
        let name = self.current.take().unwrap();
        assert_eq!(name, member.name());
        let value = &self.values[&name];
        assert_eq!(value.len() as u64, member.bytes());
        assert_eq!(digest(value), sha256);
        Ok(())
    }
}
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn members(evidence: usize) -> Vec<(String, Vec<u8>)> {
    let mut values = [
        "simplestchat-ops",
        "ops-release.json",
        "ops-evidence.json",
        "ops-predicate.json",
        "ops-preparation.json",
        "ops-attestation.jsonl",
    ]
    .into_iter()
    .map(|name| (name.into(), format!("INERT {name}\0").into_bytes()))
    .collect::<Vec<_>>();
    // Include arbitrary binary bytes without requiring UTF-8 payloads.
    values[0].1 = vec![0, 255, 13, 10, 42];
    for n in 0..evidence {
        values.push((format!("ops-evidence-{n:02}.bin"), vec![n as u8 + 1; 129]));
    }
    values
}
fn zip(values: &[(String, Vec<u8>)], deflate: bool) -> Vec<u8> {
    zip_extra(values, deflate, None)
}
fn zip_extra(values: &[(String, Vec<u8>)], deflate: bool, extra: Option<(u16, &[u8])>) -> Vec<u8> {
    let mut archive = rawzip::ZipArchiveWriter::new(Vec::new());
    for (index, (name, bytes)) in values.iter().enumerate() {
        let builder = archive
            .new_file(rawzip::path::EntryPath::verbatim(name.as_bytes()))
            .compression_method(if deflate {
                rawzip::CompressionMethod::DEFLATE
            } else {
                rawzip::CompressionMethod::STORE
            })
            .unix_permissions(0o100600);
        let builder = if let Some((kind, data)) = extra
            && index == 0
        {
            builder
                .extra_field(
                    rawzip::extra_fields::ExtraFieldId::new(kind),
                    data,
                    rawzip::Header::LOCAL | rawzip::Header::CENTRAL,
                )
                .unwrap()
        } else {
            builder
        };
        let (mut entry, config) = builder.start().unwrap();
        if deflate {
            let encoder =
                flate2::write::DeflateEncoder::new(&mut entry, flate2::Compression::fast());
            let mut writer = config.wrap(encoder);
            writer.write_all(bytes).unwrap();
            let (encoder, descriptor) = writer.finish().unwrap();
            encoder.finish().unwrap();
            entry.finish(descriptor).unwrap();
        } else {
            let mut writer = config.wrap(&mut entry);
            writer.write_all(bytes).unwrap();
            let (_, descriptor) = writer.finish().unwrap();
            entry.finish(descriptor).unwrap();
        }
    }
    archive.finish().unwrap()
}
fn central(bytes: &[u8]) -> Vec<usize> {
    let archive = rawzip::ZipArchive::from_slice(bytes).unwrap();
    let mut entries = archive.entries();
    let mut result = Vec::new();
    while let Some(entry) = entries.next_entry().unwrap() {
        result.push(entry.central_directory_offset() as usize);
    }
    result
}
fn put16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}
fn put32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
fn get32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
fn refused_before_sink(bytes: &[u8]) {
    let fixture = Fixture::new();
    let mut file = fixture.file(bytes);
    let mut sink = Memory::default();
    let error = archive::extract(&mut file, &mut sink, &mut || Ok(()))
        .err()
        .unwrap();
    assert_eq!(error.code, 1);
    assert_eq!(sink.begun, 0);
    assert!(sink.values.is_empty());
    nix::unistd::close(file).unwrap();
}

#[test]
fn stored_and_deflated_complete_flat_payloads_keep_actual_bytes_hashes_and_counts() {
    let fixture = Fixture::new();
    for deflate in [false, true] {
        for count in [0, 16] {
            let values = members(count);
            let mut file = fixture.file(&zip(&values, deflate));
            let mut sink = Memory::default();
            let inventory = archive::extract(&mut file, &mut sink, &mut || Ok(())).unwrap();
            assert_eq!(sink.begun, 6 + count);
            assert!(sink.current.is_none());
            assert_eq!(inventory.records().len(), 6 + count);
            assert_eq!(
                inventory.bytes(),
                values.iter().map(|(_, v)| v.len() as u64).sum::<u64>()
            );
            for record in inventory.records() {
                let original = values.iter().find(|(n, _)| n == record.name()).unwrap();
                assert_eq!(sink.values[record.name()], original.1);
                assert_eq!(record.bytes(), original.1.len() as u64);
                assert_eq!(record.sha256(), digest(&original.1));
            }
            nix::unistd::close(file).unwrap();
        }
    }
}

#[test]
fn closed_names_full_required_set_and_contiguous_evidence_are_admitted_before_output() {
    for name in [
        "../simplestchat-ops",
        "./simplestchat-ops",
        "/simplestchat-ops",
        "dir/simplestchat-ops",
        "simplestchat-ops/",
        "SIMPLESTCHAT-OPS",
        "ops-evidence-16.bin",
        "ops-evidence-0.bin",
        "ops-evidence-00.bin\0",
        "ops-evidence-λ.bin",
        "extra",
    ] {
        let mut values = members(0);
        values[0].0 = name.into();
        refused_before_sink(&zip(&values, false));
    }
    let mut values = members(0);
    values.push(values[0].clone());
    refused_before_sink(&zip(&values, false));
    refused_before_sink(&zip(&members(17), false));
    let mut values = members(1);
    values.last_mut().unwrap().0 = "ops-evidence-01.bin".into();
    refused_before_sink(&zip(&values, true));
    let mut values = members(0);
    values.pop();
    refused_before_sink(&zip(&values, false));
}

#[test]
fn local_central_descriptor_and_full_topology_disagreements_never_begin_output() {
    let original = zip(&members(0), false);
    let centers = central(&original);
    let c = centers[0];
    let eocd = original.len() - 22;
    let mut variants = Vec::new();
    for flags in [1, 0x40, 0x2000, 0x10] {
        let mut bytes = original.clone();
        put16(&mut bytes, c + 8, flags);
        variants.push(bytes);
    }
    for kind in [0o120000, 0o040000, 0o060000] {
        let mut bytes = original.clone();
        put32(&mut bytes, c + 38, kind << 16);
        variants.push(bytes);
    }
    let mut bytes = original.clone();
    bytes[30] ^= 1;
    variants.push(bytes);
    let mut bytes = original.clone();
    put16(&mut bytes, 8, 8);
    variants.push(bytes);
    let mut bytes = original.clone();
    put32(&mut bytes, 14, 123);
    variants.push(bytes);
    let mut bytes = original.clone();
    put32(&mut bytes, centers[1] + 42, 0);
    variants.push(bytes);
    let mut bytes = original.clone();
    put16(&mut bytes, c + 34, 1);
    variants.push(bytes);
    let mut bytes = original.clone();
    put16(&mut bytes, eocd + 8, 5);
    variants.push(bytes);
    let mut bytes = original.clone();
    put16(&mut bytes, eocd + 4, 1);
    variants.push(bytes);
    let mut bytes = original.clone();
    put32(&mut bytes, eocd + 12, 1);
    variants.push(bytes);
    let mut bytes = original.clone();
    put32(&mut bytes, eocd + 16, 1);
    variants.push(bytes);
    let mut bytes = original.clone();
    bytes.extend_from_slice(b"TRAILER");
    variants.push(bytes);
    let mut bytes = b"PRELUDE".to_vec();
    bytes.extend_from_slice(&original);
    variants.push(bytes);
    let mut bytes = original.clone();
    bytes.truncate(bytes.len() - 1);
    variants.push(bytes);
    let body_end = 30 + members(0)[0].0.len() + members(0)[0].1.len();
    let mut bytes = original.clone();
    bytes[body_end + 4] ^= 1;
    variants.push(bytes);
    for bytes in variants {
        refused_before_sink(&bytes);
    }
}

#[test]
fn member_caps_and_sparse_zip_size_refuse_without_payload_allocation() {
    let original = zip(&members(1), false);
    let centers = central(&original);
    for (index, maximum) in [
        (0, 256 * 1024 * 1024),
        (1, 64 * 1024),
        (5, 8 * 1024 * 1024),
        (6, 1024 * 1024),
    ] {
        let mut bytes = original.clone();
        put32(&mut bytes, centers[index] + 24, maximum + 1);
        refused_before_sink(&bytes);
    }
    let fixture = Fixture::new();
    let mut file = fixture.file(&original);
    file.set_len(archive::MAX_ZIP + 1).unwrap();
    let mut sink = Memory::default();
    assert_eq!(
        archive::extract(&mut file, &mut sink, &mut || Ok(()))
            .err()
            .unwrap()
            .code,
        1
    );
    assert_eq!(sink.begun, 0);
    nix::unistd::close(file).unwrap();
}

#[test]
fn alias_unknown_unused_zip64_and_truncated_extra_fields_are_not_ignored() {
    for (kind, bytes) in [
        (0x7075, vec![1; 12]),
        (0x9901, vec![1; 7]),
        (0x0001, vec![0; 16]),
        (0xabcd, vec![0]),
    ] {
        refused_before_sink(&zip_extra(&members(0), false, Some((kind, &bytes))));
    }
    let mut bytes = zip_extra(&members(0), false, Some((0x5455, &[0])));
    let c = central(&bytes)[0];
    let at = c + 46 + members(0)[0].0.len();
    put16(&mut bytes, at + 2, 0xffff);
    refused_before_sink(&bytes);
}

// Change compressed bytes while rebuilding exact offsets/descriptor lengths;
// structural admission remains valid, so only codec completion can refuse.
fn change_compressed(mut bytes: Vec<u8>, extra: bool) -> Vec<u8> {
    let centers = central(&bytes);
    let first = centers[0];
    let old = get32(&bytes, first + 20) as usize;
    let name = members(0)[0].0.len();
    let end = 30 + name + old;
    if extra {
        bytes.insert(end, 0);
    } else {
        bytes.remove(end - 1);
    }
    let shift = |n: usize| if extra { n + 1 } else { n - 1 };
    let compressed = if extra { old + 1 } else { old - 1 };
    put32(&mut bytes, shift(end) + 8, compressed as u32);
    for (index, old_center) in centers.iter().enumerate() {
        let c = shift(*old_center);
        if index == 0 {
            put32(&mut bytes, c + 20, compressed as u32);
        } else {
            let offset = get32(&bytes, c + 42) as usize;
            put32(&mut bytes, c + 42, shift(offset) as u32);
        }
    }
    let eocd = bytes.len() - 22;
    put32(&mut bytes, eocd + 16, shift(first) as u32);
    bytes
}

#[test]
fn deflate_terminal_status_crc_and_exact_compressed_consumption_are_required() {
    let original = zip(&members(0), true);
    for bytes in [
        change_compressed(original.clone(), true),
        change_compressed(original.clone(), false),
    ] {
        let fixture = Fixture::new();
        let mut file = fixture.file(&bytes);
        let mut sink = Memory::default();
        assert_eq!(
            archive::extract(&mut file, &mut sink, &mut || Ok(()))
                .err()
                .unwrap()
                .code,
            1
        );
        assert_eq!(sink.begun, 1);
        assert!(sink.current.is_some()); // Parent owns final settlement on error.
        nix::unistd::close(file).unwrap();
    }
    let mut bytes = zip(&members(0), false);
    bytes[30 + members(0)[0].0.len()] ^= 1;
    let fixture = Fixture::new();
    let mut file = fixture.file(&bytes);
    let mut sink = Memory::default();
    assert!(archive::extract(&mut file, &mut sink, &mut || Ok(())).is_err());
    assert_eq!(sink.begun, 1);
    nix::unistd::close(file).unwrap();
}

#[test]
fn inflated_bytes_cannot_exceed_the_admitted_declared_size() {
    let mut values = members(0);
    values[0].1 = vec![0; 128 * 1024];
    let mut bytes = zip(&values, true);
    let c = central(&bytes)[0];
    let compressed = get32(&bytes, c + 20) as usize;
    let descriptor = 30 + values[0].0.len() + compressed;
    put32(&mut bytes, c + 24, 1);
    put32(&mut bytes, descriptor + 12, 1);
    let fixture = Fixture::new();
    let mut file = fixture.file(&bytes);
    let mut sink = Memory::default();
    assert!(archive::extract(&mut file, &mut sink, &mut || Ok(())).is_err());
    assert_eq!(sink.begun, 1);
    assert!(sink.values.values().all(|value| value.len() <= 1));
    nix::unistd::close(file).unwrap();
}

#[test]
fn checkpoint_and_sink_failures_keep_known_status_without_admitting_later_members() {
    let fixture = Fixture::new();
    for phase in ["begin", "write", "finish"] {
        for code in [42, 124, 130, 143] {
            let mut file = fixture.file(&zip(&members(0), true));
            let mut sink = Memory {
                failure: Some((phase, code)),
                ..Memory::default()
            };
            let error = archive::extract(&mut file, &mut sink, &mut || Ok(()))
                .err()
                .unwrap();
            assert_eq!(error.code, code);
            assert_eq!(sink.begun, 1);
            assert!(sink.current.is_some());
            nix::unistd::close(file).unwrap();
        }
    }
    let mut file = fixture.file(&zip(&members(0), false));
    let mut sink = Memory::default();
    let error = archive::extract(&mut file, &mut sink, &mut || Err(super::failure(143)))
        .err()
        .unwrap();
    assert_eq!(error.code, 143);
    assert_eq!(sink.begun, 0);
    nix::unistd::close(file).unwrap();
}

// Retain mature writer payloads/CRC, changing only the first entry's on-disk
// header profile. This covers seekable headers and bounded ZIP64 size sentinels
// independently of the mature stream writer's default 32-bit descriptor form.
fn first_profile(original: &[u8], wide: bool, descriptor: bool) -> Vec<u8> {
    let centers = central(original);
    let center = centers[0];
    let old_next = get32(original, centers[1] + 42) as usize;
    let compressed = get32(original, center + 20);
    let uncompressed = get32(original, center + 24);
    let crc = get32(original, center + 16);
    let data = 30 + members(0)[0].0.len();
    let mut result = original[..30].to_vec();
    put16(&mut result, 4, if wide { 45 } else { 20 });
    put16(&mut result, 6, if descriptor { 8 } else { 0 });
    put32(&mut result, 14, if descriptor { 0 } else { crc });
    for (at, value) in [(18, compressed), (22, uncompressed)] {
        put32(
            &mut result,
            at,
            if wide {
                u32::MAX
            } else if descriptor {
                0
            } else {
                value
            },
        );
    }
    put16(&mut result, 28, if wide { 20 } else { 0 });
    result.extend_from_slice(&original[30..data]);
    if wide {
        result.extend(1u16.to_le_bytes());
        result.extend(16u16.to_le_bytes());
        for value in [uncompressed, compressed] {
            result.extend(u64::from(if descriptor { 0 } else { value }).to_le_bytes());
        }
    }
    result.extend_from_slice(&original[data..data + compressed as usize]);
    if descriptor {
        result.extend(0x0807_4b50u32.to_le_bytes());
        result.extend(crc.to_le_bytes());
        for value in [compressed, uncompressed] {
            if wide {
                result.extend(u64::from(value).to_le_bytes());
            } else {
                result.extend(value.to_le_bytes());
            }
        }
    }
    let new_next = result.len();
    result.extend_from_slice(&original[old_next..center]);
    let directory = result.len();
    for (index, start) in centers.iter().copied().enumerate() {
        let end = centers
            .get(index + 1)
            .copied()
            .unwrap_or(original.len() - 22);
        let at = result.len();
        result.extend_from_slice(&original[start..end]);
        if index == 0 {
            put16(&mut result, at + 6, if wide { 45 } else { 20 });
            put16(&mut result, at + 8, if descriptor { 8 } else { 0 });
            if wide {
                put32(&mut result, at + 20, u32::MAX);
                put32(&mut result, at + 24, u32::MAX);
                put16(&mut result, at + 30, 20);
                result.extend(1u16.to_le_bytes());
                result.extend(16u16.to_le_bytes());
                result.extend(u64::from(uncompressed).to_le_bytes());
                result.extend(u64::from(compressed).to_le_bytes());
            }
        } else {
            let old = get32(original, start + 42) as usize;
            put32(&mut result, at + 42, (new_next + old - old_next) as u32);
        }
    }
    let size = result.len() - directory;
    let eocd = result.len();
    result.extend_from_slice(&original[original.len() - 22..]);
    put32(&mut result, eocd + 12, size as u32);
    put32(&mut result, eocd + 16, directory as u32);
    result
}

#[test]
fn seekable_local_headers_and_bounded_zip64_entry_sizes_preserve_payloads() {
    let fixture = Fixture::new();
    let values = members(0);
    let original = zip(&values, false);
    for (wide, descriptor) in [(false, false), (true, false), (true, true)] {
        let mut file = fixture.file(&first_profile(&original, wide, descriptor));
        let mut sink = Memory::default();
        let inventory = archive::extract(&mut file, &mut sink, &mut || Ok(())).unwrap();
        assert_eq!(inventory.records().len(), 6);
        assert_eq!(sink.values, values.iter().cloned().collect());
        nix::unistd::close(file).unwrap();
    }
}

#[test]
fn original_file_metadata_and_private_admission_remain_live_through_sink_callbacks() {
    let fixture = Fixture::new();
    for mode in [0o644, 0o4600] {
        let mut file = fixture.file(&zip(&members(0), false));
        file.set_permissions(fs::Permissions::from_mode(mode))
            .unwrap();
        let mut sink = Memory::default();
        assert!(archive::extract(&mut file, &mut sink, &mut || Ok(())).is_err());
        assert_eq!(sink.begun, 0);
        nix::unistd::close(file).unwrap();
    }
    struct Changed {
        inner: Memory,
        source: File,
    }
    impl Sink for Changed {
        fn begin(&mut self, member: &Member) -> Result<()> {
            self.inner.begin(member)?;
            self.source
                .set_permissions(fs::Permissions::from_mode(0o640))
                .unwrap();
            Ok(())
        }
        fn write(&mut self, bytes: &[u8]) -> Result<()> {
            self.inner.write(bytes)
        }
        fn finish(&mut self, member: &Member, sha: &str) -> Result<()> {
            self.inner.finish(member, sha)
        }
    }
    let mut file = fixture.file(&zip(&members(0), true));
    let mut sink = Changed {
        inner: Memory::default(),
        source: file.try_clone().unwrap(),
    };
    assert!(archive::extract(&mut file, &mut sink, &mut || Ok(())).is_err());
    assert_eq!(sink.inner.begun, 1);
    assert!(sink.inner.values.values().all(Vec::is_empty));
    nix::unistd::close(sink.source).unwrap();
    nix::unistd::close(file).unwrap();
}

#[test]
fn canonical_zip64_footer_is_checked_and_arbitrary_footer_gaps_refuse() {
    let original = zip(&members(0), false);
    let eocd = original.len() - 22;
    let directory = get32(&original, eocd + 16);
    let size = get32(&original, eocd + 12);
    let mut extension = Vec::new();
    extension.extend(0x0606_4b50u32.to_le_bytes());
    extension.extend(44u64.to_le_bytes());
    extension.extend(45u16.to_le_bytes());
    extension.extend(45u16.to_le_bytes());
    extension.extend([0; 8]);
    extension.extend(6u64.to_le_bytes());
    extension.extend(6u64.to_le_bytes());
    extension.extend(u64::from(size).to_le_bytes());
    extension.extend(u64::from(directory).to_le_bytes());
    extension.extend(0x0706_4b50u32.to_le_bytes());
    extension.extend([0; 4]);
    extension.extend((eocd as u64).to_le_bytes());
    extension.extend(1u32.to_le_bytes());
    assert_eq!(extension.len(), 76);
    let mut bytes = original[..eocd].to_vec();
    bytes.extend(&extension);
    bytes.extend_from_slice(&original[eocd..]);
    let end = bytes.len() - 22;
    for at in [8, 10] {
        put16(&mut bytes, end + at, u16::MAX);
    }
    for at in [12, 16] {
        put32(&mut bytes, end + at, u32::MAX);
    }
    let fixture = Fixture::new();
    let mut file = fixture.file(&bytes);
    let mut sink = Memory::default();
    let result: Inventory = archive::extract(&mut file, &mut sink, &mut || Ok(())).unwrap();
    assert_eq!(result.records().len(), 6);
    nix::unistd::close(file).unwrap();
    for at in [
        eocd + 4,
        eocd + 16,
        eocd + 24,
        eocd + 40,
        eocd + 64,
        eocd + 72,
    ] {
        let mut invalid = bytes.clone();
        invalid[at] ^= 1;
        refused_before_sink(&invalid);
    }
}
