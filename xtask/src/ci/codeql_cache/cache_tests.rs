//! Owned ZIP/filesystem fixtures and fake CodeQL commands; no scan or publication service.

use super::*;
use serde_json::json;
use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    root: PathBuf,
    python: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "simplestchat-codeql-cache-{}-{} λ space",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let root = path.canonicalize().unwrap();
        let python = Command::new("python3")
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            .unwrap();
        assert!(python.status.success());
        Self {
            root,
            python: PathBuf::from(String::from_utf8(python.stdout).unwrap().trim()),
        }
    }
    fn write(&self, name: &str, bytes: impl AsRef<[u8]>) -> PathBuf {
        let path = self.root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }
    fn context(&self) -> Context {
        Context {
            root: self.root.clone(),
            environment: BTreeMap::from([
                ("PATH".into(), std::env::var("PATH").unwrap()),
                ("HOME".into(), self.root.to_string_lossy().into_owned()),
                ("ImageOS".into(), "ubuntu24".into()),
                ("ImageVersion".into(), "20261001.1".into()),
                ("GITHUB_REF".into(), "refs/heads/main".into()),
                ("GITHUB_EVENT_NAME".into(), "push".into()),
                ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
                ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
                ("GIT_CONFIG_SYSTEM".into(), "/dev/null".into()),
            ]),
            system: "Linux".into(),
            architecture: "x86_64".into(),
        }
    }
    fn python(&self, program: &str, arguments: &[&Path]) -> Vec<u8> {
        let output = Command::new(&self.python)
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .args(["-c", program])
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .unwrap()
                    .join("build"),
            )
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }
    fn git(&self, arguments: &[&str]) {
        let context = self.context();
        let status = Command::new("git")
            .current_dir(&self.root)
            .env_clear()
            .envs(context.environment)
            .args(arguments)
            .status()
            .unwrap();
        assert!(status.success());
    }
    fn tracked(&self) {
        self.write("src/main.rs", "fn main() {}\n");
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        for name in [
            "security/codeql-toolchain.json",
            ".github/workflows/codeql.yml",
        ] {
            self.write(name, fs::read(repository.join(name)).unwrap());
        }
        self.git(&["init", "-q"]);
        self.git(&["add", "."]);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(pid) = fs::read_to_string(self.root.join("child.pid")) {
            let _ = Command::new("/bin/kill")
                .args(["-KILL", "--", pid.trim()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn zip(path: &Path, members: &[(&str, &[u8])]) {
    let mut archive = rawzip::ZipArchiveWriter::new(File::create(path).unwrap());
    for (name, bytes) in members {
        let (mut entry, config) = archive
            .new_file(*name)
            .compression_method(rawzip::CompressionMethod::DEFLATE)
            .start()
            .unwrap();
        let encoder =
            flate2::write::DeflateEncoder::new(&mut entry, flate2::Compression::default());
        let mut writer = config.wrap(encoder);
        writer.write_all(bytes).unwrap();
        let (encoder, descriptor) = writer.finish().unwrap();
        encoder.finish().unwrap();
        entry.finish(descriptor).unwrap();
    }
    archive.finish().unwrap();
}

fn source_fixture(
    fixture: &Fixture,
    language: &str,
) -> (PathBuf, PathBuf, serde_json::Map<String, Value>) {
    let database = fixture.root.join(format!("database-{language}"));
    fs::create_dir(&database).unwrap();
    let source = fixture.root.join("source");
    fs::create_dir_all(&source).unwrap();
    fs::write(
        database.join("codeql-database.yml"),
        format!("sourceLocationPrefix: {}", source.display()),
    )
    .unwrap();
    let names = if language == "rust" {
        vec!["src/main.rs"]
    } else {
        vec![
            "vendor/mediasoup-sys-0.19.0/src/RTC/DtlsTransport.cpp",
            "vendor/mediasoup-sys-0.19.0/src/RTC/ICE/StunPacket.cpp",
            "vendor/mediasoup-sys-0.19.0/src/RTC/SCTP/association/Association.cpp",
            "vendor/mediasoup-sys-0.19.0/src/RTC/RTP/Packet.cpp",
        ]
    };
    let manifest = names
        .iter()
        .map(|name| ((*name).into(), json!(io::sha256(name.as_bytes()))))
        .collect();
    let members = names
        .iter()
        .map(|name| {
            (
                format!(
                    "{}/{name}",
                    source.display().to_string().trim_start_matches('/')
                ),
                name.as_bytes(),
            )
        })
        .collect::<Vec<_>>();
    zip(
        &database.join("src.zip"),
        &members
            .iter()
            .map(|(name, bytes)| (name.as_str(), *bytes))
            .collect::<Vec<_>>(),
    );
    (database, source, manifest)
}

#[test]
fn source_validation_binds_root_protocol_coverage_and_exact_bytes() {
    let fixture = Fixture::new();
    for language in ["c-cpp", "rust"] {
        let (database, source, mut manifest) = source_fixture(&fixture, language);
        archive::source_integrity(&database, &source, &manifest, language).unwrap();
        assert!(
            archive::source_integrity(&database, &source.join("different"), &manifest, language)
                .is_err()
        );
        manifest
            .values_mut()
            .next()
            .map(|value| *value = json!("0".repeat(64)))
            .unwrap();
        assert_eq!(
            archive::source_integrity(&database, &source, &manifest, language)
                .unwrap_err()
                .to_string(),
            "codeql_cache_source_changed"
        );
        zip(
            &database.join("src.zip"),
            &[("external/header.h", b"generated")],
        );
        assert_eq!(
            archive::source_integrity(&database, &source, &manifest, language)
                .unwrap_err()
                .to_string(),
            "codeql_cache_source_coverage"
        );
    }
}

#[test]
fn source_validation_rejects_unkeyed_frontend_policy_and_tracked_bytes() {
    let fixture = Fixture::new();
    let (database, source, mut manifest) = source_fixture(&fixture, "rust");
    let main = format!(
        "{}/src/main.rs",
        source.display().to_string().trim_start_matches('/')
    );
    for name in crate::ci::inputs::CODEQL_POLICY_DATA
        .iter()
        .copied()
        .chain([
            "web/index.ts",
            "README.md",
            "docs/example.md",
            "web/package.json",
            "build/unrelated.py",
        ])
    {
        let path = format!(
            "{}/{name}",
            source.display().to_string().trim_start_matches('/')
        );
        manifest.insert(name.into(), json!(io::sha256(b"unkeyed")));
        zip(
            &database.join("src.zip"),
            &[(&main, b"src/main.rs"), (&path, b"unkeyed")],
        );
        assert_eq!(
            archive::source_integrity(&database, &source, &manifest, "rust")
                .unwrap_err()
                .to_string(),
            "codeql_cache_unkeyed_source",
            "{name}"
        );
    }
}

#[test]
fn rust_embedded_inputs_are_checked_and_native_generated_inputs_remain_allowed() {
    let fixture = Fixture::new();
    let (database, source, mut manifest) = source_fixture(&fixture, "rust");
    let names = [
        "src/main.rs",
        "ops/Cargo.toml",
        "ops/src/json.rs",
        "ops/src/release/manifest.rs",
        "web/new_target/helper.rs",
        "web/tests/shared.json",
        "migrations/001.sql",
        "security/authorization/operations.json",
        "vendor/seclists-passwords/10k-most-common.txt",
    ];
    let paths = names
        .iter()
        .map(|name| {
            format!(
                "{}/{name}",
                source.display().to_string().trim_start_matches('/')
            )
        })
        .collect::<Vec<_>>();
    for name in names {
        manifest.insert(name.into(), json!(io::sha256(name.as_bytes())));
    }
    zip(
        &database.join("src.zip"),
        &paths
            .iter()
            .zip(names)
            .map(|(path, name)| (path.as_str(), name.as_bytes()))
            .collect::<Vec<_>>(),
    );
    archive::source_integrity(&database, &source, &manifest, "rust").unwrap();
    for name in [
        "migrations/001.sql",
        "ops/src/release/manifest.rs",
        "ops/Cargo.toml",
    ] {
        let expected = manifest.insert(name.into(), json!("0".repeat(64))).unwrap();
        assert!(archive::source_integrity(&database, &source, &manifest, "rust").is_err());
        manifest.insert(name.into(), expected);
    }
    let (database, source, manifest) = source_fixture(&fixture, "c-cpp");
    let mut all = manifest
        .keys()
        .map(|name| {
            (
                format!(
                    "{}/{name}",
                    source.display().to_string().trim_start_matches('/')
                ),
                name.as_bytes(),
            )
        })
        .collect::<Vec<_>>();
    all.push((
        format!(
            "{}/target/generated.cpp",
            source.display().to_string().trim_start_matches('/')
        ),
        b"generated",
    ));
    all.push(("usr/include/stdlib.h".into(), b"external"));
    zip(
        &database.join("src.zip"),
        &all.iter()
            .map(|(name, data)| (name.as_str(), *data))
            .collect::<Vec<_>>(),
    );
    archive::source_integrity(&database, &source, &manifest, "c-cpp").unwrap();
}

#[test]
fn bundle_needs_language_bound_query_results_and_never_cached_sarif() {
    let fixture = Fixture::new();
    let bundle = fixture.root.join("database.zip");
    zip(
        &bundle,
        &[
            ("rust/results/security.bqrs", b"results"),
            ("rust/log/diagnostics.bqrs", b"diagnostics"),
        ],
    );
    archive::bundle_integrity(&bundle, "rust").unwrap();
    assert!(archive::bundle_integrity(&bundle, "c-cpp").is_err());
    zip(&bundle, &[("rust/codeql-database.yml", b"metadata")]);
    assert_eq!(
        archive::bundle_integrity(&bundle, "rust")
            .unwrap_err()
            .to_string(),
        "codeql_cache_query_results_missing"
    );
    zip(
        &bundle,
        &[
            ("rust/results/security.bqrs", b"results"),
            ("rust/results/old.sarif", b"stale"),
        ],
    );
    assert_eq!(
        archive::bundle_integrity(&bundle, "rust")
            .unwrap_err()
            .to_string(),
        "codeql_cache_bundle_content"
    );
}

#[derive(Default)]
struct FakeDriver {
    calls: Vec<(String, Vec<String>)>,
    missing: bool,
    fail: bool,
}
impl cache::Driver for FakeDriver {
    fn run(&mut self, label: &str, arguments: &[String]) -> Result<()> {
        self.calls.push((label.into(), arguments.to_vec()));
        require(!self.fail, "fixture_command_failed")?;
        if label == "codeql-cache-bundle" {
            let output = arguments
                .iter()
                .find_map(|arg| arg.strip_prefix("--output="))
                .unwrap();
            let language = Path::new(arguments.last().unwrap())
                .file_name()
                .unwrap()
                .to_str()
                .unwrap();
            let metadata = format!("{language}/codeql-database.yml");
            let result = format!("{language}/results/security.bqrs");
            let members = if self.missing {
                vec![(metadata.as_str(), b"identity".as_slice())]
            } else {
                vec![
                    (metadata.as_str(), b"identity".as_slice()),
                    (result.as_str(), b"queries".as_slice()),
                ]
            };
            zip(Path::new(output), &members);
        }
        Ok(())
    }
}

#[test]
fn save_restore_preserves_queries_revision_private_modes_and_exact_commands() {
    let fixture = Fixture::new();
    for language in ["rust", "c-cpp"] {
        let directory = fixture.root.join(language).join("cache");
        let mut driver = FakeDriver::default();
        let receipt = cache::save(
            &mut driver,
            &fixture.root,
            &directory,
            "exact-key",
            &"a".repeat(40),
            language,
        )
        .unwrap();
        assert_eq!(receipt["reused"], false);
        assert_eq!(directory.metadata().unwrap().mode() & 0o077, 0);
        assert_eq!(
            directory.join("receipt.json").metadata().unwrap().mode() & 0o777,
            0o600
        );
        let database = fixture.root.join("restored").join(language);
        let restored = cache::restore(&mut driver, &directory, "exact-key", &database)
            .unwrap()
            .unwrap();
        assert_eq!(restored["revision"], "a".repeat(40));
        assert_eq!(restored["reused"], true);
        assert_eq!(restored["bundleSha256"], receipt["bundleSha256"]);
        assert_eq!(
            driver
                .calls
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["codeql-cache-bundle", "codeql-cache-unbundle"]
        );
        for flag in [
            "--include-results",
            "--include-diagnostics",
            "--no-include-logs",
            "--cache-cleanup=clear",
        ] {
            assert!(driver.calls[0].1.iter().any(|arg| arg == flag));
        }
        assert_eq!(
            driver.calls[0].1.last().unwrap(),
            fixture
                .root
                .join("databases")
                .join(language)
                .to_str()
                .unwrap()
        );
        assert_eq!(
            driver.calls[1].1,
            [
                "database",
                "unbundle",
                &format!("--name={language}"),
                &format!("--target={}", database.parent().unwrap().display()),
                directory.join("database.zip").to_str().unwrap()
            ]
        );
        assert_eq!(directory.read_dir().unwrap().count(), 2);
    }
    assert!(!fixture.root.read_dir().unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".codeql-analyzed-")
    }));
}

#[test]
fn failed_or_incomplete_bundle_cannot_publish_and_existing_cache_is_untouched() {
    let fixture = Fixture::new();
    let directory = fixture.root.join("cache");
    for (missing, fail) in [(true, false), (false, true)] {
        let mut driver = FakeDriver {
            missing,
            fail,
            ..FakeDriver::default()
        };
        assert!(
            cache::save(
                &mut driver,
                &fixture.root,
                &directory,
                "key",
                &"a".repeat(40),
                "rust"
            )
            .is_err()
        );
        assert!(!directory.exists());
        assert_eq!(fixture.root.read_dir().unwrap().count(), 0);
    }
    fs::create_dir(&directory).unwrap();
    fixture.write("cache/keep", "original");
    let mut driver = FakeDriver::default();
    assert!(
        cache::save(
            &mut driver,
            &fixture.root,
            &directory,
            "key",
            &"a".repeat(40),
            "rust"
        )
        .is_err()
    );
    assert!(driver.calls.is_empty());
    assert_eq!(fs::read(directory.join("keep")).unwrap(), b"original");
}

#[test]
fn restore_rejects_tampering_wrong_keys_partial_inventory_and_symlinks_before_command() {
    let fixture = Fixture::new();
    let database = fixture.root.join("rust");
    let directory = fixture.root.join("cache");
    let mut driver = FakeDriver::default();
    assert!(
        cache::restore(&mut driver, &directory, "key", &database)
            .unwrap()
            .is_none()
    );
    cache::save(
        &mut driver,
        &fixture.root,
        &directory,
        "key",
        &"a".repeat(40),
        "rust",
    )
    .unwrap();
    driver.calls.clear();
    assert!(cache::restore(&mut driver, &directory, "wrong", &database).is_err());
    let receipt_path = directory.join("receipt.json");
    let original = fs::read(&receipt_path).unwrap();
    for (field, value) in [
        ("bundleBytes", json!(true)),
        ("bundleBytes", json!(1.0)),
        ("bundleBytes", json!(0)),
        ("revision", json!("bad")),
        ("extra", json!(true)),
        ("bundleSha256", json!("0".repeat(64))),
    ] {
        let mut receipt: Value = serde_json::from_slice(&original).unwrap();
        receipt[field] = value;
        fs::write(&receipt_path, serde_json::to_vec(&receipt).unwrap()).unwrap();
        assert!(
            cache::restore(&mut driver, &directory, "key", &database).is_err(),
            "{field}"
        );
    }
    fs::write(&receipt_path, &original).unwrap();
    let bundle = directory.join("database.zip");
    let bytes = fs::read(&bundle).unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(&bundle)
        .unwrap()
        .write_all(b"changed")
        .unwrap();
    assert!(cache::restore(&mut driver, &directory, "key", &database).is_err());
    fs::write(&bundle, bytes).unwrap();
    fixture.write("cache/unexpected", "stale");
    assert!(cache::restore(&mut driver, &directory, "key", &database).is_err());
    fs::remove_file(directory.join("unexpected")).unwrap();
    fs::rename(&bundle, directory.join("real.zip")).unwrap();
    symlink(directory.join("real.zip"), &bundle).unwrap();
    assert!(cache::restore(&mut driver, &directory, "key", &database).is_err());
    assert!(driver.calls.is_empty());
}

#[test]
fn openssl_identity_matches_reference_and_rejects_links() {
    let fixture = Fixture::new();
    for name in [
        "include/openssl/ssl.h",
        "lib/libssl.a",
        "lib/libcrypto.a",
        "share/simplestchat/receipt.json",
    ] {
        fixture.write(name, name);
    }
    let native = key::openssl_identity(&fixture.root).unwrap();
    let expected = fixture.python("import sys;from pathlib import Path;sys.path.insert(0,sys.argv[1]);import security_codeql_cache as c;print(c.openssl_identity(Path(sys.argv[2])))",&[&fixture.root]);
    assert_eq!(native, String::from_utf8(expected).unwrap().trim());
    symlink(
        fixture.root.join("lib/libssl.a"),
        fixture.root.join("include/alias"),
    )
    .unwrap();
    assert!(key::openssl_identity(&fixture.root).is_err());
}

#[test]
fn full_rust_keys_match_reference_and_change_for_source_suite_scope_and_paths() {
    let fixture = Fixture::new();
    fixture.tracked();
    let context = fixture.context();
    let source = fixture.root.join("source");
    let native = key::key(&context, &source, None, "rust", "security", false).unwrap();
    let program = r"
import json,os,sys
from pathlib import Path
from unittest.mock import patch
sys.path.insert(0,sys.argv[1])
import security_codeql_cache as c
with patch.dict(os.environ,json.loads(sys.argv[3]),clear=True),patch('platform.system',return_value='Linux'),patch('platform.machine',return_value='x86_64'):
    print(c.cache_key(Path(sys.argv[2]),Path(sys.argv[2])/'source',None,'rust','security'))
";
    let output = Command::new(&fixture.python)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .args(["-c", program])
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .join("build"),
        )
        .arg(&fixture.root)
        .arg(serde_json::to_string(&context.environment).unwrap())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(native, String::from_utf8(output.stdout).unwrap().trim());
    assert_ne!(
        native,
        key::key(&context, &source, None, "rust", "all", false).unwrap()
    );
    assert_ne!(
        native,
        key::key(&context, &source, None, "rust", "security", true).unwrap()
    );
    assert_ne!(
        native,
        key::key(
            &context,
            &source.join("other"),
            None,
            "rust",
            "security",
            false
        )
        .unwrap()
    );
    fixture.write("src/main.rs", "fn main() { println!(\"changed\"); }\n");
    assert_ne!(
        native,
        key::key(&context, &source, None, "rust", "security", false).unwrap()
    );
}

#[test]
fn archive_path_encoding_alias_and_size_decisions_match_python() {
    let fixture = Fixture::new();
    let program = r"
import json,pathlib,stat,struct,sys,warnings,zipfile
sys.path.insert(0,sys.argv[1])
import security_codeql_cache as reference
root=pathlib.Path(sys.argv[2]); report=[]
cases=[('valid',['rust/results/a.bqrs']),('directory',['rust/','rust/results/a.bqrs']),
    ('unicode',['rust/results/é.bqrs']),('relative',['../rust/results/a.bqrs']),
    ('absolute',['/rust/results/a.bqrs']),('double',['rust//results/a.bqrs']),
    ('dot',['rust/./results/a.bqrs']),('parent',['rust/x/../results/a.bqrs']),
    ('backslash',['rust\\results/a.bqrs']),('control',['rust/results/a\x01.bqrs']),
    ('same',['rust/results/a.bqrs','rust/results/a.bqrs']),
    ('file-directory',['rust/results/a.bqrs','rust/results/a.bqrs/']),
    ('symlink',['rust/results/a.bqrs']),('fifo',['rust/results/a.bqrs']),
    ('empty',[]),('wrong-language',['other/results/a.bqrs']),
    ('no-results',['rust/database.yml']),('sarif',['rust/results/a.bqrs','rust/results/a.sarif']),
    ('cp437-distinct',['rust/results/aa.bqrs','rust/results/é.bqrs']),
    ('cp437-collision',['rust/results/aa.bqrs','rust/results/├⌐.bqrs']),
    ('unicode-extra',['rust/results/a.bqrs']),('huge-declared',['rust/results/a.bqrs','rust/results/b.bqrs'])]
for label,names in cases:
    path=root/(label+'.zip')
    with warnings.catch_warnings(),zipfile.ZipFile(path,'w') as archive:
        warnings.simplefilter('ignore',UserWarning)
        for name in names:
            item=zipfile.ZipInfo(name)
            if label in ('symlink','fifo'): item.external_attr=(stat.S_IFLNK if label=='symlink' else stat.S_IFIFO)<<16
            if label=='unicode-extra':
                import zlib
                field=b'\x01'+struct.pack('<I',zlib.crc32(name.encode()))+b'../ignored-unicode-path'
                item.extra=struct.pack('<HH',0x7075,len(field))+field
            archive.writestr(item,b'fixture')
    data=path.read_bytes()
    if label.startswith('cp437'): data=data.replace(b'/aa.bqrs',b'/\xc3\xa9.bqrs')
    if label=='huge-declared':
        data=bytearray(data); offset=data.index(b'PK\x01\x02'); struct.pack_into('<I',data,offset+24,0xffffffff)
    path.write_bytes(data)
    try: reference.bundle_integrity(path,'rust'); valid=True
    except (reference.ToolError,ValueError,zipfile.BadZipFile): valid=False
    report.append([label,valid])
print(json.dumps(report))
";
    let reports: Vec<(String, bool)> =
        serde_json::from_slice(&fixture.python(program, &[&fixture.root])).unwrap();
    for (label, expected) in reports {
        // Python before 3.14 ignores UnicodePath; all maintained readers reject
        // ambiguous unsafe overrides consistently, including on those hosts.
        if label == "unicode-extra" {
            assert!(
                archive::bundle_integrity(&fixture.root.join("unicode-extra.zip"), "rust").is_err()
            );
            continue;
        }
        assert_eq!(
            archive::bundle_integrity(&fixture.root.join(format!("{label}.zip")), "rust").is_ok(),
            expected,
            "{label}"
        );
    }
}

#[test]
fn only_stored_and_deflated_sources_are_read_back_other_metadata_is_safe_to_inspect() {
    let fixture = Fixture::new();
    let program = r"
import pathlib,sys,zipfile
root=pathlib.Path(sys.argv[2])
methods=[('stored',zipfile.ZIP_STORED),('deflated',zipfile.ZIP_DEFLATED),
    ('bzip2',zipfile.ZIP_BZIP2),('lzma',zipfile.ZIP_LZMA)]
if hasattr(zipfile,'ZIP_ZSTANDARD'): methods.append(('zstd',zipfile.ZIP_ZSTANDARD))
for name,method in methods:
    database=root/name;database.mkdir()
    (database/'codeql-database.yml').write_text('sourceLocationPrefix: /rust')
    with zipfile.ZipFile(database/'src.zip','w',compression=method) as archive:
        archive.writestr('rust/src/main.rs',b'fn main() {}')
        archive.writestr('rust/results/source.bqrs',b'evaluated query')
";
    fixture.python(program, &[&fixture.root]);
    let manifest =
        serde_json::Map::from_iter([("src/main.rs".into(), json!(io::sha256(b"fn main() {}")))]);
    for entry in fixture.root.read_dir().unwrap() {
        let path = entry.unwrap().path();
        archive::bundle_integrity(&path.join("src.zip"), "rust").unwrap();
        let result = archive::source_integrity(&path, Path::new("/rust"), &manifest, "rust");
        if matches!(
            path.file_name().unwrap().to_str().unwrap(),
            "stored" | "deflated"
        ) {
            result.unwrap();
        } else {
            assert_eq!(
                result.unwrap_err().to_string(),
                "codeql_cache_source_compression"
            );
        }
    }
}

#[test]
fn command_timeout_kills_owned_descendant_even_after_leader_exit() {
    let fixture = Fixture::new();
    let pidfile = fixture.root.join("child.pid");
    let script=fixture.write("fake slow analyzer",format!("#!{}\nimport pathlib,subprocess,sys\nchild=subprocess.Popen([sys.executable,'-c','import time;time.sleep(60)'])\npathlib.Path({}).write_text(str(child.pid))\n",
        fixture.python.display(),serde_json::to_string(pidfile.to_str().unwrap()).unwrap()));
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let error = process::capture(
        &fixture.root,
        &fixture.context().environment,
        script.to_str().unwrap(),
        &[],
        1,
        1024,
        1024,
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "codeql_cache_command_timeout");
    let pid = fs::read_to_string(pidfile).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let status = Command::new("/bin/ps")
            .args(["-p", pid.trim(), "-o", "stat="])
            .output()
            .unwrap();
        if !status.status.success()
            || String::from_utf8_lossy(&status.stdout)
                .trim()
                .starts_with('Z')
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "owned descendant stopped"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn expansion_count_and_regular_file_bounds_fail_before_any_analyzer() {
    let fixture = Fixture::new();
    let program = r"
import pathlib,sys,zipfile
root=pathlib.Path(sys.argv[2])
with zipfile.ZipFile(root/'too-many.zip','w') as archive:
    for i in range(100001): archive.writestr('rust/results/'+str(i)+'.bqrs',b'')
with zipfile.ZipFile(root/'source-too-large.zip','w',compression=zipfile.ZIP_DEFLATED) as archive:
    archive.writestr('source/src/main.rs',b'x'*(16*1024**2+1))
";
    fixture.python(program, &[&fixture.root]);
    let database = fixture.root.join("large-database");
    fs::create_dir(&database).unwrap();
    fs::write(
        database.join("codeql-database.yml"),
        "sourceLocationPrefix: /source",
    )
    .unwrap();
    fs::rename(
        fixture.root.join("source-too-large.zip"),
        database.join("src.zip"),
    )
    .unwrap();
    let manifest = serde_json::Map::from_iter([("src/main.rs".into(), json!("0".repeat(64)))]);
    assert_eq!(
        archive::source_integrity(&database, Path::new("/source"), &manifest, "rust")
            .unwrap_err()
            .to_string(),
        "codeql_cache_source_size"
    );
    assert_eq!(
        archive::bundle_integrity(&fixture.root.join("too-many.zip"), "rust")
            .unwrap_err()
            .to_string(),
        "codeql_cache_archive_members"
    );
    let huge = fixture.write("sparse.zip", []);
    File::options()
        .write(true)
        .open(&huge)
        .unwrap()
        .set_len(archive::MAX_BUNDLE + 1)
        .unwrap();
    assert!(archive::bundle_integrity(&huge, "rust").is_err());
    symlink(
        fixture.root.join("too-many.zip"),
        fixture.root.join("alias.zip"),
    )
    .unwrap();
    assert!(archive::bundle_integrity(&fixture.root.join("alias.zip"), "rust").is_err());
}

#[test]
fn actual_command_boundary_is_private_bounded_and_preserves_literal_arguments() {
    let fixture = Fixture::new();
    let log = fixture.root.join("command.json");
    let script = fixture.write("fake analyzer",format!("#!{}\nimport json,os,pathlib,sys\npathlib.Path({}).write_text(json.dumps({{'args':sys.argv[1:],'tokenPresent':'GH_TOKEN' in os.environ,'home':os.environ['HOME']}}))\nprint('private analyzer output')\n",
        fixture.python.display(),serde_json::to_string(log.to_str().unwrap()).unwrap()));
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let mut context = fixture.context();
    context
        .environment
        .insert("GH_TOKEN".into(), "must-remain-private".into());
    let options = Options {
        operation: "save".into(),
        values: BTreeMap::from([
            ("output".into(), fixture.root.to_string_lossy().into_owned()),
            ("codeql".into(), script.to_string_lossy().into_owned()),
        ]),
        include_vendor: false,
    };
    let mut driver = Driver::new(&context, &options).unwrap();
    cache::Driver::run(
        &mut driver,
        "fixture",
        &[
            "database".into(),
            "argument with spaces; $(not-a-shell)".into(),
        ],
    )
    .unwrap();
    let seen: Value = serde_json::from_slice(&fs::read(log).unwrap()).unwrap();
    assert_eq!(
        seen["args"],
        json!(["database", "argument with spaces; $(not-a-shell)"])
    );
    assert_eq!(seen["tokenPresent"], false);
    assert_eq!(seen["home"], fixture.root.join("home").to_str().unwrap());
    for (body, limit) in [
        ("import sys;sys.stdout.write('x'*1025)", 1024),
        (
            "import sys;sys.stderr.write('secret'*1025);sys.exit(7)",
            1024,
        ),
    ] {
        fs::write(&script, format!("#!{}\n{body}\n", fixture.python.display())).unwrap();
        let error = process::capture(
            &fixture.root,
            &context.environment,
            script.to_str().unwrap(),
            &[],
            2,
            limit,
            limit,
        )
        .unwrap_err();
        assert!(!format!("{error:#}").contains("secret"));
    }
}

#[test]
fn native_identity_framing_matches_python_and_every_native_component_binds() {
    let fixture = Fixture::new();
    let identity = json!({"inputs":"native-inputs","language":"c-cpp","suite":"security",
        "includeVendor":false,"sourceRoot":fixture.root.join("source").to_str().unwrap(),
        "toolchain":{"pin":"fixture λ"},"opensslRoot":fixture.root.join("openssl").to_str().unwrap(),
        "opensslSha256":"openssl","packagesSha256":io::sha256(b"packages"),
        "compilerSha256":{"gcc":"compiler","g++":"compiler"}});
    let expected = fixture.python(r"
import sys
from pathlib import Path
from unittest.mock import patch
sys.path.insert(0,sys.argv[1])
import security_codeql_cache as c
with patch.object(c.ci_verified,'cache_key',return_value='native-inputs'),patch.object(c.ci_verified,'cache_namespace',return_value='main-linux-x86_64'),patch.object(c,'command',return_value=b'packages'),patch.object(c,'executable',return_value='/usr/bin/dpkg-query'),patch.object(c.ci_verified,'digest',return_value='compiler'),patch.object(c,'openssl_identity',return_value='openssl'),patch.object(c.tools,'pin',return_value={'pin':'fixture λ'}):
    print(c.cache_key(Path(sys.argv[2]),Path(sys.argv[2])/'source',Path(sys.argv[2])/'openssl','c-cpp','security'))
",&[&fixture.root]);
    let native = key::identity_key(&identity, "main-linux-x86_64", "c-cpp");
    assert_eq!(native, String::from_utf8(expected).unwrap().trim());
    for field in identity.as_object().unwrap().keys() {
        let mut changed = identity.clone();
        changed[field] = json!("changed");
        assert_ne!(
            native,
            key::identity_key(&changed, "main-linux-x86_64", "c-cpp"),
            "{field}"
        );
    }
    assert_ne!(
        native,
        key::identity_key(&identity, "untrusted-linux-x86_64", "c-cpp")
    );
}

#[test]
fn native_and_python_receipts_restore_each_others_genuine_bundles() {
    let fixture = Fixture::new();
    for language in ["rust", "c-cpp"] {
        let root = fixture.root.join(language);
        fs::create_dir(&root).unwrap();
        let native_cache = root.join("native-cache");
        let mut driver = FakeDriver::default();
        let saved = cache::save(
            &mut driver,
            &root,
            &native_cache,
            "native-key",
            &"a".repeat(40),
            language,
        )
        .unwrap();
        let program = r"
import json,pathlib,sys,zipfile
from unittest.mock import patch
sys.path.insert(0,sys.argv[1])
import security_codeql_cache as c
from security_context import Context,ROOT
root=pathlib.Path(sys.argv[2]); language=root.name
output=root/'python-output';output.mkdir();context=Context(ROOT,output)
def run(label,argv,**kwargs):
    if label=='codeql-cache-bundle':
        path=pathlib.Path(next(value.removeprefix('--output=') for value in argv if value.startswith('--output=')))
        with zipfile.ZipFile(path,'w',compression=zipfile.ZIP_DEFLATED) as archive:
            archive.writestr(language+'/codeql-database.yml',b'identity')
            archive.writestr(language+'/results/security.bqrs',b'queries')
    return 0,b''
with patch.object(context,'run',side_effect=run):
    restored=c.restore(context,pathlib.Path('/fake-codeql'),root/'native-cache','native-key',root/'restored'/language)
    saved=c.save(context,pathlib.Path('/fake-codeql'),root/'python-cache','python-key','b'*40,language=language)
print(json.dumps({'restored':restored,'saved':saved}))
";
        let output: Value = serde_json::from_slice(&fixture.python(program, &[&root])).unwrap();
        assert_eq!(output["restored"]["bundleSha256"], saved["bundleSha256"]);
        assert_eq!(output["restored"]["reused"], true);
        let restored = cache::restore(
            &mut driver,
            &root.join("python-cache"),
            "python-key",
            &root.join("restored").join(language),
        )
        .unwrap()
        .unwrap();
        assert_eq!(restored["bundleSha256"], output["saved"]["bundleSha256"]);
        assert_eq!(restored["revision"], "b".repeat(40));
        assert_eq!(restored["reused"], true);
    }
}

#[test]
fn source_header_identity_offsets_crc_and_self_extractor_prefix_are_checked() {
    let fixture = Fixture::new();
    let (database, source, manifest) = source_fixture(&fixture, "rust");
    let path = database.join("src.zip");
    let original = fs::read(&path).unwrap();
    let mut preamble = b"self extractor preamble\n".to_vec();
    preamble.extend_from_slice(&original);
    fs::write(&path, preamble).unwrap();
    archive::source_integrity(&database, &source, &manifest, "rust").unwrap();
    let mut changed = original.clone();
    changed[30] = if changed[30] == b'x' { b'y' } else { b'x' };
    fs::write(&path, changed).unwrap();
    assert_eq!(
        archive::source_integrity(&database, &source, &manifest, "rust")
            .unwrap_err()
            .to_string(),
        "codeql_cache_source_header"
    );
    let mut changed = original.clone();
    let central = changed
        .windows(4)
        .position(|bytes| bytes == b"PK\x01\x02")
        .unwrap();
    changed[central + 20..central + 24].copy_from_slice(&1024_u32.to_le_bytes());
    fs::write(&path, changed).unwrap();
    assert_eq!(
        archive::source_integrity(&database, &source, &manifest, "rust")
            .unwrap_err()
            .to_string(),
        "codeql_cache_source_overlap"
    );
    let mut changed = original;
    changed[central + 16..central + 20].copy_from_slice(&0_u32.to_le_bytes());
    fs::write(&path, changed).unwrap();
    assert!(archive::source_integrity(&database, &source, &manifest, "rust").is_err());
}

#[test]
fn cli_requires_exact_fields_and_preserves_local_vendor_opt_in() {
    let fixture = Fixture::new();
    for args in [
        vec![],
        vec!["key"],
        vec!["key", "--language", "python", "--source-root", "/source"],
        vec!["validate-bundle", "--language", "rust"],
        vec!["save", "--unknown", "value"],
    ] {
        assert!(options(&args.into_iter().map(str::to_owned).collect::<Vec<_>>()).is_none());
    }
    let arguments = ["key", "--language", "rust", "--source-root", "/source"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert!(options(&arguments).is_some());
    let context = fixture.context();
    require_scope(&context, "rust", false).unwrap();
    require_scope(&context, "c-cpp", true).unwrap();
    assert!(require_scope(&context, "c-cpp", false).is_err());
    for marker in ["CI", "GITHUB_ACTIONS", "ACT"] {
        let mut context = fixture.context();
        context.environment.insert(marker.into(), String::new());
        assert!(require_scope(&context, "rust", true).is_err());
        assert!(require_scope(&context, "c-cpp", true).is_err());
        require_scope(&context, "rust", false).unwrap();
    }
}

#[test]
fn failed_command_cannot_make_cleanup_remove_replacement_staging() {
    struct Replaced {
        path: Option<PathBuf>,
    }
    impl cache::Driver for Replaced {
        fn run(&mut self, _label: &str, arguments: &[String]) -> Result<()> {
            let path = Path::new(
                arguments
                    .iter()
                    .find_map(|arg| arg.strip_prefix("--output="))
                    .unwrap(),
            )
            .parent()
            .unwrap();
            fs::rename(path, path.with_extension("original"))?;
            fs::create_dir(path)?;
            fs::write(path.join("other-owner"), "untouched")?;
            self.path = Some(path.to_path_buf());
            anyhow::bail!("fixture_command_failed")
        }
    }
    let fixture = Fixture::new();
    let mut driver = Replaced { path: None };
    assert!(
        cache::save(
            &mut driver,
            &fixture.root,
            &fixture.root.join("cache"),
            "key",
            &"a".repeat(40),
            "rust"
        )
        .is_err()
    );
    assert_eq!(
        fs::read(driver.path.unwrap().join("other-owner")).unwrap(),
        b"untouched"
    );
    assert!(!fixture.root.join("cache").exists());
}
