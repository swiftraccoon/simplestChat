//! Owned files only; namespace handshakes run exactly between check and open.

use super::*;
use std::{
    os::unix::fs::{DirBuilderExt, PermissionsExt, symlink},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "impaired-output-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
    fn output(&self) -> Output {
        Output::new(&self.0.join("output")).unwrap()
    }
    fn replace(&self) {
        fs::rename(self.0.join("output"), self.0.join("retired")).unwrap();
        fs::DirBuilder::new()
            .mode(0o700)
            .create(self.0.join("output"))
            .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn fixed_files_are_private_exclusive_and_summary_keeps_generator_modes() {
    let fixture = Fixture::new();
    let output = fixture.output();
    let mut log = output.file("generator.log").unwrap();
    log.write_all(b"private log").unwrap();
    nix::unistd::close(log).unwrap();
    assert!(output.file("generator.log").is_err());
    assert!(output.file("../unlisted").is_err());
    output
        .json("impaired-summary.json", &serde_json::json!({"counter":7}))
        .unwrap();
    output.statistics(b"private tc observations\n").unwrap();
    for name in ["generator.log", "impaired-summary.json", "netem-stats.txt"] {
        let metadata = fs::metadata(output.path.join(name)).unwrap();
        assert_eq!(metadata.mode() & 0o7777, 0o600);
        assert_eq!(metadata.uid(), output.identity.uid());
    }
    assert!(output.json("load_test_summary.json", &0).is_err());
    assert!(
        output
            .json("impaired-exit.json", &"x".repeat(16 * 1024))
            .is_err()
    );
    assert!(
        output
            .statistics(&vec![0; super::super::plan::CLI_LIMIT as usize + 1])
            .is_err()
    );
    let summary = output.path.join("load_test_summary.json");
    fs::write(&summary, br#"{"observed":7,"unicode":"\u2603"}"#).unwrap();
    fs::set_permissions(&summary, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        output.read_summary().unwrap(),
        serde_json::json!({"observed":7,"unicode":"☃"})
    );
    output.finish().unwrap();
}

#[test]
fn replacement_between_check_and_log_open_never_receives_a_payload() {
    let fixture = Fixture::new();
    let output = fixture.output();
    assert!(output.fresh("server.log", || fixture.replace()).is_err());
    assert_eq!(fs::read_dir(fixture.0.join("output")).unwrap().count(), 0);
    assert_eq!(
        fs::metadata(fixture.0.join("retired/server.log"))
            .unwrap()
            .len(),
        0
    );
    assert!(output.finish().is_err());
}

#[test]
fn evidence_and_statistics_writes_remain_in_the_retained_directory() {
    for name in [
        "impaired-exit.json",
        "impaired-summary.json",
        "netem-stats.txt",
    ] {
        let fixture = Fixture::new();
        let output = fixture.output();
        assert!(
            output
                .write(name, b"private payload", || fixture.replace())
                .is_err()
        );
        assert_eq!(fs::read_dir(fixture.0.join("output")).unwrap().count(), 0);
        assert_eq!(
            fs::metadata(fixture.0.join("retired").join(name))
                .unwrap()
                .len(),
            0
        );
        assert!(output.finish().is_err());
    }
}

#[test]
fn moved_output_and_decoy_summary_are_refused_without_mutating_either_input() {
    let fixture = Fixture::new();
    let output = fixture.output();
    let original = br#"{"counter":7}"#;
    let decoy = br#"{"private":"foreign input"}"#;
    fs::write(output.path.join("load_test_summary.json"), original).unwrap();
    assert!(
        output
            .read_summary_after(|| {
                fixture.replace();
                fs::write(fixture.0.join("output/load_test_summary.json"), decoy).unwrap();
            })
            .is_err()
    );
    assert_eq!(
        fs::read(fixture.0.join("retired/load_test_summary.json")).unwrap(),
        original
    );
    assert_eq!(
        fs::read(fixture.0.join("output/load_test_summary.json")).unwrap(),
        decoy
    );
    assert!(output.finish().is_err());
}

#[test]
fn summary_links_nonregular_growth_and_invalid_json_are_refused() {
    let fixture = Fixture::new();
    let output = fixture.output();
    let name = output.path.join("load_test_summary.json");
    fs::write(fixture.0.join("foreign"), b"private input").unwrap();
    symlink(fixture.0.join("foreign"), &name).unwrap();
    assert!(output.read_summary().is_err());
    fs::remove_file(&name).unwrap();
    fs::create_dir(&name).unwrap();
    assert!(output.read_summary().is_err());
    fs::remove_dir(&name).unwrap();
    let file = File::create(&name).unwrap();
    file.set_len(SUMMARY_LIMIT + 1).unwrap();
    nix::unistd::close(file).unwrap();
    assert!(output.read_summary().is_err());
    fs::remove_file(&name).unwrap();
    fs::hard_link(fixture.0.join("foreign"), &name).unwrap();
    assert!(output.read_summary().is_err());
    fs::remove_file(&name).unwrap();
    fs::write(&name, b"invalid private JSON").unwrap();
    let error = output.read_summary().unwrap_err().to_string();
    assert_eq!(error, "Invalid impaired summary JSON");
    assert!(!error.contains("private"));
    assert_eq!(
        fs::read(fixture.0.join("foreign")).unwrap(),
        b"private input"
    );
    output.finish().unwrap();
}
