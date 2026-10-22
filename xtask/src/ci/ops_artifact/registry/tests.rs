//! Tiny inert archives and private namespace faults; imported code never runs.

use super::*;
use crate::ci::command::Command;
use serde_json::{Value, json};
use std::{
    cell::Cell,
    fs,
    io::Write,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

struct NoCommands {
    calls: usize,
    stop: Option<(usize, u8)>,
}
impl Executor for NoCommands {
    async fn run(&mut self, _: Command) -> Result<Vec<u8>> {
        panic!("offline hydration must not invoke Cargo or imported code")
    }
    async fn ready(&mut self) -> Result<()> {
        self.calls += 1;
        if let Some((at, code)) = self.stop
            && self.calls >= at
        {
            return Err(failure(code));
        }
        Ok(())
    }
}
fn executor() -> NoCommands {
    NoCommands {
        calls: 0,
        stop: None,
    }
}
struct FakeClock(Cell<std::time::Instant>);
impl Clock for FakeClock {
    fn now(&self) -> std::time::Instant {
        self.0.get()
    }
}
fn clock() -> FakeClock {
    FakeClock(Cell::new(std::time::Instant::now()))
}
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    cache: PathBuf,
    output: PathBuf,
    lock: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "ops-registry-fixture-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let cache = root.join("cache");
        fs::create_dir(&cache).unwrap();
        Self {
            cache,
            output: root.join("output"),
            lock: root.join("Cargo.lock"),
            root,
        }
    }
    fn request(&self, data: &[u8]) -> Request {
        fs::write(self.cache.join("sample-1.2.3.crate"), data).unwrap();
        fs::write(&self.lock, lockfile(&sha(data))).unwrap();
        let value = json!({"schemaVersion":1,"lock":{"path":self.lock,
            "sha256":sha(&fs::read(&self.lock).unwrap())},"archivesRoot":self.cache,
            "output":self.output});
        let bytes = serde_json::to_vec(&value).unwrap();
        Request::decode(&bytes, &sha(&bytes)).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
fn lockfile(checksum: &str) -> Vec<u8> {
    format!(
        "version = 4\n[[package]]\nname='sample'\nversion='1.2.3'\n\
        source='registry+https://github.com/rust-lang/crates.io-index'\nchecksum='{checksum}'\n"
    )
    .into_bytes()
}
fn selected() -> lock::Crate {
    lock::Crate {
        name: "sample".into(),
        version: "1.2.3".into(),
        directory: "sample-1.2.3".into(),
        checksum: "a".repeat(64),
    }
}
fn manifest() -> &'static [u8] {
    b"[package]\nname='sample'\nversion='1.2.3'\n"
}
fn tar(files: &[(&str, &[u8], u32)]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, bytes, mode) in files {
        let mut header = tar::Header::new_gnu();
        header.set_mode(*mode);
        header.set_size(bytes.len() as u64);
        header.set_cksum();
        builder.append_data(&mut header, name, *bytes).unwrap();
    }
    builder.into_inner().unwrap()
}
fn raw_tar(name: &str, kind: tar::EntryType, body: &[u8]) -> Vec<u8> {
    let mut header = tar::Header::new_gnu();
    header.as_mut_bytes()[..100].fill(0);
    header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
    header.set_entry_type(kind);
    header.set_mode(0o644);
    header.set_size(body.len() as u64);
    header.set_cksum();
    let mut builder = tar::Builder::new(Vec::new());
    builder.append(&header, body).unwrap();
    builder.into_inner().unwrap()
}
fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}
fn valid() -> Vec<u8> {
    gzip(&tar(&[
        ("sample-1.2.3/Cargo.toml", manifest(), 0o644),
        (
            "sample-1.2.3/src/lib.rs",
            b"compile_error!(\"MUST NEVER EXECUTE\");",
            0o755,
        ),
    ]))
}

#[test]
fn original_request_bytes_exact_schema_private_debug_and_disjoint_paths() {
    let f = Fixture::new();
    let r = f.request(&valid());
    assert_eq!(format!("{r:?}"), "PinnedOfflineRegistryRequest");
    let mut value = json!({"schemaVersion":1,"lock":{"path":f.lock,"sha256":"a".repeat(64)},
        "archivesRoot":f.cache,"output":f.output});
    for (field, invalid) in [
        ("schemaVersion", json!(1.0)),
        ("schemaVersion", json!(true)),
        ("output", json!(f.cache.join("child"))),
        ("output", json!(f.root)),
        ("archivesRoot", json!("relative")),
        ("PRIVATE_SECRET", json!("SECRET")),
    ] {
        let mut copy = value.clone();
        copy[field] = invalid;
        let b = serde_json::to_vec(&copy).unwrap();
        assert!(Request::decode(&b, &sha(&b)).is_err());
    }
    value["lock"]["extra"] = json!(1);
    let b = serde_json::to_vec(&value).unwrap();
    assert!(Request::decode(&b, &sha(&b)).is_err());
    let b = br#"{"schemaVersion":1,"schemaVersion":1}"#;
    assert!(Request::decode(b, &sha(b)).is_err());
    assert!(Request::decode(&[b' '; 65537], &sha(&[b' '; 65537])).is_err());
}

#[test]
fn lock_mature_parser_duplicate_depth_numeric_source_checksum_and_identity_guards() {
    let valid = lockfile(&"a".repeat(64));
    assert_eq!(lock::parse(&valid).unwrap().len(), 1);
    let cases = [
        [valid.clone(), b"version=4\n".to_vec()].concat(),
        String::from_utf8(valid.clone())
            .unwrap()
            .replace("version = 4", "version = 4.0")
            .into_bytes(),
        String::from_utf8(valid.clone())
            .unwrap()
            .replace(
                "registry+https://github.com/rust-lang/crates.io-index",
                "git+https://private.invalid",
            )
            .into_bytes(),
        String::from_utf8(valid.clone())
            .unwrap()
            .replace("name='sample'", "name='../bad'")
            .into_bytes(),
        [valid.clone(), valid["version = 4\n".len()..].to_vec()].concat(),
        format!(
            "version=4\nunknown={}0{}\n",
            "[".repeat(1000),
            "]".repeat(1000)
        )
        .into_bytes(),
        format!(
            "version=4\n{}\nx=1\n",
            format_args!("[{}]", "a.".repeat(1000) + "b")
        )
        .into_bytes(),
        vec![b' '; lock::MAXIMUM as usize + 1],
    ];
    for case in cases {
        assert!(lock::parse(&case).is_err());
    }
    assert!(lock::manifest(b"[package]\nname='wrong'\nversion='1.2.3'", &selected()).is_err());
    assert!(lock::manifest(b"[package]\nname='sample'\nname='sample'", &selected()).is_err());
}

#[tokio::test]
async fn gzip_requires_single_full_crc_checked_member_with_no_hidden_tail() {
    let bytes = valid();
    let clock = clock();
    let d = Deadline::new(&clock);
    let mut ex = executor();
    assert!(
        !archive::inflate(&bytes, &mut ex, &d)
            .await
            .unwrap()
            .is_empty()
    );
    let mut crc = bytes.clone();
    let n = crc.len();
    crc[n - 8] ^= 1;
    let cases = [
        crc,
        bytes[..bytes.len() - 1].to_vec(),
        [bytes.clone(), b"hidden".to_vec()].concat(),
        [bytes.clone(), gzip(b"ignored")].concat(),
    ];
    for b in cases {
        assert!(archive::inflate(&b, &mut ex, &d).await.is_err());
    }
    let b = gzip(&[b' '; 1024]);
    assert!(
        archive::inflate_limited(&b, &mut ex, &d, 1023)
            .await
            .is_err()
    );
    assert_eq!(
        archive::inflate_limited(&b, &mut ex, &d, 1024)
            .await
            .unwrap()
            .len(),
        1024
    );
}

#[test]
fn raw_tar_alias_extensions_traversal_and_implied_parent_limits_refuse_before_writes() {
    for name in [
        "sample-1.2.3/Cargo.toml/",
        "sample-1.2.3/../Cargo.toml",
        "/sample-1.2.3/Cargo.toml",
        "sample-1.2.3//Cargo.toml",
        "sample-1.2.3/.cargo-checksum.json/child",
    ] {
        let b = raw_tar(name, tar::EntryType::Regular, manifest());
        assert!(archive::Archive::parse(b, &selected()).is_err(), "{name}");
    }
    // V7's NUL type and trailing slash must not be adopted as a regular file.
    assert!(
        archive::Archive::parse(
            raw_tar(
                "sample-1.2.3/Cargo.toml/",
                tar::EntryType::new(0),
                manifest()
            ),
            &selected()
        )
        .is_err()
    );
    for kind in [
        tar::EntryType::XHeader,
        tar::EntryType::XGlobalHeader,
        tar::EntryType::GNULongLink,
        tar::EntryType::GNULongName,
    ] {
        assert!(
            archive::Archive::parse(raw_tar("metadata", kind, &[b'x'; 8193]), &selected()).is_err()
        );
    }
    // A V7 L record is not a GNU extension to the mature normal reader and
    // must not be adopted as a regular Cargo.toml payload.
    let mut legacy = tar::Header::new_old();
    legacy.set_path("sample-1.2.3/Cargo.toml").unwrap();
    legacy.set_entry_type(tar::EntryType::GNULongName);
    legacy.set_size(manifest().len() as u64);
    legacy.set_mode(0o644);
    legacy.set_cksum();
    let mut builder = tar::Builder::new(Vec::new());
    builder.append(&legacy, manifest()).unwrap();
    assert!(archive::Archive::parse(builder.into_inner().unwrap(), &selected()).is_err());
    let mut builder = tar::Builder::new(Vec::new());
    for i in 0..4097 {
        let mut h = tar::Header::new_gnu();
        h.set_mode(0o644);
        h.set_size(0);
        h.set_cksum();
        builder
            .append_data(&mut h, format!("sample-1.2.3/d{i}/f"), &[][..])
            .unwrap();
    }
    // Only4097 wirefiles, but8195 effective nodes after implied parents.
    assert!(archive::Archive::parse(builder.into_inner().unwrap(), &selected()).is_err());
}

#[test]
fn tar_root_names_links_duplicate_conflicts_reserved_metadata_and_end_markers() {
    let good = tar(&[("sample-1.2.3/Cargo.toml", manifest(), 0o644)]);
    assert!(archive::Archive::parse(good.clone(), &selected()).is_ok());
    let mut trailing = good.clone();
    trailing.push(1);
    let mut cases = vec![trailing, good[..good.len() - 1024].to_vec()];
    for name in ["other/Cargo.toml", "sample-1.2.3/.cargo-checksum.json"] {
        cases.push(tar(&[
            ("sample-1.2.3/Cargo.toml", manifest(), 0o644),
            (name, b"private", 0o644),
        ]));
    }
    cases.push(tar(&[
        ("sample-1.2.3/Cargo.toml", manifest(), 0o644),
        ("sample-1.2.3/x", b"file", 0o644),
        ("sample-1.2.3/x/y", b"bad", 0o644),
    ]));
    cases.push(tar(&[
        ("sample-1.2.3/Cargo.toml", manifest(), 0o644),
        ("sample-1.2.3/Cargo.toml", manifest(), 0o644),
    ]));
    cases.push(tar(&[("sample-1.2.3/Cargo.toml", manifest(), 0o4644)]));
    for kind in [
        tar::EntryType::Symlink,
        tar::EntryType::Link,
        tar::EntryType::GNUSparse,
        tar::EntryType::XGlobalHeader,
        tar::EntryType::GNULongLink,
    ] {
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(kind);
        h.set_size(0);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, "sample-1.2.3/link", &[][..]).unwrap();
        cases.push(b.into_inner().unwrap());
    }
    for bytes in cases {
        assert!(archive::Archive::parse(bytes, &selected()).is_err());
    }
}

#[test]
fn tar_gnu_longname_uses_bounded_extension_then_complete_effective_path() {
    let name = format!("sample-1.2.3/{}", "long".repeat(35));
    let bytes = tar(&[
        ("sample-1.2.3/Cargo.toml", manifest(), 0o644),
        (&name, b"actual", 0o644),
    ]);
    let parsed = archive::Archive::parse(bytes, &selected()).unwrap();
    assert!(parsed.members.contains_key(&name));
}

#[tokio::test]
async fn actual_lock_archive_stored_bytes_checksums_modes_and_full_inventory_agree() {
    let f = Fixture::new();
    let r = f.request(&valid());
    let budget = Budget::default();
    let cache = Cache::open(&r.archives_root, &budget).unwrap();
    let run = OwnedRun::local(&r.output, &budget).unwrap();
    let clock = clock();
    let mut ex = executor();
    let d = Deadline::new(&clock);
    let bytes = hydrate(&r, &cache, &run, &mut ex, &d).await.unwrap();
    let receipt: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(receipt["registryPackages"], 1);
    assert_eq!(receipt["registry"]["files"], 3);
    let root = f.output.join("registry/sample-1.2.3");
    let checks: Value =
        serde_json::from_slice(&fs::read(root.join(".cargo-checksum.json")).unwrap()).unwrap();
    assert_eq!(checks["package"], sha(&valid()));
    assert_eq!(
        checks["files"]["Cargo.toml"],
        sha(&fs::read(root.join("Cargo.toml")).unwrap())
    );
    assert_eq!(
        checks["files"]["src/lib.rs"],
        sha(&fs::read(root.join("src/lib.rs")).unwrap())
    );
    assert!(checks["files"].get(".cargo-checksum.json").is_none());
    assert_eq!(
        fs::metadata(root.join("src/lib.rs"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o700
    );
    assert_eq!(
        fs::metadata(root.join("Cargo.toml"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o600
    );
    assert!(!String::from_utf8(bytes).unwrap().contains("MUST NEVER"));
    cache.finish().unwrap();
    run.finish().unwrap();
    assert_eq!(budget.count(), 0);
}

#[tokio::test]
async fn published_regular_git_payloads_are_inert_and_checksum_covered() {
    let payload = b"private inert upstream reference\n";
    let original = gzip(&tar(&[
        ("sample-1.2.3/Cargo.toml", manifest(), 0o644),
        ("sample-1.2.3/.git/refs/heads/docs", payload, 0o644),
        (
            "sample-1.2.3/.git/config",
            b"[core]\nhooksPath=NEVER_EXECUTE",
            0o644,
        ),
    ]));
    let f = Fixture::new();
    let r = f.request(&original);
    let budget = Budget::default();
    let cache = Cache::open(&r.archives_root, &budget).unwrap();
    let run = OwnedRun::local(&r.output, &budget).unwrap();
    let clock = clock();
    let mut ex = executor();
    hydrate(&r, &cache, &run, &mut ex, &Deadline::new(&clock))
        .await
        .unwrap();
    let base = f.output.join("registry/sample-1.2.3");
    assert_eq!(
        fs::read(base.join(".git/refs/heads/docs")).unwrap(),
        payload
    );
    let checksum: Value =
        serde_json::from_slice(&fs::read(base.join(".cargo-checksum.json")).unwrap()).unwrap();
    assert_eq!(checksum["files"][".git/refs/heads/docs"], sha(payload));
    assert_eq!(
        checksum["files"][".git/config"],
        sha(b"[core]\nhooksPath=NEVER_EXECUTE")
    );
    assert_eq!(checksum["package"], sha(&original));
    cache.finish().unwrap();
    run.finish().unwrap();
    assert_eq!(budget.count(), 0);
}

#[tokio::test]
async fn archive_sha_precedes_gzip_and_tar_and_failures_publish_no_receipt() {
    let f = Fixture::new();
    let r = f.request(b"not even gzip");
    let budget = Budget::default();
    // Change bytes after lock selection: refusal occurs before archive parsing.
    fs::write(f.cache.join("sample-1.2.3.crate"), b"different").unwrap();
    let cache = Cache::open(&r.archives_root, &budget).unwrap();
    let run = OwnedRun::local(&r.output, &budget).unwrap();
    let clock = clock();
    let mut ex = executor();
    assert!(
        hydrate(&r, &cache, &run, &mut ex, &Deadline::new(&clock))
            .await
            .is_err()
    );
    assert!(
        fs::read_dir(f.output.join("registry"))
            .unwrap()
            .next()
            .is_none()
    );
    assert_eq!(fs::read_dir(&f.output).unwrap().count(), 1);
    cache.finish().unwrap();
    run.finish().unwrap();
    assert_eq!(budget.count(), 0);
}

#[tokio::test]
async fn fixed_cache_nofollow_single_link_and_fresh_output_admissions() {
    for fault in 0..3 {
        let f = Fixture::new();
        let r = f.request(&valid());
        let p = f.cache.join("sample-1.2.3.crate");
        if fault == 0 {
            fs::rename(&p, f.root.join("actual")).unwrap();
            symlink(f.root.join("actual"), &p).unwrap();
        }
        if fault == 1 {
            fs::hard_link(&p, f.root.join("alias")).unwrap();
        }
        if fault == 2 {
            fs::remove_file(&p).unwrap();
            fs::create_dir(&p).unwrap();
        }
        let budget = Budget::default();
        let cache = Cache::open(&r.archives_root, &budget).unwrap();
        let clock = clock();
        let mut ex = executor();
        assert!(
            cache
                .archive(&selected(), &mut ex, &Deadline::new(&clock))
                .await
                .is_err()
        );
        cache.finish().unwrap();
        assert_eq!(budget.count(), 0);
    }
    let f = Fixture::new();
    fs::create_dir(&f.output).unwrap();
    assert!(OwnedRun::local(&f.output, &Budget::default()).is_err());
}

#[tokio::test]
async fn output_rehash_membership_modes_and_known_signal_deadline_fences() {
    for fault in 0..3 {
        let f = Fixture::new();
        let budget = Budget::default();
        let run = OwnedRun::local(&f.output, &budget).unwrap();
        let mut output = output::Output::new(&run).unwrap();
        output.directory("sample-1.2.3").unwrap();
        output
            .write("sample-1.2.3/file", b"original", false)
            .unwrap();
        let p = f.output.join("registry/sample-1.2.3/file");
        if fault == 0 {
            fs::write(&p, b"modified").unwrap();
        }
        if fault == 1 {
            fs::write(p.with_file_name("extra"), b"unexpected").unwrap();
        }
        if fault == 2 {
            fs::set_permissions(&p, fs::Permissions::from_mode(0o666)).unwrap();
        }
        let clock = clock();
        let mut ex = executor();
        assert!(
            output
                .measure(&run, &mut ex, &Deadline::new(&clock))
                .await
                .is_err()
        );
        let _ = output.finish(&run);
        run.finish().unwrap();
        assert_eq!(budget.count(), 0);
    }
    for code in [130, 143, 124] {
        let f = Fixture::new();
        let r = f.request(&valid());
        let budget = Budget::default();
        let cache = Cache::open(&r.archives_root, &budget).unwrap();
        let run = OwnedRun::local(&r.output, &budget).unwrap();
        let clock = clock();
        let mut ex = executor();
        ex.stop = Some((4, code));
        assert_eq!(
            hydrate(&r, &cache, &run, &mut ex, &Deadline::new(&clock))
                .await
                .unwrap_err()
                .code,
            code
        );
        cache.finish().unwrap();
        run.finish().unwrap();
        assert_eq!(budget.count(), 0);
    }
    let clock = clock();
    let d = Deadline::new(&clock);
    clock
        .0
        .set(clock.now() + std::time::Duration::from_secs(1200));
    assert_eq!(
        archive::inflate(&valid(), &mut executor(), &d)
            .await
            .unwrap_err()
            .code,
        124
    );
}
