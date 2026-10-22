//! Owned tar fixtures and the retained Python artifact reader as an oracle.

use super::*;
use std::io::Cursor;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "simplestchat-artifact-reader-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        DirBuilder::new().mode(0o700).create(&root).unwrap();
        Self(root)
    }
    fn archive(&self, entries: &[(&str, u8, &[u8])]) -> PathBuf {
        let path = self.0.join("generator-results.tar");
        let mut builder = tar::Builder::new(File::create(&path).unwrap());
        for (name, kind, contents) in entries {
            let mut header = tar::Header::new_ustar();
            assert!(name.len() < 100);
            header.as_mut_bytes()[..100].fill(0);
            header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
            header.set_entry_type(tar::EntryType::new(*kind));
            header.set_size(contents.len() as u64);
            header.set_mode(0o7777);
            header.set_uid(4321);
            header.set_gid(9876);
            if b"12".contains(kind) {
                header.set_link_name("PRIVATE target").unwrap();
            }
            header.set_cksum();
            builder.append(&header, Cursor::new(contents)).unwrap();
        }
        builder.finish().unwrap();
        path
    }
}

fn pax(key: &str, value: &str) -> Vec<u8> {
    let payload = format!(" {key}={value}\n");
    let mut length = payload.len() + 1;
    loop {
        let record = format!("{length}{payload}");
        if record.len() == length {
            return record.into_bytes();
        }
        length = record.len();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn regular_results_use_only_private_host_names_and_digest_the_exact_contents() {
    let fixture = Fixture::new();
    let source = fixture.archive(&[
        ("./load_test_summary.json", b'0', b"{}"),
        (RESULTS, 0, b"[1,2]"),
    ]);
    let destination = fixture.0.join("generator");
    let manifest = extract(&source, &destination).unwrap();
    assert_eq!(manifest["complete"], true);
    assert_eq!(manifest["schemaVersion"], 1);
    assert_eq!(
        manifest["files"][SUMMARY]["sha256"],
        hex(&Sha256::digest(b"{}"))
    );
    assert_eq!(manifest["files"][RESULTS]["bytes"], 5);
    assert_eq!(
        read_regular(&destination.join(SUMMARY), MAX_SUMMARY).unwrap(),
        b"{}"
    );
    for path in [
        destination,
        fixture.0.join("generator").join(SUMMARY),
        fixture.0.join("generator").join(RESULTS),
    ] {
        assert_eq!(
            fs::metadata(&path).unwrap().mode() & 0o7777,
            if path.is_dir() { 0o700 } else { 0o600 }
        );
    }
}

#[test]
fn all_metadata_is_validated_before_any_result_can_be_materialized() {
    let fixtures = [
        ("../PRIVATE.json", b'0'),
        ("/PRIVATE.json", b'0'),
        ("generator.log", b'0'),
        ("step.json", b'0'),
        ("generator/nested.json", b'0'),
        ("././load_test_summary.json", b'0'),
        (SUMMARY, b'1'),
        (SUMMARY, b'2'),
        (SUMMARY, b'3'),
        (SUMMARY, b'6'),
    ];
    for (name, kind) in fixtures {
        let fixture = Fixture::new();
        let source = fixture.archive(&[(RESULTS, b'0', b"{}"), (name, kind, b"{}")]);
        let destination = fixture.0.join("generator");
        let error = extract(&source, &destination).unwrap_err();
        assert!(!error.to_string().contains("PRIVATE"));
        assert_eq!(fs::read_dir(destination).unwrap().count(), 0);
    }
    for entries in [
        vec![(RESULTS, b'0', &b"{}"[..])],
        vec![
            (RESULTS, b'0', &b"{}"[..]),
            (SUMMARY, b'0', &b"{}"[..]),
            (TIMEOUT, b'0', &b"{}"[..]),
        ],
        vec![
            (SUMMARY, b'0', &b"{}"[..]),
            ("./load_test_summary.json", b'0', &b"{}"[..]),
        ],
    ] {
        let fixture = Fixture::new();
        let source = fixture.archive(&entries);
        let destination = fixture.0.join("generator");
        assert!(extract(&source, &destination).is_err());
        assert_eq!(fs::read_dir(destination).unwrap().count(), 0);
    }
}

#[test]
fn root_directory_and_pax_metadata_are_optional_but_cannot_redirect_output() {
    for directory in [".", "./"] {
        let fixture = Fixture::new();
        let source = fixture.archive(&[
            (directory, b'5', b""),
            (SUMMARY, b'0', b"{}"),
            (RESULTS, b'0', b"[]"),
        ]);
        extract(&source, &fixture.0.join("generator")).unwrap();
    }
    let fixture = Fixture::new();
    let metadata = pax("path", "../PRIVATE payload.json");
    let source = fixture.archive(&[
        ("metadata", b'x', &metadata),
        (SUMMARY, b'0', b"{}"),
        (RESULTS, b'0', b"[]"),
    ]);
    let destination = fixture.0.join("generator");
    assert!(extract(&source, &destination).is_err());
    assert_eq!(fs::read_dir(destination).unwrap().count(), 0);
}

#[test]
fn ordinary_pax_fields_match_the_allowlist_and_ambiguous_fields_are_refused() {
    let fixture = Fixture::new();
    let metadata = [pax("mtime", "123.456"), pax("path", SUMMARY)].concat();
    let source = fixture.archive(&[
        ("metadata", b'x', &metadata),
        ("replaced-name", b'0', b"{}"),
        (RESULTS, b'0', b"[]"),
    ]);
    extract(&source, &fixture.0.join("generator")).unwrap();
    for metadata in [
        [pax("path", SUMMARY), pax("path", "../PRIVATE.json")].concat(),
        pax("GNU.sparse.map", "0,2"),
    ] {
        let fixture = Fixture::new();
        let source = fixture.archive(&[
            ("metadata", b'x', &metadata),
            (SUMMARY, b'0', b"{}"),
            (RESULTS, b'0', b"[]"),
        ]);
        let destination = fixture.0.join("generator");
        assert_eq!(
            extract(&source, &destination).unwrap_err().to_string(),
            "unsafe_result_entry"
        );
        assert_eq!(fs::read_dir(destination).unwrap().count(), 0);
    }
    let duplicate = Fixture::new();
    let source = duplicate.archive(&[(".", b'5', b""), ("./", b'5', b"")]);
    assert_eq!(
        extract(&source, &duplicate.0.join("generator"))
            .unwrap_err()
            .to_string(),
        "duplicate_result_entry"
    );
}

#[test]
fn physical_header_body_and_terminal_limits_precede_general_parsing() {
    let fixture = Fixture::new();
    let source = fixture.archive(&[("metadata", b'x', &vec![0; MAX_TAR_METADATA as usize + 1])]);
    assert_eq!(
        check_headers(&mut File::open(&source).unwrap(), 8, MAX_RESULT)
            .unwrap_err()
            .to_string(),
        "oversized_tar_body"
    );
    let source = fixture.archive(&[(SUMMARY, b'0', b"{}")]);
    assert_eq!(
        check_headers(&mut File::open(&source).unwrap(), 0, MAX_RESULT)
            .unwrap_err()
            .to_string(),
        "too_many_tar_headers"
    );
    assert_eq!(
        check_headers(&mut File::open(&source).unwrap(), 8, 1)
            .unwrap_err()
            .to_string(),
        "oversized_tar_body"
    );
    let path = fixture.0.join("broken.tar");
    fs::write(&path, [vec![0; 1024], b"PRIVATE".to_vec()].concat()).unwrap();
    assert_eq!(
        check_headers(&mut File::open(&path).unwrap(), 8, MAX_RESULT)
            .unwrap_err()
            .to_string(),
        "trailing_tar_data"
    );
    fs::write(&path, vec![0; 512]).unwrap();
    assert_eq!(
        check_headers(&mut File::open(&path).unwrap(), 8, MAX_RESULT)
            .unwrap_err()
            .to_string(),
        "missing_tar_end_marker"
    );
    fs::write(&path, vec![0; 511]).unwrap();
    assert_eq!(
        check_headers(&mut File::open(&path).unwrap(), 8, MAX_RESULT)
            .unwrap_err()
            .to_string(),
        "truncated_tar_header"
    );
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(MAX_ARCHIVE + 1)
        .unwrap();
    assert_eq!(
        check_headers(&mut File::open(&path).unwrap(), 8, MAX_RESULT)
            .unwrap_err()
            .to_string(),
        "result_archive_too_large"
    );
    let source = fixture.archive(&[(SUMMARY, b'0', b"{}")]);
    File::options()
        .write(true)
        .open(&source)
        .unwrap()
        .set_len(513)
        .unwrap();
    assert_eq!(
        check_headers(&mut File::open(source).unwrap(), 8, MAX_RESULT)
            .unwrap_err()
            .to_string(),
        "truncated_tar_body"
    );
}

#[test]
fn linked_nonregular_large_and_existing_inputs_or_destinations_are_refused() {
    let fixture = Fixture::new();
    let source = fixture.archive(&[(SUMMARY, b'0', b"{}"), (RESULTS, b'0', b"[]")]);
    let link = fixture.0.join("linked");
    symlink(&source, &link).unwrap();
    assert!(read_regular(&link, MAX_RESULT).is_err());
    assert!(read_regular(&fixture.0, MAX_RESULT).is_err());
    assert!(read_regular(&source, 1).is_err());
    assert!(extract(&link, &fixture.0.join("not-created")).is_err());
    let destination = fixture.0.join("generator");
    fs::create_dir(&destination).unwrap();
    assert!(extract(&source, &destination).is_err());
    assert!(new_file(&source, b"never replace").is_err());
    assert!(new_file(&link, b"never follow").is_err());
}

#[test]
fn successful_collection_publishes_its_manifest_last_and_removes_only_its_archive() {
    let fixture = Fixture::new();
    let source = fixture.archive(&[(SUMMARY, b'0', b"{}"), (RESULTS, b'0', b"[]")]);
    let size = fs::metadata(&source).unwrap().len();
    let digest = hex(&Sha256::digest(fs::read(&source).unwrap()));
    let manifest = finish_collection(&source, &fixture.0).unwrap();
    assert_eq!(manifest["archiveBytes"], size);
    assert_eq!(manifest["archiveSha256"], digest);
    assert!(!source.exists());
    let stored: Value = serde_json::from_slice(
        &read_regular(&fixture.0.join("collection.json"), MAX_SUMMARY).unwrap(),
    )
    .unwrap();
    assert_eq!(stored, manifest);
    assert_eq!(
        fs::metadata(fixture.0.join("collection.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let failed = Fixture::new();
    let source = failed.archive(&[(RESULTS, b'0', b"[]")]);
    assert!(finish_collection(&source, &failed.0).is_err());
    assert!(source.exists());
    assert!(!failed.0.join("collection.json").exists());
}

#[test]
fn ordinary_tar_manifests_match_the_python_reference() {
    let fixture = Fixture::new();
    let source = fixture.archive(&[
        ("./", b'5', b""),
        (SUMMARY, b'0', b"{}"),
        (RESULTS, b'0', b"[1,2]"),
    ]);
    let destination = fixture.0.join("native");
    let native = extract(&source, &destination).unwrap();
    let reference = Command::new("python3").args(["-c", "import json,sys; from pathlib import Path; sys.path[:0]=[sys.argv[1],sys.argv[2]];import capacity_artifacts;print(json.dumps(capacity_artifacts.extract(Path(sys.argv[3]),Path(sys.argv[4]))))"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("build"))
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("ops/ansible/files"))
        .arg(&source).arg(fixture.0.join("python")).output().unwrap();
    assert!(
        reference.status.success(),
        "Python oracle fixture failed: {}",
        String::from_utf8_lossy(&reference.stderr)
    );
    assert_eq!(
        native,
        serde_json::from_slice::<Value>(&reference.stdout).unwrap()
    );
}

#[test]
fn maximum_pax_logical_size_is_refused_before_reading_or_writing_bodies() {
    let fixture = Fixture::new();
    let source = fixture.archive(&[
        ("metadata", b'x', &pax("size", "18446744073709551615")),
        (SUMMARY, b'0', b"{}"),
        (RESULTS, b'0', b"[]"),
    ]);
    let destination = fixture.0.join("generator");
    assert_eq!(
        extract(&source, &destination).unwrap_err().to_string(),
        "invalid_result_archive"
    );
    assert_eq!(fs::read_dir(destination).unwrap().count(), 0);
    assert!(!fixture.0.join("collection.json").exists());
}

#[test]
fn benchmark_results_publish_fixed_root_outputs_and_retain_failed_archives() {
    let fixture = Fixture::new();
    let source = fixture.archive(&[(SUMMARY, b'0', b"{}"), (RESULTS, b'0', b"[]")]);
    let manifest = finish_benchmark_results(&source, &fixture.0).unwrap();
    assert_eq!(manifest["complete"], true);
    assert_eq!(fs::read(fixture.0.join(SUMMARY)).unwrap(), b"{}");
    assert_eq!(fs::read(fixture.0.join(RESULTS)).unwrap(), b"[]");
    assert!(!fixture.0.join("generator").exists());
    assert!(!source.exists());

    let failed = Fixture::new();
    let source = failed.archive(&[
        (SUMMARY, b'0', b"{}"),
        (RESULTS, b'0', b"[]"),
        (TIMEOUT, b'0', b"{}"),
    ]);
    assert_eq!(
        finish_benchmark_results(&source, &failed.0)
            .unwrap_err()
            .to_string(),
        "generator_timeout_marker"
    );
    assert!(source.exists());
    assert!(!failed.0.join(SUMMARY).exists());
    assert!(!failed.0.join(RESULTS).exists());
    assert!(!failed.0.join("collection.json").exists());
}

#[test]
fn handshake_collection_accepts_only_one_bounded_regular_fixed_basename() {
    let fixture = Fixture::new();
    let source = fixture.archive(&[("./handshakes.pcap", b'0', b"pcap fixture")]);
    let manifest = finish_handshake_capture(&source, &fixture.0).unwrap();
    assert_eq!(
        manifest["files"][CAPTURE]["sha256"],
        hex(&Sha256::digest(b"pcap fixture"))
    );
    assert_eq!(fs::read(fixture.0.join(CAPTURE)).unwrap(), b"pcap fixture");
    assert!(fixture.0.join("capture-collection.json").exists());
    assert!(!source.exists());
    assert_eq!(
        fs::metadata(fixture.0.join(CAPTURE)).unwrap().mode() & 0o7777,
        0o600
    );

    for (name, kind) in [
        (SUMMARY, b'0'),
        ("../handshakes.pcap", b'0'),
        (CAPTURE, b'2'),
        (CAPTURE, b'1'),
        (CAPTURE, b'5'),
    ] {
        let failed = Fixture::new();
        let source = failed.archive(&[(name, kind, b"fixture")]);
        assert!(finish_handshake_capture(&source, &failed.0).is_err());
        assert!(source.exists());
        assert!(!failed.0.join(CAPTURE).exists());
        assert!(!failed.0.join("capture-collection.json").exists());
    }
    let failed = Fixture::new();
    let source = failed.archive(&[(CAPTURE, b'0', b"a"), (CAPTURE, b'0', b"b")]);
    assert!(finish_handshake_capture(&source, &failed.0).is_err());
    assert!(!failed.0.join(CAPTURE).exists());
}

#[test]
fn handshake_size_limits_and_existing_or_linked_destinations_refuse_publication() {
    let fixture = Fixture::new();
    let source = fixture.archive(&[(CAPTURE, b'0', b"fixture")]);
    File::options()
        .write(true)
        .open(&source)
        .unwrap()
        .set_len(65 * 1024 * 1024 + 1)
        .unwrap();
    assert_eq!(
        finish_handshake_capture(&source, &fixture.0)
            .unwrap_err()
            .to_string(),
        "result_archive_too_large"
    );
    assert!(!fixture.0.join(CAPTURE).exists());

    // Reject the declared body ceiling before any allocation or body read.
    let mut oversized = tar::Header::new_ustar();
    oversized.set_path(CAPTURE).unwrap();
    oversized.set_entry_type(tar::EntryType::Regular);
    oversized.set_size(64 * 1024 * 1024 + 1);
    oversized.set_mode(0o600);
    oversized.set_cksum();
    fs::write(&source, oversized.as_bytes()).unwrap();
    assert_eq!(
        finish_handshake_capture(&source, &fixture.0)
            .unwrap_err()
            .to_string(),
        "oversized_tar_body"
    );
    assert!(!fixture.0.join(CAPTURE).exists());

    let source = fixture.archive(&[(CAPTURE, b'0', b"fixture")]);
    let original = fixture.0.join("original");
    fs::write(&original, b"preserve").unwrap();
    symlink(&original, fixture.0.join(CAPTURE)).unwrap();
    assert!(finish_handshake_capture(&source, &fixture.0).is_err());
    assert_eq!(fs::read(original).unwrap(), b"preserve");
    assert!(source.exists());
    assert!(!fixture.0.join("capture-collection.json").exists());
}

#[test]
fn directory_replacement_cannot_publish_outside_the_validated_results() {
    let fixture = Fixture::new();
    let run = fixture.0.join("run");
    let moved = fixture.0.join("moved");
    let outside = fixture.0.join("outside");
    fs::create_dir(&run).unwrap();
    fs::create_dir(&outside).unwrap();
    let original = fixture.archive(&[(CAPTURE, b'0', &vec![42; 64 * 1024 * 1024])]);
    let archive = run.join("handshakes.tar");
    fs::rename(original, &archive).unwrap();
    // Create the alias before validation begins: creating it later would change
    // the source inode ctime and correctly reject the archive as mutated.
    fs::hard_link(&archive, outside.join("handshakes.tar")).unwrap();
    let run_for_swap = run.clone();
    let moved_for_swap = moved.clone();
    let outside_for_swap = outside.clone();
    let swapped = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if fs::metadata(run_for_swap.join(CAPTURE))
                .is_ok_and(|metadata| metadata.len() == 64 * 1024 * 1024)
            {
                fs::rename(&run_for_swap, &moved_for_swap).unwrap();
                symlink(&outside_for_swap, &run_for_swap).unwrap();
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "output was not created"
            );
            std::thread::yield_now();
        }
    });
    let result = finish_handshake_capture(&archive, &run);
    swapped.join().unwrap();
    assert!(
        result.is_err(),
        "a replaced output root was accepted: {result:?}"
    );
    assert!(!outside.join("capture-collection.json").exists());
}

#[test]
fn result_mutation_cannot_publish_a_stale_successful_manifest() {
    let fixture = Fixture::new();
    let source = fixture.archive(&[(CAPTURE, b'0', &vec![42; 64 * 1024 * 1024])]);
    let output = fixture.0.join(CAPTURE);
    let output_for_mutation = output.clone();
    let mutation = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if fs::metadata(&output_for_mutation)
                .is_ok_and(|metadata| metadata.len() == 64 * 1024 * 1024)
            {
                fs::write(&output_for_mutation, b"replaced result").unwrap();
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "output was not created"
            );
            std::thread::yield_now();
        }
    });
    let result = finish_handshake_capture(&source, &fixture.0);
    mutation.join().unwrap();
    assert!(
        result.is_err(),
        "a changed extracted output was accepted: {result:?}"
    );
    assert!(!fixture.0.join("capture-collection.json").exists());
}
