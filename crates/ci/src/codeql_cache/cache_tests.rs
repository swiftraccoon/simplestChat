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
        Self { root }
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
    /// A `#!/bin/sh` fake named `name` with `body`, and its path. The root
    /// holds no single quote, so a body may quote any path below it.
    fn script(&self, name: &str, body: &str) -> PathBuf {
        assert!(!self.root.to_str().unwrap().contains('\''));
        let script = self.write(
            name,
            format!("#!/bin/sh\n{}{body}\n", crate::fake_shell::PRELUDE),
        );
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        script
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
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap();
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

/// What the deleted Python cache module answered for these tests' fixtures,
/// recorded from it before it was removed.
fn reference() -> Value {
    serde_json::from_str(include_str!("recorded_reference.json")).unwrap()
}

/// Write a ZIP archive at `path` with each member's name, method and bytes,
/// in order. A deflated member is compressed; a member of any other method
/// carries its bytes unchanged under that method's number, so it is only
/// read correctly by a reader that refuses the method before decompressing.
fn archive(path: &Path, members: &[(String, rawzip::CompressionMethod, Vec<u8>)]) {
    let output = std::io::BufWriter::new(File::create(path).unwrap());
    let mut archive = rawzip::ZipArchiveWriter::new(output);
    for (name, method, bytes) in members {
        let (mut entry, config) = archive
            .new_file(name.as_str())
            .compression_method(*method)
            .start()
            .unwrap();
        let descriptor = if *method == rawzip::CompressionMethod::DEFLATE {
            let encoder =
                flate2::write::DeflateEncoder::new(&mut entry, flate2::Compression::default());
            let mut writer = config.wrap(encoder);
            writer.write_all(bytes).unwrap();
            let (encoder, descriptor) = writer.finish().unwrap();
            encoder.finish().unwrap();
            descriptor
        } else {
            let mut writer = config.wrap(&mut entry);
            writer.write_all(bytes).unwrap();
            writer.finish().unwrap().1
        };
        entry.finish(descriptor).unwrap();
    }
    archive.finish().unwrap().into_inner().unwrap();
}

/// The bytes a recording spells in hexadecimal.
fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
        .collect()
}

/// Write the recorded tree `tree` below `root`: a text, hexadecimal bytes
/// with their mode, or a link to another name below `root`.
fn planted(root: &Path, tree: &Value) {
    for (name, spec) in tree.as_object().unwrap() {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        if let Some(target) = spec["link"].as_str() {
            symlink(root.join(target), &path).unwrap();
            continue;
        }
        match spec["text"].as_str() {
            Some(text) => fs::write(&path, text).unwrap(),
            None => fs::write(&path, unhex(spec["hex"].as_str().unwrap())).unwrap(),
        }
        if let Some(mode) = spec["mode"].as_u64() {
            let mode = u32::try_from(mode).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        }
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
    for name in crate::inputs::CODEQL_POLICY_DATA.iter().copied().chain([
        "web/index.ts",
        "README.md",
        "docs/example.md",
        "web/package.json",
        "build/unrelated.py",
    ]) {
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
    // The reference's prefix (four files, each holding its own name) and the
    // same prefix with a link to one of them.
    let mut tally = [0; 2];
    for case in reference()["opensslIdentities"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let fixture = Fixture::new();
        planted(&fixture.root, &case["tree"]);
        let native = key::openssl_identity(&fixture.root);
        if let Some(held) = case["result"]["held"].as_str() {
            assert_eq!(native.unwrap(), held, "{name}");
            tally[0] += 1;
        } else {
            let refused = case["result"]["refused"].as_str().unwrap();
            assert_eq!(native.unwrap_err().to_string(), refused, "{name}");
            tally[1] += 1;
        }
    }
    assert_eq!(tally, [1, 1]);
}

#[test]
fn full_rust_keys_match_reference_and_change_for_source_suite_scope_and_paths() {
    // Each recorded checkout is tracked here as the recorder tracked it, and
    // keyed at the recorded source path in the recorded environment.
    let mut keyed = 0;
    for case in reference()["rustKeys"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let fixture = Fixture::new();
        planted(&fixture.root, &case["tree"]);
        fixture.git(&["init", "-q"]);
        fixture.git(&["add", "."]);
        let mut context = fixture.context();
        for (variable, value) in case["environment"].as_object().unwrap() {
            let value = value.as_str().unwrap().to_owned();
            context.environment.insert(variable.clone(), value);
        }
        assert_eq!(context.system, case["system"].as_str().unwrap());
        assert_eq!(context.architecture, case["machine"].as_str().unwrap());
        let source = Path::new(case["source"].as_str().unwrap());
        let suite = case["suite"].as_str().unwrap();
        let vendor = case["includeVendor"].as_bool().unwrap();
        let native = key::key(&context, source, None, "rust", suite, vendor).unwrap();
        assert_eq!(native, case["result"]["held"].as_str().unwrap(), "{name}");
        keyed += 1;
    }
    assert_eq!(keyed, 5);
    let fixture = Fixture::new();
    fixture.tracked();
    let context = fixture.context();
    let source = fixture.root.join("source");
    let native = key::key(&context, &source, None, "rust", "security", false).unwrap();
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
fn archive_path_encoding_alias_and_size_decisions_match_the_reference() {
    // The reference's own archives, as it wrote them and judged them. Its
    // interpreter refused the ambiguous Unicode path override, so every
    // maintained reader is held to the same verdict.
    let fixture = Fixture::new();
    let mut tally = [0; 2];
    for case in reference()["archives"].as_array().unwrap() {
        let label = case["name"].as_str().unwrap();
        let bundle = fixture.write(
            &format!("{label}.zip"),
            unhex(case["hex"].as_str().unwrap()),
        );
        let valid = case["valid"].as_bool().unwrap();
        assert_eq!(
            archive::bundle_integrity(&bundle, "rust").is_ok(),
            valid,
            "{label}"
        );
        tally[usize::from(valid)] += 1;
    }
    assert_eq!(tally, [18, 4]);
}

#[test]
fn only_stored_and_deflated_sources_are_read_back_other_metadata_is_safe_to_inspect() {
    let fixture = Fixture::new();
    use rawzip::CompressionMethod as Method;
    for (name, method) in [
        ("stored", Method::STORE),
        ("deflated", Method::DEFLATE),
        ("bzip2", Method::BZIP2),
        ("lzma", Method::LZMA),
        ("zstd", Method::ZSTD),
    ] {
        let database = fixture.root.join(name);
        fs::create_dir(&database).unwrap();
        fs::write(
            database.join("codeql-database.yml"),
            "sourceLocationPrefix: /rust",
        )
        .unwrap();
        archive(
            &database.join("src.zip"),
            &[
                ("rust/src/main.rs".into(), method, b"fn main() {}".to_vec()),
                (
                    "rust/results/source.bqrs".into(),
                    method,
                    b"evaluated query".to_vec(),
                ),
            ],
        );
    }
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
    let script = fixture.script(
        "fake slow analyzer",
        &format!(
            "/bin/sleep 60 &\nprintf '%s' \"$!\" >'{}'",
            pidfile.display()
        ),
    );
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
    let members = (0..100_001)
        .map(|index| {
            (
                format!("rust/results/{index}.bqrs"),
                rawzip::CompressionMethod::STORE,
                Vec::new(),
            )
        })
        .collect::<Vec<_>>();
    archive(&fixture.root.join("too-many.zip"), &members);
    archive(
        &fixture.root.join("source-too-large.zip"),
        &[(
            "source/src/main.rs".into(),
            rawzip::CompressionMethod::DEFLATE,
            vec![b'x'; 16 * 1024 * 1024 + 1],
        )],
    );
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
    let script = fixture.script(
        "fake analyzer",
        &format!(
            concat!(
                "home=${{HOME?}}\n",
                "if [ \"${{GH_TOKEN+set}}\" = set ]; then present=true; else present=false; fi\n",
                "{{ printf '{{\"args\": '; json_strings \"$@\"\n",
                "  printf ', \"tokenPresent\": %s, \"home\": ' \"$present\"; json_string \"$home\"\n",
                "  printf '}}'; }} >'{}'\n",
                "printf 'private analyzer output\\n'"
            ),
            log.display()
        ),
    );
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
        ("repeat x 1025", 1024),
        ("repeat secret 1025 >&2\nexit 7", 1024),
    ] {
        fixture.script("fake analyzer", body);
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
fn native_identity_framing_matches_the_reference_and_every_native_component_binds() {
    // Each identity is what the reference made of the inputs it was given in
    // place of its source closure, namespace, packages, compilers, OpenSSL
    // and pin.
    let identity = |case: &Value| {
        let compiler = &case["compilerSha256"];
        json!({"inputs":case["inputs"],"language":case["language"],"suite":case["suite"],
            "includeVendor":false,"sourceRoot":case["sourceRoot"],
            "toolchain":case["toolchain"],"opensslRoot":case["opensslRoot"],
            "opensslSha256":case["opensslSha256"],
            "packagesSha256":io::sha256(case["packages"].as_str().unwrap().as_bytes()),
            "compilerSha256":{"gcc":compiler,"g++":compiler}})
    };
    let cases = reference()["identities"].as_array().unwrap().clone();
    for case in &cases {
        let namespace = case["namespace"].as_str().unwrap();
        assert_eq!(
            key::identity_key(&identity(case), namespace, "c-cpp"),
            case["result"]["held"].as_str().unwrap(),
            "{}",
            case["name"]
        );
    }
    assert_eq!(cases.len(), 3);
    let identity = identity(&cases[0]);
    let native = key::identity_key(&identity, "main-linux-x86_64", "c-cpp");
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
fn native_and_reference_receipts_restore_each_others_genuine_bundles() {
    let fixture = Fixture::new();
    let reference = reference();
    let mut languages = Vec::new();
    for case in reference["receipts"].as_array().unwrap() {
        let language = case["language"].as_str().unwrap();
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
        // This save made, byte for byte, the cache the reference restored.
        let recorded = case["rustCache"].as_object().unwrap();
        assert_eq!(native_cache.read_dir().unwrap().count(), recorded.len());
        for (name, file) in recorded {
            assert_eq!(
                fs::read(native_cache.join(name)).unwrap(),
                unhex(file["hex"].as_str().unwrap()),
                "{language} {name}"
            );
        }
        let restored = &case["restored"]["held"];
        assert_eq!(restored["bundleSha256"], saved["bundleSha256"]);
        assert_eq!(restored["reused"], true);
        // The reference's own cache, as it saved it, restores here.
        let python_cache = root.join("python-cache");
        planted(&python_cache, &case["pythonCache"]);
        let restored = cache::restore(
            &mut driver,
            &python_cache,
            "python-key",
            &root.join("restored").join(language),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            restored["bundleSha256"],
            case["saved"]["held"]["bundleSha256"]
        );
        assert_eq!(restored["revision"], "b".repeat(40));
        assert_eq!(restored["reused"], true);
        languages.push(language);
    }
    assert_eq!(languages, ["rust", "c-cpp"]);
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
