//! New local assembly contract. All executables are inert metadata bytes.

use super::*;
use crate::{distribution::fixture as elf, test_support::Fixture};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

use super::super::tests::fixture;

struct Inputs {
    root: Fixture,
    manifest: ArtifactManifest,
    selection: Selection,
    evidence: EvidenceSources,
    original: Vec<u8>,
}
impl Inputs {
    fn new(with_evidence: bool) -> Self {
        let root = Fixture::new();
        let binary = elf::fixture(true, false, false);
        fs::write(root.0.join("candidate"), &binary).unwrap();
        let mut value = fixture::manifest(&binary);
        let mut sources = BTreeMap::new();
        if with_evidence {
            let bytes = b"PRIVATE-EVIDENCE-NOT-A-BUILD-RECEIPT\n";
            let path = root.0.join("private-input");
            fs::write(&path, bytes).unwrap();
            value["evidenceDigests"] = json!({"PRIVATE-NAME/claim.json":fixture::digest(bytes)});
            sources.insert("PRIVATE-NAME/claim.json".into(), path);
        }
        let mut original = b"\xef\xbb\xbf".to_vec();
        original.extend(serde_json::to_vec_pretty(&value).unwrap());
        original.extend(b"\n\n");
        let manifest = ArtifactManifest::decode(&original).unwrap();
        let selection =
            Selection::decode(&serde_json::to_vec(&fixture::selection()).unwrap()).unwrap();
        Self {
            root,
            manifest,
            selection,
            evidence: EvidenceSources::new(sources).unwrap(),
            original,
        }
    }
    fn output(&self) -> PathBuf {
        self.root.0.join("output")
    }
    fn prepare(&self) -> Result<PreparedArtifact, Failure> {
        prepare_artifact(
            &self.manifest,
            &self.root.0.join("candidate"),
            &self.selection,
            &self.evidence,
            &self.output(),
        )
    }
    fn record(&self, name: &str) -> Value {
        serde_json::from_slice(&fs::read(self.output().join(name)).unwrap()).unwrap()
    }
}

#[test]
fn assembly_measures_exact_original_payloads_and_keeps_declarations_separate() {
    for with_evidence in [false, true] {
        let inputs = Inputs::new(with_evidence);
        let result = inputs.prepare().unwrap();
        assert_eq!(
            fs::read(inputs.output().join("ops-release.json")).unwrap(),
            inputs.original
        );
        assert_eq!(
            fs::read(inputs.output().join("simplestchat-ops")).unwrap(),
            fs::read(inputs.root.0.join("candidate")).unwrap()
        );
        let preparation = inputs.record("ops-preparation.json");
        assert_eq!(preparation["kind"], "ops-unsigned-artifact");
        assert_eq!(preparation["evidenceCount"], usize::from(with_evidence));
        let predicate = inputs.record("ops-predicate.json");
        assert_eq!(predicate["predicateType"], PREDICATE_TYPE);
        assert_eq!(
            predicate["identity"]["manifestSha256"],
            fixture::digest(&inputs.original)
        );
        assert_eq!(predicate["identity"]["executable"]["noInterpreter"], true);
        for (name, digest) in preparation["fileDigests"].as_object().unwrap() {
            let bytes = fs::read(inputs.output().join(name)).unwrap();
            assert_eq!(digest["sha256"], fixture::digest(&bytes));
            assert_eq!(digest["bytes"], bytes.len());
        }
        let public = serde_json::to_string(&result).unwrap();
        let debug = format!("{result:?} {:?}", inputs.evidence);
        for private in [
            "PRIVATE-NAME",
            "PRIVATE-EVIDENCE",
            "inputDigests",
            "evidenceDigests",
            "passed",
            "buildPassed",
            inputs.root.0.to_str().unwrap(),
        ] {
            assert!(!public.contains(private));
            assert!(!debug.contains(private));
        }
        assert_eq!(
            fs::metadata(inputs.output()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for entry in fs::read_dir(inputs.output()).unwrap() {
            assert_eq!(
                entry.unwrap().metadata().unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}

#[test]
fn every_selected_identity_is_fenced_before_binary_or_output_access() {
    for field in 0..7 {
        let inputs = Inputs::new(false);
        let mut selected = inputs.selection.0.clone();
        match field {
            0 => selected.repository = "other/repo".into(),
            1 => selected.revision = "b".repeat(40),
            2 => selected.package_version = "0.1.1".into(),
            3 => selected.protocol_version = 2,
            4 => selected.target = "other".into(),
            5 => selected.build_run_id += 1,
            _ => selected.run_attempt += 1,
        }
        fs::remove_file(inputs.root.0.join("candidate")).unwrap();
        let result = prepare_artifact(
            &inputs.manifest,
            &inputs.root.0.join("candidate"),
            &Selection(selected),
            &inputs.evidence,
            &inputs.output(),
        );
        assert_eq!(result.unwrap_err(), Failure::Selection);
        assert!(!inputs.output().exists());
    }
}

#[test]
fn evidence_mapping_has_independent_strict_schema_paths_and_exact_declared_set() {
    for bytes in [
        b"[]".as_slice(),
        b"{\"a\":1}",
        b"{\"a\":\"relative\"}",
        b"{\"../escape\":\"/owned\"}",
        b"{\"a\":\"/owned\",\"a\":\"/owned\"}",
    ] {
        assert!(EvidenceSources::decode(bytes).is_err());
    }
    let mut oversized = vec![b' '; MAX_EVIDENCE_MAPPING_BYTES + 1];
    oversized[..2].copy_from_slice(b"{}");
    assert!(EvidenceSources::decode(&oversized).is_err());
    let entries: BTreeMap<_, _> = (0..17)
        .map(|n| (format!("logical-{n}"), PathBuf::from("/owned")))
        .collect();
    assert!(EvidenceSources::new(entries).is_err());
    for extra in [false, true] {
        let mut inputs = Inputs::new(!extra);
        inputs.evidence = if extra {
            EvidenceSources::new(BTreeMap::from([("extra".into(), PathBuf::from("/owned"))]))
                .unwrap()
        } else {
            EvidenceSources::new(BTreeMap::new()).unwrap()
        };
        assert_eq!(inputs.prepare().unwrap_err(), Failure::Evidence);
        assert!(!inputs.output().exists());
    }
}

#[test]
fn evidence_identity_or_safe_reader_failure_never_publishes_a_final_record() {
    for kind in ["different", "empty", "oversize", "symlink", "fifo"] {
        let inputs = Inputs::new(true);
        let path = inputs.root.0.join("private-input");
        match kind {
            "different" => fs::write(&path, b"changed").unwrap(),
            "empty" => fs::write(&path, []).unwrap(),
            "oversize" => fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap()
                .set_len(MAX_EVIDENCE_BYTES + 1)
                .unwrap(),
            "symlink" => {
                fs::remove_file(&path).unwrap();
                symlink("candidate", &path).unwrap();
            }
            _ => {
                fs::remove_file(&path).unwrap();
                nix::unistd::mkfifo(&path, nix::sys::stat::Mode::from_bits_truncate(0o600))
                    .unwrap();
            }
        }
        assert!(inputs.prepare().is_err(), "{kind}");
        assert!(
            !inputs.output().join("ops-preparation.json").exists(),
            "{kind}"
        );
    }
}

#[test]
fn fresh_output_and_static_linkage_are_required_without_replacing_existing_files() {
    let inputs = Inputs::new(false);
    fs::create_dir(inputs.output()).unwrap();
    assert_eq!(inputs.prepare().unwrap_err(), Failure::Output);
    assert!(fs::read_dir(inputs.output()).unwrap().next().is_none());
    let mut inputs = Inputs::new(false);
    let binary = elf::fixture(true, true, true);
    fs::write(inputs.root.0.join("candidate"), &binary).unwrap();
    inputs.manifest =
        ArtifactManifest::decode(&serde_json::to_vec(&fixture::manifest(&binary)).unwrap())
            .unwrap();
    assert_eq!(inputs.prepare().unwrap_err(), Failure::Executable);
    assert!(!inputs.output().join("ops-preparation.json").exists());
}

#[test]
fn retained_original_manifest_is_used_after_its_old_path_is_replaced() {
    let inputs = Inputs::new(false);
    fs::write(inputs.root.0.join("ops-release.json"), b"replacement").unwrap();
    inputs.prepare().unwrap();
    assert_eq!(
        fs::read(inputs.output().join("ops-release.json")).unwrap(),
        inputs.original
    );
    assert_eq!(
        fs::read(inputs.root.0.join("ops-release.json")).unwrap(),
        b"replacement"
    );
}

#[test]
fn changed_output_contents_or_namespace_prevents_final_publication_and_foreign_unlink() {
    for phase in ["binaryCopied", "pairVerified", "preparationWritten"] {
        let inputs = Inputs::new(false);
        let result = prepare_after(
            &inputs.manifest,
            &inputs.root.0.join("candidate"),
            &inputs.selection,
            &inputs.evidence,
            &inputs.output(),
            |current| {
                if current == phase {
                    if phase == "preparationWritten" {
                        fs::rename(
                            inputs.output().join("ops-preparation.json"),
                            inputs.output().join("original-preparation"),
                        )
                        .unwrap();
                        fs::write(inputs.output().join("ops-preparation.json"), b"foreign")
                            .unwrap();
                    } else {
                        fs::write(inputs.output().join("simplestchat-ops"), b"changed").unwrap();
                    }
                }
            },
        );
        assert!(result.is_err());
        if phase == "preparationWritten" {
            assert_eq!(
                fs::read(inputs.output().join("ops-preparation.json")).unwrap(),
                b"foreign"
            );
        } else {
            assert!(!inputs.output().join("ops-preparation.json").exists());
        }
    }
    let inputs = Inputs::new(false);
    let moved = inputs.root.0.join("moved");
    let result = prepare_after(
        &inputs.manifest,
        &inputs.root.0.join("candidate"),
        &inputs.selection,
        &inputs.evidence,
        &inputs.output(),
        |phase| {
            if phase == "preparationWritten" {
                fs::rename(inputs.output(), &moved).unwrap();
                fs::create_dir(inputs.output()).unwrap();
                fs::write(inputs.output().join("ops-preparation.json"), b"foreign").unwrap();
            }
        },
    );
    assert!(result.is_err());
    assert_eq!(
        fs::read(inputs.output().join("ops-preparation.json")).unwrap(),
        b"foreign"
    );
    assert!(!moved.join("ops-preparation.json").exists());
}

#[test]
fn every_owned_directory_close_is_attempted_and_a_close_failure_rolls_back_our_record() {
    let root = Fixture::new();
    let path = root.0.join("output");
    let mut output = files::Output::create(&path).unwrap();
    output
        .write("ops-preparation.json", b"unsigned byte inventory\n")
        .unwrap();
    let expected = path.components().count();
    let mut attempted = 0;
    let result = output.finish_with(|file| {
        attempted += 1;
        nix::unistd::close(file).map_err(|_| Failure::Close)?;
        if attempted == 1 {
            Err(Failure::Close)
        } else {
            Ok(())
        }
    });
    assert_eq!(result.unwrap_err(), Failure::Close);
    assert_eq!(attempted, expected);
    assert!(!path.join("ops-preparation.json").exists());
}

#[test]
fn a_same_length_destination_write_before_the_first_stamp_cannot_adopt_a_false_digest() {
    use std::io::{Seek, SeekFrom, Write};
    let root = Fixture::new();
    let path = root.0.join("output");
    let mut output = files::Output::create(&path).unwrap();
    let result = output.write_after("ops-evidence.json", b"intended", |file| {
        let mut file = file;
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(b"tampered").unwrap();
    });
    assert_eq!(result.unwrap_err(), Failure::Changed);
    assert!(output.finish().is_err());
    assert_eq!(
        fs::read(path.join("ops-evidence.json")).unwrap(),
        b"tampered"
    );
    assert!(!path.join("ops-preparation.json").exists());
    let path = root.0.join("failed-final-output");
    let mut output = files::Output::create(&path).unwrap();
    let result = output.write_after("ops-preparation.json", b"intended", |file| {
        let mut file = file;
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(b"tampered").unwrap();
    });
    assert_eq!(result.unwrap_err(), Failure::Changed);
    output.rollback();
    output.finish().unwrap();
    assert!(!path.join("ops-preparation.json").exists());
}

#[test]
fn a_kernel_creator_owner_mismatch_closes_before_any_payload_write() {
    use std::os::{fd::AsRawFd, unix::fs::MetadataExt};
    let root = Fixture::new();
    let path = root.0.join("output");
    let output = files::Output::create(&path).unwrap();
    let mut attempted = false;
    let mut descriptor = -1;
    let owner = fs::metadata(&path).unwrap().uid() ^ 1;
    let result = output.target_with("ops-evidence.json", owner, |file| {
        attempted = true;
        descriptor = file.as_raw_fd();
        nix::unistd::close(file).map_err(|_| Failure::Close)
    });
    assert_eq!(result.unwrap_err(), Failure::Output);
    assert!(attempted);
    assert!(!Path::new(&format!("/dev/fd/{descriptor}")).exists());
    assert!(fs::read(path.join("ops-evidence.json")).unwrap().is_empty());
    assert!(output.finish().is_err());
}

#[test]
fn sixteen_evidence_slots_and_the_exact_per_file_limit_are_admitted() {
    for count in [1, 16] {
        let mut inputs = Inputs::new(false);
        let bytes = if count == 1 {
            vec![b'x'; MAX_EVIDENCE_BYTES as usize]
        } else {
            vec![b'x'; 8]
        };
        let mut value = fixture::manifest(&fs::read(inputs.root.0.join("candidate")).unwrap());
        let mut declared = serde_json::Map::new();
        let mut sources = BTreeMap::new();
        for index in 0..count {
            let name = format!("evidence/{index:02}.json");
            let path = inputs.root.0.join(format!("input-{index}"));
            fs::write(&path, &bytes).unwrap();
            sources.insert(name.clone(), path);
            declared.insert(name, fixture::digest(&bytes).into());
        }
        value["evidenceDigests"] = declared.into();
        inputs.manifest = ArtifactManifest::decode(&serde_json::to_vec(&value).unwrap()).unwrap();
        inputs.evidence = EvidenceSources::new(sources).unwrap();
        let prepared = inputs.prepare().unwrap();
        let prepared = serde_json::to_value(prepared).unwrap();
        assert_eq!(prepared["payloadCount"], count + 5);
        assert_eq!(prepared["evidenceCount"], count);
        assert_eq!(
            fs::read(
                inputs
                    .output()
                    .join(format!("ops-evidence-{:02}.bin", count - 1))
            )
            .unwrap(),
            bytes
        );
    }
}
