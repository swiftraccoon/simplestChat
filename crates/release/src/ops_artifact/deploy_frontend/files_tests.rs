//! Actual private filesystem witnesses; no Git, child, network or host operation.

use super::super::plan::Clock;
use super::*;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant},
};

const REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
static NEXT: AtomicU64 = AtomicU64::new(0);

struct NativeClock;
impl Clock for NativeClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

struct Fixture {
    root: PathBuf,
    budget: Budget,
    cp: Checkpoint,
}
impl Fixture {
    fn new(packed: bool) -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "deploy-files-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let now = Instant::now();
        let fixture = Self {
            root,
            budget: Budget::default(),
            cp: Checkpoint {
                clock: Arc::new(NativeClock),
                origin: now,
                end: now + Duration::from_secs(60),
                abort: Arc::new(AtomicBool::new(false)),
            },
        };
        fixture.directory(".git");
        fixture.write(".git/HEAD", b"ref: refs/heads/main\n");
        fixture.write(".git/index", b"INERT-INDEX");
        fixture.write(".git/config", b"[core]\n\tbare = false\n");
        if packed {
            fixture.write(".git/packed-refs", &fixture.packed());
        } else {
            fixture.directory(".git/refs");
            fixture.directory(".git/refs/heads");
            fixture.write(".git/refs/heads/main", format!("{REVISION}\n").as_bytes());
        }
        fixture
    }
    fn directory(&self, name: &str) {
        let path = self.root.join(name);
        fs::create_dir(&path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fn write(&self, name: &str, bytes: &[u8]) {
        let path = self.root.join(name);
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fn packed(&self) -> Vec<u8> {
        format!("# pack-refs with: peeled fully-peeled sorted\n{REVISION} refs/heads/main\n")
            .into_bytes()
    }
    fn selected(&self) -> Vec<GitInput> {
        let inputs = git_identity(&self.root, REVISION, &self.budget, &self.cp).unwrap();
        self.closed();
        inputs
    }
    fn check(&self, inputs: &[GitInput]) -> Result<()> {
        let result = inputs
            .iter()
            .try_for_each(|input| input.check(&self.budget, &self.cp));
        self.closed();
        result
    }
    fn closed(&self) {
        assert_eq!(self.budget.count(), 0, "all descriptor permits retired");
        assert_eq!(self.budget.close_counts().1, 0, "no fallback closes");
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn loose_main_retains_all_original_inputs_and_absent_packed_refs() {
    for changed in ["HEAD", "index", "config", "refs/heads/main", "packed-refs"] {
        let fixture = Fixture::new(false);
        let inputs = fixture.selected();
        assert_eq!(inputs.len(), 5);
        fixture.check(&inputs).unwrap();
        fixture.check(&inputs).unwrap();
        fixture.write(&format!(".git/{changed}"), b"CHANGED-OR-NEW");
        assert!(fixture.check(&inputs).is_err(), "changed {changed}");
    }
}

#[test]
fn packed_main_witnesses_the_first_missing_loose_ref_component() {
    for missing in ["refs", "refs/heads", "refs/heads/main"] {
        let fixture = Fixture::new(true);
        if missing != "refs" {
            fixture.directory(".git/refs");
        }
        if missing == "refs/heads/main" {
            fixture.directory(".git/refs/heads");
        }
        let inputs = fixture.selected();
        fixture.check(&inputs).unwrap();
        if missing == "refs/heads/main" {
            fixture.write(".git/refs/heads/main", format!("{REVISION}\n").as_bytes());
        } else {
            fixture.directory(&format!(".git/{missing}"));
        }
        assert!(fixture.check(&inputs).is_err(), "appeared {missing}");
    }
}

#[test]
fn ref_repacking_and_byte_identical_file_replacements_invalidate_originals() {
    let fixture = Fixture::new(false);
    let inputs = fixture.selected();
    fs::remove_file(fixture.root.join(".git/refs/heads/main")).unwrap();
    fixture.write(".git/packed-refs", &fixture.packed());
    assert!(fixture.check(&inputs).is_err());

    for name in ["HEAD", "index", "config", "refs/heads/main"] {
        let fixture = Fixture::new(false);
        let inputs = fixture.selected();
        let path = fixture.root.join(".git").join(name);
        let bytes = fs::read(&path).unwrap();
        fs::rename(&path, fixture.root.join("retained-original")).unwrap();
        fixture.write(&format!(".git/{name}"), &bytes);
        assert!(fixture.check(&inputs).is_err(), "replaced {name}");
    }
}

#[test]
fn replaced_git_ancestry_refuses_even_when_every_leaf_byte_matches() {
    let fixture = Fixture::new(false);
    let inputs = fixture.selected();
    fs::rename(fixture.root.join(".git"), fixture.root.join("original-git")).unwrap();
    fixture.directory(".git");
    fixture.directory(".git/refs");
    fixture.directory(".git/refs/heads");
    for name in ["HEAD", "index", "config", "refs/heads/main"] {
        let bytes = fs::read(fixture.root.join("original-git").join(name)).unwrap();
        fixture.write(&format!(".git/{name}"), &bytes);
    }
    assert!(fixture.check(&inputs).is_err());
}

#[test]
fn invalid_head_symbolic_or_mismatched_refs_and_unsafe_leaves_refuse() {
    for case in 0..8 {
        let fixture = Fixture::new(false);
        match case {
            0 => fixture.write(".git/HEAD", format!("{REVISION}\n").as_bytes()),
            1 => fixture.write(".git/HEAD", b"ref: refs/heads/other\n"),
            2 => fixture.write(".git/refs/heads/main", b"ref: refs/heads/other\n"),
            3 => fixture.write(".git/refs/heads/main", "b".repeat(40).as_bytes()),
            4 => {
                let path = fixture.root.join(".git/config");
                fs::rename(&path, fixture.root.join("saved-config")).unwrap();
                symlink(fixture.root.join("saved-config"), path).unwrap();
            }
            5 => fs::set_permissions(
                fixture.root.join(".git/index"),
                fs::Permissions::from_mode(0o620),
            )
            .unwrap(),
            6 => fs::hard_link(
                fixture.root.join(".git/config"),
                fixture.root.join("config-alias"),
            )
            .unwrap(),
            _ => fixture.write(".git/HEAD", b"\xff"),
        }
        assert!(git_identity(&fixture.root, REVISION, &fixture.budget, &fixture.cp).is_err());
        fixture.closed();
    }
}

#[test]
fn packed_main_requires_one_exact_current_revision_entry() {
    for bytes in [
        b"# no selected branch\n".to_vec(),
        format!("{} refs/heads/main\n", "b".repeat(40)).into_bytes(),
        format!("{REVISION} refs/heads/main\n{REVISION} refs/heads/main\n").into_bytes(),
    ] {
        let fixture = Fixture::new(true);
        fixture.write(".git/packed-refs", &bytes);
        assert!(git_identity(&fixture.root, REVISION, &fixture.budget, &fixture.cp).is_err());
        fixture.closed();
    }
}

#[test]
fn original_reads_refuse_oversized_files_and_changed_parent_authority() {
    let fixture = Fixture::new(false);
    let path = fixture.root.join(".git/index");
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(SMALL + 1)
        .unwrap();
    assert!(Original::open(&path, &fixture.budget, &fixture.cp).is_err());
    fixture.closed();

    fixture.write(".git/index", b"INERT-INDEX");
    let (original, bytes) = Original::open(&path, &fixture.budget, &fixture.cp).unwrap();
    assert_eq!(bytes.as_slice(), b"INERT-INDEX");
    fixture.closed();
    original.check(&fixture.budget, &fixture.cp).unwrap();
    fixture.closed();
    fs::set_permissions(fixture.root.join(".git"), fs::Permissions::from_mode(0o750)).unwrap();
    assert!(original.check(&fixture.budget, &fixture.cp).is_err());
    fixture.closed();
}

#[test]
fn evidence_is_private_immutable_and_refuses_changed_record_bytes() {
    let fixture = Fixture::new(false);
    let mut evidence = Evidence::new(&fixture.root, &fixture.budget).unwrap();
    assert_eq!(fs::metadata(evidence.path()).unwrap().mode() & 0o777, 0o700);
    evidence
        .write("request.json", b"ORIGINAL", &fixture.cp)
        .unwrap();
    let path = evidence.path().join("request.json");
    let metadata = fs::metadata(&path).unwrap();
    assert_eq!(metadata.mode() & 0o777, 0o600);
    assert_eq!(metadata.uid(), rustix::process::getuid().as_raw());
    assert_eq!(metadata.nlink(), 1);
    evidence.check(&fixture.cp).unwrap();
    assert!(
        evidence
            .write("request.json", b"REPLACED", &fixture.cp)
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap().as_slice(), b"ORIGINAL");
    fs::write(path, b"MUTATION").unwrap();
    assert!(evidence.check(&fixture.cp).is_err());
    evidence.finish().unwrap();
    fixture.closed();
}

#[test]
fn evidence_record_names_cannot_escape_the_owned_directory() {
    let fixture = Fixture::new(false);
    let mut evidence = Evidence::new(&fixture.root, &fixture.budget).unwrap();
    let absolute = fixture.root.join("absolute-escape.json");
    for name in ["", ".", "..", "../escaped.json", "nested/record.json"] {
        assert!(evidence.write(name, b"PRIVATE", &fixture.cp).is_err());
    }
    assert!(
        evidence
            .write(absolute.to_str().unwrap(), b"PRIVATE", &fixture.cp)
            .is_err()
    );
    assert!(!absolute.exists());
    assert!(!fixture.root.join("results/escaped.json").exists());
    assert_eq!(fs::read_dir(evidence.path()).unwrap().count(), 0);
    evidence.check(&fixture.cp).unwrap();
    evidence.finish().unwrap();
    fixture.closed();
}

#[test]
fn shared_results_parent_keeps_its_mode_and_each_new_evidence_leaf_is_private() {
    let fixture = Fixture::new(false);
    fixture.directory("results");
    let parent = fixture.root.join("results");
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
    let mut evidence = Evidence::new(&fixture.root, &fixture.budget).unwrap();
    assert_eq!(evidence.path().parent(), Some(parent.as_path()));
    let leaf = fs::metadata(evidence.path()).unwrap();
    assert_eq!(leaf.mode() & 0o777, 0o700);
    assert_eq!(leaf.uid(), rustix::process::getuid().as_raw());
    evidence
        .write("request.json", b"PRIVATE", &fixture.cp)
        .unwrap();
    let record = fs::metadata(evidence.path().join("request.json")).unwrap();
    assert_eq!(record.mode() & 0o777, 0o600);
    assert_eq!(record.uid(), rustix::process::getuid().as_raw());
    assert_eq!(record.nlink(), 1);
    evidence.check(&fixture.cp).unwrap();
    evidence.finish().unwrap();
    assert_eq!(fs::metadata(parent).unwrap().mode() & 0o777, 0o755);
    fixture.closed();
}

#[test]
fn unsafe_shared_results_parent_refuses_before_creating_private_records() {
    for linked in [false, true] {
        let fixture = Fixture::new(false);
        let parent = fixture.root.join("results");
        let destination = if linked {
            fixture.directory("other-results");
            let destination = fixture.root.join("other-results");
            symlink(&destination, &parent).unwrap();
            destination
        } else {
            fixture.directory("results");
            fs::set_permissions(&parent, fs::Permissions::from_mode(0o777)).unwrap();
            parent.clone()
        };
        assert!(Evidence::new(&fixture.root, &fixture.budget).is_err());
        assert_eq!(fs::read_dir(destination).unwrap().count(), 0);
        if linked {
            assert!(fs::symlink_metadata(parent).unwrap().is_symlink());
        } else {
            assert_eq!(fs::metadata(parent).unwrap().mode() & 0o777, 0o777);
        }
        fixture.closed();
    }
}
