use super::files::{Artifacts, Output};
use serde_json::json;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt, symlink},
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Temporary(PathBuf);
impl Temporary {
    fn new() -> Self {
        let p = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "public-smoke-files-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::DirBuilder::new().mode(0o700).create(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
#[test]
fn real_private_publication_retains_original_inode_across_rename_and_explicit_finish() {
    let tmp = Temporary::new();
    let path = tmp.0.join("output");
    let mut output = Output::new(&path, false).unwrap();
    output
        .publish(&json!({"schemaVersion":1,"passed":true}))
        .unwrap();
    output.finish().unwrap();
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o700);
    let m = fs::metadata(path.join("summary.json")).unwrap();
    assert_eq!(m.mode() & 0o7777, 0o600);
    assert_eq!(m.nlink(), 1);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(path.join("summary.json")).unwrap())
            .unwrap()["passed"],
        true
    );
    assert!(!path.join("summary.json.part").exists());
}
#[test]
fn existing_foreign_names_and_replaced_summary_are_refused_without_deleting_them() {
    let tmp = Temporary::new();
    let path = tmp.0.join("output");
    let mut output = Output::new(&path, false).unwrap();
    assert!(Output::new(&path, false).is_err());
    output.publish(&json!({"passed":false})).unwrap();
    fs::rename(path.join("summary.json"), path.join("original")).unwrap();
    fs::write(path.join("summary.json"), b"FOREIGN").unwrap();
    assert!(output.finish().is_err());
    assert_eq!(fs::read(path.join("summary.json")).unwrap(), b"FOREIGN");
}
#[test]
fn nofollow_parent_and_exact_default_parent_privacy_are_enforced() {
    let tmp = Temporary::new();
    let alias = tmp.0.join("alias");
    symlink(&tmp.0, &alias).unwrap();
    assert!(Output::new(&alias.join("refused"), false).is_err());
    assert!(!tmp.0.join("refused").exists());
    let output = Output::new(&tmp.0.join("results/fresh"), true).unwrap();
    output.finish().unwrap();
    assert_eq!(
        fs::metadata(tmp.0.join("results")).unwrap().mode() & 0o7777,
        0o700
    );
}
