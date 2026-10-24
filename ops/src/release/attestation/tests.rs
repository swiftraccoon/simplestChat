//! Real inert archives and filesystem effects; no Git, Docker or remote operation.

use super::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256, Sha384};
use std::{
    fs,
    io::Cursor,
    os::unix::fs::DirBuilderExt,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

static NEXT: AtomicUsize = AtomicUsize::new(0);
pub(super) const REV: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const ORIGINAL: [&str; 4] = ["image.tar", "release.json", "outcome.json", "source.json"];
pub(super) struct Fixture {
    pub(super) root: PathBuf,
    pub(super) artifact: PathBuf,
    pub(super) security: PathBuf,
}
fn encoded(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}
fn checksum(bytes: &[u8]) -> String {
    files::hex(&Sha256::digest(bytes))
}
fn archive(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut archive = tar::Builder::new(Vec::new());
    for (name, bytes) in files {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(bytes.len().try_into().unwrap());
        header.set_cksum();
        archive
            .append_data(&mut header, name, Cursor::new(bytes))
            .unwrap();
    }
    archive.into_inner().unwrap()
}
impl Fixture {
    pub(super) fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "PRIVATE-attestation-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let artifact = root.join("artifact");
        let security = root.join("security");
        fs::create_dir(&artifact).unwrap();
        fs::create_dir(&security).unwrap();
        fs::create_dir(security.join("spdx")).unwrap();
        let config = encoded(&json!({"architecture":"amd64","os":"linux","config":{
            "User":"10001:10001","Cmd":["/app/simplestChat"],
            "Labels":{"org.opencontainers.image.revision":REV}}}));
        let image_id = format!("sha256:{}", checksum(&config));
        let image = archive(&[
            (
                "manifest.json",
                encoded(&json!([{"Config":"config.json",
            "RepoTags":[format!("simplestchat-release/production:{REV}")],
            "Layers":["layer.tar"]}])),
            ),
            ("config.json", config),
            ("layer.tar", b"inert layer bytes, never executed".to_vec()),
        ]);
        let image_sha = checksum(&image);
        fs::write(artifact.join("image.tar"), image).unwrap();
        fs::write(
            artifact.join("release.json"),
            encoded(&json!({"schemaVersion":1,
            "revision":REV,"platform":"linux/amd64","archiveSha256":image_sha,
            "imageTag":format!("simplestchat-release/production:{REV}"),
            "migrations":{"1":files::hex(&Sha384::digest(b"SELECT 1;"))},
            "createdAt":"2026-09-13T00:00:00Z"})),
        )
        .unwrap();
        fs::write(
            artifact.join("outcome.json"),
            encoded(&json!({"schemaVersion":1,"revision":REV,
            "passed":true,"error":null,"exportedImageId":image_id})),
        )
        .unwrap();
        fs::write(
            artifact.join("source.json"),
            b"opaque source evidence without schema parsing",
        )
        .unwrap();
        fs::write(
            security.join("spdx/sbom.spdx.json"),
            b"opaque SBOM bytes without SPDX parsing",
        )
        .unwrap();
        fs::write(
            security.join("runtime-proof.json"),
            encoded(&json!({"schemaVersion":1,
            "required":false,"passed":true,"archiveSha256":image_sha,"imageId":image_id,
            "revision":REV,"platform":"linux/amd64"})),
        )
        .unwrap();
        fs::write(security.join("vex.openvex.json"), b"{\"statements\":[]}").unwrap();
        let mut outcome = json!({"passed":true,"secretDetectorSelfTest":true,"revision":REV,
            "archiveSha256":image_sha,"imageId":image_id,"platform":"linux/amd64",
            "selectedImage":{"archiveConfigSha256":image_id.strip_prefix("sha256:").unwrap()},
            "checks":{"vulnerabilities":{"passed":true},"licenses":{"passed":true},
                "secrets":{"passed":true}},"vexRequired":false,"binarySha256":"b".repeat(64)});
        for (field, name) in policy::EVIDENCE {
            let path = security.join(name);
            if !path.exists() {
                fs::write(&path, b"raw hash-only evidence").unwrap();
            }
            outcome[field] = checksum(&fs::read(path).unwrap()).into();
        }
        fs::write(security.join("outcome.json"), encoded(&outcome)).unwrap();
        Self {
            root,
            artifact,
            security,
        }
    }
    fn options(&self) -> Options {
        Options::parse(&[
            "--artifact-dir".into(),
            self.artifact.as_os_str().into(),
            "--security-dir".into(),
            self.security.as_os_str().into(),
            "--repository".into(),
            "owner/repo".into(),
            "--revision".into(),
            REV.into(),
            "--run-id".into(),
            "789".into(),
            "--run-attempt".into(),
            "1".into(),
            "--timeout-seconds".into(),
            "60".into(),
            "--finalization-seconds".into(),
            "10".into(),
        ])
        .unwrap()
        .unwrap()
    }
    fn original(&self) -> Vec<Vec<u8>> {
        ORIGINAL
            .iter()
            .map(|name| fs::read(self.artifact.join(name)).unwrap())
            .collect()
    }
    fn mutate_security(&self, change: impl FnOnce(&mut Value)) {
        let path = self.security.join("outcome.json");
        let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        change(&mut value);
        fs::write(path, encoded(&value)).unwrap();
    }
    fn required(&self, expires: &str) {
        let path = self.security.join("runtime-proof.json");
        let mut proof: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let security: Value =
            serde_json::from_slice(&fs::read(self.security.join("outcome.json")).unwrap()).unwrap();
        proof["required"] = true.into();
        proof["scope"] = "managed-default-server-dtls".into();
        proof["sourceReview"] = json!({"expires":expires});
        for name in ["binarySha256", "nativeSha256", "elfSha256", "sbomSha256"] {
            proof[name] = security[name].clone();
        }
        fs::write(&path, encoded(&proof)).unwrap();
        fs::write(
            self.security.join("vex.openvex.json"),
            b"{\"statements\":[{\"uninterpreted\":true}]}",
        )
        .unwrap();
        self.mutate_security(|security| {
            security["vexRequired"] = true.into();
            security["runtimeProofSha256"] = checksum(&fs::read(path).unwrap()).into();
            security["vexSha256"] =
                checksum(&fs::read(self.security.join("vex.openvex.json")).unwrap()).into();
        });
    }
    fn run(&self, fault: Option<(&str, bool)>) -> (Result<()>, (usize, usize)) {
        let options = self.options();
        let mut owner = Owner::new(Instant::now(), options.whole, options.reserve).unwrap();
        owner.today = Some(chrono::NaiveDate::from_ymd_opt(2050, 1, 1).unwrap());
        files::start_probe(None);
        if let Some((name, read)) = fault {
            files::copy_fault(name.to_owned(), read, 3);
        }
        let result = execute(&options, &mut owner);
        let observed = files::take_probe();
        (result, observed)
    }
    fn copies(&self) -> usize {
        COPIES
            .iter()
            .filter(|(_, name)| self.artifact.join(name).exists())
            .count()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[tokio::test]
async fn complete_prepare_retains_originals_and_writes_exact_unsigned_nine_file_contract() {
    let fixture = Fixture::new();
    let originals = fixture.original();
    let (result, closes) = fixture.run(None);
    let after = fixture.original();
    result.unwrap();
    assert_eq!(originals, after);
    assert!(closes.0 > 18 && closes.1 == 0);
    assert_eq!(fs::read_dir(&fixture.artifact).unwrap().count(), 9);
    let claim: Value =
        serde_json::from_slice(&fs::read(fixture.artifact.join("release-predicate.json")).unwrap())
            .unwrap();
    assert_eq!(claim["fileDigests"].as_object().unwrap().len(), 8);
    for name in crate::release::trust::BOUND_FILES {
        assert_eq!(
            claim["fileDigests"][name],
            checksum(&fs::read(fixture.artifact.join(name)).unwrap())
        );
    }
    assert!(!fixture.artifact.join("release-attestation.jsonl").exists());
}

#[tokio::test]
async fn all_eight_evidence_hash_refusals_precede_every_copy_and_close_all_peers() {
    for (field, _) in policy::EVIDENCE {
        let fixture = Fixture::new();
        let originals = fixture.original();
        fixture.mutate_security(|value| value[field] = "0".repeat(64).into());
        let (result, closes) = fixture.run(None);
        assert!(result.is_err(), "{field}");
        assert_eq!(closes.1, 0);
        assert_eq!(fixture.copies(), 0);
        assert_eq!(fixture.original(), originals);
        assert!(!fixture.artifact.join("release-predicate.json").exists());
    }
}

#[tokio::test]
async fn five_empty_hash_only_files_and_opaque_source_and_sbom_are_accepted() {
    for (field, name) in policy::EVIDENCE.into_iter().filter(|(_, name)| {
        ![
            "spdx/sbom.spdx.json",
            "runtime-proof.json",
            "vex.openvex.json",
        ]
        .contains(name)
    }) {
        let fixture = Fixture::new();
        fs::write(fixture.security.join(name), []).unwrap();
        fixture.mutate_security(|value| value[field] = checksum(&[]).into());
        let (result, closes) = fixture.run(None);
        result.unwrap();
        assert_eq!(closes.1, 0);
    }
}

#[tokio::test]
async fn exclusive_destination_conflicts_retain_the_reference_copy_prefix() {
    for (at, name) in COPIES
        .iter()
        .map(|(_, name)| *name)
        .chain(["release-predicate.json"])
        .enumerate()
    {
        let fixture = Fixture::new();
        let originals = fixture.original();
        fs::write(fixture.artifact.join(name), b"foreign retained bytes").unwrap();
        let (result, closes) = fixture.run(None);
        assert!(result.is_err());
        assert_eq!(closes.1, 0);
        assert_eq!(fixture.original(), originals);
        assert_eq!(
            fs::read(fixture.artifact.join(name)).unwrap(),
            b"foreign retained bytes"
        );
        for (index, (_, copy)) in COPIES.iter().enumerate() {
            assert_eq!(fixture.artifact.join(copy).exists(), index <= at);
        }
    }
}

#[tokio::test]
async fn real_copy_read_and_write_errors_retain_partial_bytes_and_settle_all_owners() {
    for read in [false, true] {
        for (at, (_, name)) in COPIES.iter().enumerate() {
            let fixture = Fixture::new();
            let originals = fixture.original();
            let (result, closes) = fixture.run(Some((name, read)));
            assert_eq!(result, Err(Cause::Io));
            assert_eq!(closes.1, 0);
            assert_eq!(fixture.original(), originals);
            assert_eq!(fs::read(fixture.artifact.join(name)).unwrap().len(), 3);
            assert_eq!(fixture.copies(), at + 1);
            assert!(!fixture.artifact.join("release-predicate.json").exists());
        }
    }
}

#[tokio::test]
async fn expiry_is_exclusive_and_optional_proofs_do_not_require_reviews() {
    for (date, passed) in [
        ("2049-12-31", false),
        ("2050-01-01", false),
        ("2050-01-02", true),
        ("2050-02-30", false),
        ("0000-01-01", false),
    ] {
        let fixture = Fixture::new();
        fixture.required(date);
        let (result, closes) = fixture.run(None);
        assert_eq!(result.is_ok(), passed, "{date}");
        assert_eq!(closes.1, 0);
        assert_eq!(fixture.copies(), 4);
        assert_eq!(
            fixture.artifact.join("release-predicate.json").exists(),
            passed
        );
    }
}

#[tokio::test]
async fn byte_json_encoding_and_ignored_large_integer_lexemes_remain_original_bytes() {
    for utf16 in [false, true] {
        let fixture = Fixture::new();
        let path = fixture.security.join("outcome.json");
        let mut bytes = fs::read(&path).unwrap();
        bytes.pop();
        bytes.extend_from_slice(format!(",\"ignored\":{}}}", "9".repeat(200)).as_bytes());
        if utf16 {
            let text = std::str::from_utf8(&bytes).unwrap();
            bytes = [
                vec![0xff, 0xfe],
                text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
            ]
            .concat();
        }
        fs::write(path, &bytes).unwrap();
        let (result, closes) = fixture.run(None);
        result.unwrap();
        assert_eq!(closes.1, 0);
        assert_eq!(
            fs::read(fixture.artifact.join("image-security.json")).unwrap(),
            bytes
        );
    }
}

#[tokio::test]
async fn original_alias_exchange_is_refused_and_all_earlier_handles_close() {
    let fixture = Fixture::new();
    let options = fixture.options();
    let mut owner = Owner::new(Instant::now(), options.whole, options.reserve).unwrap();
    files::start_probe(None);
    let mut state = State::new();
    let admitted = state.input(0, &options, &mut owner).map(|_| ());
    let path = fixture.artifact.join("image.tar");
    let exchanged: std::io::Result<()> = (|| {
        let bytes = fs::read(&path)?;
        fs::rename(&path, fixture.artifact.join("original-image"))?;
        fs::write(path, &bytes)
    })();
    if exchanged.is_err() {
        owner.record(Cause::Io);
    }
    let result = state.perform(&options, &mut owner);
    if let Err(error) = result {
        owner.record(error);
    }
    state.finish(&mut owner);
    let closes = files::take_probe();
    admitted.unwrap();
    exchanged.unwrap();
    assert!(owner.error().is_some());
    assert_eq!(closes.1, 0);
    assert!(!fixture.artifact.join("release-predicate.json").exists());
}

#[tokio::test]
async fn original_budget_reserve_and_late_finalization_cannot_produce_success() {
    for elapsed in [50, 60] {
        let fixture = Fixture::new();
        let options = fixture.options();
        let mut owner = Owner::new(Instant::now(), options.whole, options.reserve).unwrap();
        owner.elapsed = Some(Duration::from_secs(elapsed));
        files::start_probe(None);
        let result = execute(&options, &mut owner);
        let closes = files::take_probe();
        assert_eq!(result, Err(Cause::Deadline));
        assert_eq!(closes.1, 0);
        assert_eq!(fixture.copies(), 0);
    }
    let fixture = Fixture::new();
    let options = fixture.options();
    let mut owner = Owner::new(Instant::now(), options.whole, options.reserve).unwrap();
    files::start_probe(None);
    let mut state = State::new();
    let performed = state.perform(&options, &mut owner);
    if let Err(error) = performed.as_ref() {
        owner.record(*error);
    }
    owner.elapsed = Some(Duration::from_secs(60));
    state.finish(&mut owner);
    let closes = files::take_probe();
    performed.unwrap();
    assert_eq!(owner.error(), Some(Cause::Deadline));
    assert_eq!(closes.1, 0);
    assert!(fixture.artifact.join("release-predicate.json").exists());
}

#[tokio::test]
async fn close_failure_is_negative_and_never_skips_other_admitted_peers() {
    let fixture = Fixture::new();
    let options = fixture.options();
    let mut owner = Owner::new(Instant::now(), options.whole, options.reserve).unwrap();
    files::start_probe(Some(1));
    let result = execute(&options, &mut owner);
    let closes = files::take_probe();
    assert_eq!(result, Err(Cause::Close));
    assert!(closes.0 > 1 && closes.1 == 0);
    assert_eq!(fixture.copies(), 0);
}

#[tokio::test]
async fn known_runtime_security_refusal_precedes_a_later_replaced_vex_read() {
    let fixture = Fixture::new();
    let options = fixture.options();
    let mut owner = Owner::new(Instant::now(), options.whole, options.reserve).unwrap();
    files::start_probe(None);
    let mut state = State::new();
    let prepared = state.perform(&options, &mut owner);
    let observed: Result<()> = (|| {
        prepared?;
        let mut security = state.read("image-security.json", &options, &mut owner)?;
        if let crate::json::Json::Object(values, _) = &mut security {
            values.insert("vexRequired".into(), crate::json::Json::Null);
        }
        let mut digests = BTreeMap::new();
        for name in crate::release::trust::BOUND_FILES {
            digests.insert(
                name.to_owned(),
                state.bound(name, &options, &mut owner)?.hash(&mut owner)?,
            );
        }
        let image = security
            .get("imageId")
            .and_then(crate::json::Json::string)
            .ok_or(Cause::Policy)?
            .to_owned();
        let path = fixture.artifact.join("vex.openvex.json");
        control::io(
            &mut owner,
            fs::rename(&path, fixture.artifact.join("retained-original-vex")),
        )?;
        control::io(&mut owner, fs::write(path, b"later foreign replacement"))?;
        state.bind_runtime(&security, &options, &image, &digests, &mut owner)
    })();
    if let Err(error) = observed {
        owner.record(error);
    }
    state.finish(&mut owner);
    let closes = files::take_probe();
    assert_eq!(owner.error(), Some(Cause::Policy));
    assert_eq!(closes.1, 0);
}

#[tokio::test]
async fn lexical_expiry_refusal_precedes_utc_but_calendar_parse_follows_it() {
    for (expiry, expected) in [("not-a-date", Cause::Policy), ("2050-02-30", Cause::Io)] {
        let mut owner = Owner::new(
            Instant::now(),
            Duration::from_secs(60),
            Duration::from_secs(10),
        )
        .unwrap();
        owner.utc_error = true;
        let proof = policy::decode(
            &encoded(&json!({"sourceReview":{"expires":expiry}})),
            &mut owner,
        )
        .unwrap();
        let security = policy::decode(b"{}", &mut owner).unwrap();
        let result = policy::runtime_review(&security, &proof, true, &mut owner);
        assert_eq!(result, Err(expected));
        assert_eq!(owner.error(), Some(expected));
    }
}

#[tokio::test]
async fn actual_regular_file_bounds_keep_exact_metadata_and_late_image_edges() {
    for maximum in [policy::METADATA as u64, policy::IMAGE] {
        for (length, passed) in [(0, false), (maximum, true), (maximum + 1, false)] {
            let fixture = Fixture::new();
            let options = fixture.options();
            let file = fs::OpenOptions::new()
                .write(true)
                .open(fixture.security.join("native.json"))
                .unwrap();
            let sized = file.set_len(length);
            let closed = nix::unistd::close(file);
            sized.unwrap();
            closed.unwrap();
            let mut owner = Owner::new(Instant::now(), options.whole, options.reserve).unwrap();
            files::start_probe(None);
            let mut state = State::new();
            let bounded = state
                .input(6, &options, &mut owner)
                .and_then(|file| file.bound(maximum, &mut owner));
            if let Err(error) = bounded {
                owner.record(error);
            }
            state.finish(&mut owner);
            let closes = files::take_probe();
            assert_eq!(bounded.is_ok(), passed);
            assert_eq!(closes.1, 0);
        }
    }
}

#[tokio::test]
async fn an_observed_policy_refusal_remains_primary_through_later_close_failures() {
    let fixture = Fixture::new();
    let options = fixture.options();
    fixture.mutate_security(|value| value["passed"] = false.into());
    let mut owner = Owner::new(Instant::now(), options.whole, options.reserve).unwrap();
    let mut state = State::new();
    let result = state.perform(&options, &mut owner);
    if let Err(error) = result {
        owner.record(error);
    }
    files::start_probe(Some(1));
    state.finish(&mut owner);
    let closes = files::take_probe();
    assert_eq!(result, Err(Cause::Policy));
    assert_eq!(owner.error(), Some(Cause::Policy));
    assert!(closes.0 > 1 && closes.1 == 0);
}

#[tokio::test]
async fn overlapping_input_directories_are_not_given_an_extra_admission_policy() {
    let fixture = Fixture::new();
    let mut security: Value =
        serde_json::from_slice(&fs::read(fixture.security.join("outcome.json")).unwrap()).unwrap();
    let build: Value =
        serde_json::from_slice(&fs::read(fixture.artifact.join("outcome.json")).unwrap()).unwrap();
    security
        .as_object_mut()
        .unwrap()
        .extend(build.as_object().unwrap().clone());
    fs::write(fixture.artifact.join("outcome.json"), encoded(&security)).unwrap();
    fs::create_dir(fixture.artifact.join("spdx")).unwrap();
    for (_, name) in policy::EVIDENCE {
        fs::copy(fixture.security.join(name), fixture.artifact.join(name)).unwrap();
    }
    let before = fixture.original();
    let mut options = fixture.options();
    options.security = options.artifact.clone();
    let mut owner = Owner::new(Instant::now(), options.whole, options.reserve).unwrap();
    files::start_probe(None);
    let result = execute(&options, &mut owner);
    let closes = files::take_probe();
    assert!(result.is_err());
    assert_eq!(closes.1, 0);
    assert_eq!(fixture.original(), before);
    assert!(fixture.artifact.join("sbom.spdx.json").exists());
    assert!(fixture.artifact.join("image-security.json").exists());
    assert_eq!(
        fs::read(fixture.artifact.join("runtime-proof.json")).unwrap(),
        fs::read(fixture.security.join("runtime-proof.json")).unwrap()
    );
    assert!(!fixture.artifact.join("release-predicate.json").exists());
}

#[test]
fn complete_operation_checks_its_original_end_after_owner_and_runtime_teardown() {
    for offset in [None, Some(Duration::ZERO), Some(Duration::from_secs(1))] {
        let fixture = Fixture::new();
        let options = fixture.options();
        let started = Instant::now();
        let end = started.checked_add(options.whole).unwrap();
        COMPLETION_OFFSET.set(offset);
        RUNTIME_DROPPED.set(false);
        files::start_probe(None);
        let passed = operation(started, end, &options);
        let closes = files::take_probe();
        let runtime_dropped = RUNTIME_DROPPED.get();
        COMPLETION_OFFSET.set(None);
        assert_eq!(passed, offset.is_none());
        assert!(runtime_dropped);
        assert!(closes.0 > 0 && closes.1 == 0);
        assert_eq!(fixture.copies(), 4);
        assert!(fixture.artifact.join("release-predicate.json").exists());
    }
}

#[test]
fn late_real_signals_after_runtime_restore_refuse_prepare_success() {
    use control::signal_tests::{self, Entry};
    let Some(expected) = signal_tests::worker(Entry::Prepare) else {
        return signal_tests::supervise(Entry::Prepare);
    };
    let previous = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o027));
    let fixture = Fixture::new();
    let options = fixture.options();
    let started = Instant::now();
    RUNTIME_DROPPED.set(false);
    files::start_probe(None);
    let passed = operation(started, started + options.whole, &options);
    let probes = files::take_probe();
    let copies = fixture.copies();
    let predicate = fixture.artifact.join("release-predicate.json").exists();
    let runtime_closed = RUNTIME_DROPPED.get();
    drop(fixture);
    let restored = nix::sys::stat::umask(previous);
    signal_tests::assert_finished(
        if passed {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        },
        expected,
        nix::sys::stat::Mode::from_bits_truncate(0o027),
    );
    assert!(runtime_closed && predicate);
    assert_eq!(copies, 4);
    assert!(probes.0 > 0 && probes.1 == 0);
    assert_eq!(restored, nix::sys::stat::Mode::from_bits_truncate(0o027));
}
