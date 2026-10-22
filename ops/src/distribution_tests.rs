//! Inert ELF metadata and owned filesystem fixtures; candidates are never run.

use super::*;
use crate::test_support::Fixture;
use std::{fs, os::unix::fs::symlink};

#[path = "distribution_fixture.rs"]
mod fixture;
use fixture::*;

fn inspect_bytes(bytes: &[u8]) -> Result<ExecutableIdentity, Failure> {
    let directory = Fixture::new();
    let path = directory.0.join("PRIVATE-SENTINEL");
    fs::write(&path, bytes).unwrap();
    inspect(&path)
}

#[test]
fn selected_identity_refuses_noncanonical_digests_and_sizes() {
    for digest in ["", "A", &"A".repeat(64), &"f".repeat(63)] {
        assert!(SelectedExecutable::new(digest, 1, Architecture::X86_64).is_err());
    }
    for size in [0, MAX_BYTES + 1] {
        assert!(SelectedExecutable::new(&"a".repeat(64), size, Architecture::X86_64).is_err());
    }
}

#[test]
fn static_and_static_pie_linkage_facts_are_separate_from_provenance() {
    for dynamic in [false, true] {
        let bytes = fixture(dynamic, false, false);
        let identity = inspect_bytes(&bytes).unwrap();
        assert!(identity.no_interpreter);
        assert!(identity.no_needed);
        assert_eq!(identity.needed_count, 0);
        assert_eq!(identity.pie_flag, dynamic);
        assert_eq!(identity.bind_now, dynamic);
        assert!(identity.non_executable_stack);
        assert!(identity.no_writable_executable_load);
        assert!(identity.relro_present);
        assert_eq!(identity.architecture, Architecture::X86_64);
        assert_eq!(identity.bytes, bytes.len() as u64);
        let output = serde_json::to_string(&identity).unwrap();
        assert!(!output.contains("musl"));
        assert!(!output.contains("compatible"));
    }
}

#[test]
fn original_digest_includes_unused_private_and_trailing_bytes_and_selected_binding() {
    let directory = Fixture::new();
    let path = directory.0.join("candidate");
    let mut bytes = fixture(true, true, true);
    bytes.extend_from_slice(b"PRIVATE-SENTINEL-overlay");
    fs::write(&path, &bytes).unwrap();
    let identity = inspect(&path).unwrap();
    let expected: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(identity.sha256, expected);
    assert!(!identity.no_interpreter);
    assert!(!identity.no_needed);
    assert_eq!(identity.needed_count, 1);
    let selected =
        SelectedExecutable::new(&expected, bytes.len() as u64, Architecture::X86_64).unwrap();
    assert_eq!(verify(&path, &selected).unwrap(), identity);
    for selected in [
        SelectedExecutable::new(&"0".repeat(64), bytes.len() as u64, Architecture::X86_64).unwrap(),
        SelectedExecutable::new(&expected, bytes.len() as u64 + 1, Architecture::X86_64).unwrap(),
    ] {
        assert_eq!(verify(&path, &selected), Err(Failure::Identity));
    }
    assert!(
        !serde_json::to_string(&identity)
            .unwrap()
            .contains("PRIVATE-SENTINEL")
    );
}

#[test]
fn malformed_or_unsupported_headers_and_table_budgets_are_refused() {
    for (offset, value) in [(4, 1), (5, 2), (6, 0), (7, 255)] {
        let mut bytes = fixture(false, false, false);
        bytes[offset] = value;
        assert!(inspect_bytes(&bytes).is_err(), "header byte {offset}");
    }
    for (offset, value) in [
        (16, 1),
        (18, 183),
        (52, 63),
        (54, 55),
        (56, 0),
        (56, 8193),
        (56, 65535),
    ] {
        let mut bytes = fixture(false, false, false);
        u16_at(&mut bytes, offset, value);
        assert!(
            inspect_bytes(&bytes).is_err(),
            "header word {offset}={value}"
        );
    }
    for offset in [24, 32] {
        let mut bytes = fixture(false, false, false);
        u64_at(&mut bytes, offset, u64::MAX);
        assert!(inspect_bytes(&bytes).is_err());
    }
    for size in [0, 1, 63, 64, 343] {
        assert!(inspect_bytes(&fixture(false, false, false)[..size]).is_err());
    }
}

#[test]
fn malformed_and_ambiguous_segments_cannot_prove_empty_linkage() {
    for (offset, value) in [
        (64 + 8, u64::MAX),
        (64 + 16, u64::MAX),
        (64 + 32, 4097),
        (64 + 40, 4095),
        (64 + 48, 3),
    ] {
        let mut bytes = fixture(false, false, false);
        u64_at(&mut bytes, offset, value);
        assert!(inspect_bytes(&bytes).is_err());
    }
    for kind in [2, 3, 0x6474e551] {
        let mut bytes = fixture(true, true, true);
        program(&mut bytes, 4, kind, 4, DYNAMIC, 64);
        assert!(inspect_bytes(&bytes).is_err(), "duplicate {kind}");
    }
    for size in [0, 1, 131088] {
        let mut bytes = fixture(true, false, false);
        program(&mut bytes, 2, 2, 4, DYNAMIC, size);
        assert!(inspect_bytes(&bytes).is_err());
    }
    let mut bytes = fixture(true, true, false);
    bytes[INTERPRETER] = 0;
    assert!(inspect_bytes(&bytes).is_err());
}

#[test]
fn dynamic_termination_duplicates_strings_and_mapping_are_validated_before_facts() {
    for entries in [
        vec![(1, 1)],
        vec![(0, 0), (1, 1)],
        vec![(5, BASE + STRINGS as u64), (5, BASE), (0, 0)],
        vec![(30, 0), (30, 0), (0, 0)],
        vec![(5, BASE + STRINGS as u64), (0, 0)],
        vec![(1, 1), (0, 0)],
        vec![(5, BASE + STRINGS as u64), (10, 1024 * 1024 + 1), (0, 0)],
        vec![(5, u64::MAX), (10, 1), (0, 0)],
        vec![(5, BASE + STRINGS as u64), (10, 1), (1, 2), (0, 0)],
    ] {
        let mut bytes = fixture(true, false, false);
        dynamic(&mut bytes, &entries);
        assert!(inspect_bytes(&bytes).is_err(), "{entries:?}");
    }
    let mut bytes = fixture(true, false, false);
    program(&mut bytes, 4, 1, 4, 0, 4096);
    assert!(inspect_bytes(&bytes).is_err());
}

#[test]
fn dynamic_offset_and_address_must_identify_one_complete_unshadowed_load_mapping() {
    for address in [BASE + 1536, BASE + 8192] {
        let mut bytes = fixture(true, false, false);
        // A valid decoy table at p_offset cannot prove facts about another
        // runtime address, whether the other table has NEEDED or is unmapped.
        u64_at(&mut bytes, 64 + 2 * 56 + 16, address);
        assert_eq!(inspect_bytes(&bytes), Err(Failure::Dynamic));
    }
    for (offset, size, memory) in [
        (DYNAMIC + 16, 16, 16),
        (DYNAMIC, 0, 64),
        (STRINGS + 1, 1, 1),
        (STRINGS, 0, 128),
    ] {
        let mut bytes = fixture(true, false, false);
        program(&mut bytes, 4, 1, 4, offset, size);
        u64_at(&mut bytes, 64 + 4 * 56 + 40, memory);
        assert!(inspect_bytes(&bytes).is_err(), "{offset}/{size}/{memory}");
    }
}

#[test]
fn page_rounded_dynamic_and_string_shadows_refuse_decoy_absence_facts() {
    let mut bytes = fixture(true, false, false);
    // Both pages retain identical headers. The second LOAD's declared interval
    // misses DYNAMIC, but its 4 KiB file mapping replaces that entire page.
    program(&mut bytes, 4, 1, 4, 4096 + 2000, 512);
    u64_at(&mut bytes, 64 + 4 * 56 + 16, BASE + 2000);
    dynamic(
        &mut bytes,
        &[
            (5, BASE + STRINGS as u64),
            (10, 28),
            (0x6ffffffb, 0x08000001),
            (0, 0),
            (0, 0),
        ],
    );
    let page = bytes.clone();
    bytes.extend_from_slice(&page);
    u64_at(&mut bytes, 4096 + DYNAMIC + 3 * 16, 1);
    u64_at(&mut bytes, 4096 + DYNAMIC + 3 * 16 + 8, 1);
    assert!(inspect_bytes(&bytes).is_err());

    let mut bytes = fixture(true, false, false);
    bytes.resize(16384, 0);
    let strings = b"\0PRIVATE-SENTINEL-loader.so\0";
    bytes[8192 + STRINGS..8192 + STRINGS + strings.len()].copy_from_slice(strings);
    program(&mut bytes, 0, 1, 5, 0, 16384);
    dynamic(
        &mut bytes,
        &[
            (5, BASE + 8192 + STRINGS as u64),
            (10, strings.len() as u64),
            (1, 1),
            (0, 0),
        ],
    );
    program(&mut bytes, 4, 1, 4, 12288 + 2000, 512);
    u64_at(&mut bytes, 64 + 4 * 56 + 16, BASE + 8192 + 2000);
    assert!(inspect_bytes(&bytes).is_err());
}

#[test]
fn mapping_page_domain_checks_congruence_zero_fill_and_nonoverlapping_boundary() {
    let mut bytes = fixture(true, false, false);
    bytes.resize(8192, 0);
    program(&mut bytes, 4, 1, 4, 4096, 64);
    assert!(inspect_bytes(&bytes).is_ok());
    u64_at(&mut bytes, 64 + 4 * 56 + 16, BASE + 4097);
    assert_eq!(inspect_bytes(&bytes), Err(Failure::Segment));
    program(&mut bytes, 4, 1, 4, 4095, 0);
    u64_at(&mut bytes, 64 + 4 * 56 + 40, 1);
    assert!(inspect_bytes(&bytes).is_err());
}

#[test]
fn global_page_alias_refusal_applies_even_without_a_declared_dynamic_table() {
    let mut bytes = fixture(false, false, false);
    program(&mut bytes, 4, 1, 5, 4096 + 2000, 512);
    u64_at(&mut bytes, 64 + 4 * 56 + 16, BASE + 2000);
    let page = bytes.clone();
    bytes.extend_from_slice(&page);
    // The original program table has no DYNAMIC. Its page-mapped replacement
    // does. Global LOAD admission must run before relying on either absence.
    u32_at(&mut bytes, 4096 + 64 + 2 * 56, 2);
    assert_eq!(inspect_bytes(&bytes), Err(Failure::Segment));
}

#[test]
fn original_complete_program_table_and_any_phdr_declaration_must_agree() {
    let mut bytes = fixture(false, false, false);
    program(&mut bytes, 0, 6, 4, 64, 5 * 56);
    program(&mut bytes, 1, 1, 5, 0, 4096);
    assert!(inspect_bytes(&bytes).is_ok());
    for (offset, value) in [
        (64 + 8, 65),
        (64 + 16, BASE + 128),
        (64 + 32, 5 * 56 - 1),
        (64 + 40, 5 * 56 + 1),
    ] {
        let mut changed = bytes.clone();
        u64_at(&mut changed, offset, value);
        assert_eq!(inspect_bytes(&changed), Err(Failure::ProgramHeaders));
    }
    let mut duplicate = bytes;
    program(&mut duplicate, 4, 6, 4, 64, 5 * 56);
    assert_eq!(inspect_bytes(&duplicate), Err(Failure::ProgramHeaders));

    let mut aliased = fixture(false, false, false);
    program(&mut aliased, 4, 1, 4, 0, 4096);
    u64_at(&mut aliased, 64 + 4 * 56 + 16, BASE + 4096);
    assert_eq!(inspect_bytes(&aliased), Err(Failure::ProgramHeaders));

    for (offset, size) in [(128, 4096 - 128), (0, 65)] {
        let mut bytes = fixture(false, false, false);
        program(&mut bytes, 0, 1, 5, offset, size);
        assert_eq!(inspect_bytes(&bytes), Err(Failure::ProgramHeaders));
    }
}

#[test]
fn unsafe_file_kinds_size_and_namespace_links_are_refused_without_execution() {
    let directory = Fixture::new();
    let path = directory.0.join("candidate");
    fs::write(&path, fixture(false, false, false)).unwrap();
    let link = directory.0.join("link");
    symlink(&path, &link).unwrap();
    assert_eq!(inspect(&link), Err(Failure::Input));
    assert_eq!(inspect(&directory.0), Err(Failure::Input));
    let parent = directory.0.join("parent");
    symlink(&directory.0, &parent).unwrap();
    assert_eq!(inspect(&parent.join("candidate")), Err(Failure::Input));
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(MAX_BYTES + 1)
        .unwrap();
    assert_eq!(inspect(&path), Err(Failure::Input));
    fs::write(&path, []).unwrap();
    assert_eq!(inspect(&path), Err(Failure::Input));
}

#[test]
fn hash_parser_and_final_publication_cannot_cross_file_or_directory_replacement() {
    for stage in ["hashed", "parsed"] {
        for replacement in ["contents", "file", "directory"] {
            let directory = Fixture::new();
            let parent = directory.0.join("inputs");
            fs::create_dir(&parent).unwrap();
            let path = parent.join("candidate");
            let bytes = fixture(true, false, false);
            fs::write(&path, &bytes).unwrap();
            let result = inspect_after(&path, |observed| {
                if observed != stage {
                    return;
                }
                match replacement {
                    "contents" => fs::write(&path, &bytes[..64]).unwrap(),
                    "file" => {
                        fs::remove_file(&path).unwrap();
                        fs::write(&path, &bytes).unwrap();
                    }
                    _ => {
                        fs::rename(&parent, directory.0.join("retained")).unwrap();
                        fs::create_dir(&parent).unwrap();
                        fs::write(&path, &bytes).unwrap();
                    }
                }
            });
            assert!(result.is_err(), "{stage}/{replacement}");
        }
    }
}

#[test]
fn relevant_tables_are_bounded_at_the_exact_limit_before_reader_allocation() {
    let mut bytes = fixture(false, false, false);
    bytes.resize(64 + 8192 * 56, 0);
    let size = bytes.len();
    program(&mut bytes, 0, 1, 5, 0, size);
    u16_at(&mut bytes, 56, 8192);
    assert_eq!(inspect_bytes(&bytes).unwrap().program_headers, 8192);
    u16_at(&mut bytes, 56, 8193);
    assert_eq!(inspect_bytes(&bytes), Err(Failure::ProgramHeaders));

    let mut bytes = fixture(true, false, false);
    bytes.resize(DYNAMIC + 8192 * 16, 0);
    bytes[DYNAMIC..].fill(0);
    // Keep the first LOAD covering the enlarged metadata-only fixture.
    let size = bytes.len();
    program(&mut bytes, 0, 1, 5, 0, size);
    program(&mut bytes, 2, 2, 4, DYNAMIC, 8192 * 16);
    assert!(inspect_bytes(&bytes).unwrap().no_needed);
    bytes.extend_from_slice(&[0; 16]);
    let size = bytes.len();
    program(&mut bytes, 0, 1, 5, 0, size);
    program(&mut bytes, 2, 2, 4, DYNAMIC, 8193 * 16);
    assert_eq!(inspect_bytes(&bytes), Err(Failure::Dynamic));

    for size in [1024 * 1024, 1024 * 1024 + 1] {
        let mut bytes = fixture(true, false, false);
        bytes.resize(STRINGS + size, 0);
        let length = bytes.len();
        program(&mut bytes, 0, 1, 5, 0, length);
        dynamic(
            &mut bytes,
            &[(5, BASE + STRINGS as u64), (10, size as u64), (0, 0)],
        );
        assert_eq!(inspect_bytes(&bytes).is_ok(), size == 1024 * 1024);
    }
}

#[test]
fn dynamic_overlap_matches_actual_python_parser_without_reusing_glibc_policy() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let directory = Fixture::new();
    let mut paths = Vec::new();
    let mut expected = Vec::new();
    for interpreter in [false, true] {
        for needed in [false, true] {
            for flags in [0, 4, 8, 12] {
                for flags1 in [0, 1, 0x08000000, 0x08000001] {
                    let mut bytes = fixture(true, interpreter, needed);
                    // The reference server parser also requires a section-name
                    // table. It is unrelated to the inspector's linkage facts.
                    u64_at(&mut bytes, 40, 2560);
                    u16_at(&mut bytes, 58, 64);
                    u16_at(&mut bytes, 60, 1);
                    u32_at(&mut bytes, 2560 + 4, 3);
                    u64_at(&mut bytes, 2560 + 24, 2048);
                    u64_at(&mut bytes, 2560 + 32, 1);
                    let mut entries = vec![
                        (5, BASE + STRINGS as u64),
                        (10, b"\0PRIVATE-SENTINEL-loader.so\0".len() as u64),
                        (30, flags),
                        (0x6ffffffb, flags1),
                    ];
                    if needed {
                        entries.push((1, 1));
                    }
                    entries.push((0, 0));
                    dynamic(&mut bytes, &entries);
                    let path = directory.0.join(format!("{}.elf", paths.len()));
                    fs::write(&path, &bytes).unwrap();
                    expected.push(inspect(&path).unwrap());
                    paths.push(path);
                }
            }
        }
    }
    let source = r#"
import json,pathlib,sys
sys.path.insert(0,sys.argv[1]);import security_elf as p
results=[]
for path in json.load(sys.stdin):
 value=p.parse(pathlib.Path(path).read_bytes(),'linux/amd64')
 flags=value.dynamic.get(p.DT_FLAGS,[0])[0]
 flags1=value.dynamic.get(p.DT_FLAGS_1,[0])[0]
 results.append({'noInterpreter':value.interpreter is None,'neededCount':len(value.needed),
 'noNeeded':not value.needed,'pieFlag':bool(flags1&p.DF_1_PIE),
 'bindNow':p.DT_BIND_NOW in value.dynamic or bool(flags&p.DF_BIND_NOW or flags1&p.DF_1_NOW),
 'noTextRelocations':p.DT_TEXTREL not in value.dynamic and not bool(flags&p.DF_TEXTREL)})
json.dump(results,sys.stdout)
"#;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let mut child = Command::new("python3")
        .args(["-B", "-c", source])
        .arg(root.join("build"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&paths).unwrap())
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let values: Vec<serde_json::Value> = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(values.len(), 64);
    for (expected, fields) in expected.iter().zip(values) {
        let actual = serde_json::to_value(expected).unwrap();
        for (field, value) in fields.as_object().unwrap() {
            assert_eq!(&actual[field], value, "{field}");
        }
    }
}
