//! Owned fixture tests for the complete transaction and shared workload lock.
use super::*;
use std::{
    cell::Cell,
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    rc::Rc,
};

pub(super) struct Fixture {
    pub path: PathBuf,
}
impl Fixture {
    pub fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("simplestchat-assets-{}", commit::nonce().unwrap()));
        fs::create_dir(&path).unwrap();
        mode(&path, 0o700);
        for name in [
            "srv",
            "srv/simplestchat-public",
            "etc",
            "etc/simplestchat-public",
            "run",
        ] {
            fs::create_dir(path.join(name)).unwrap();
            mode(&path.join(name), 0o700);
        }
        Self { path }
    }
    pub fn root(&self) -> PathBuf {
        self.path.join("srv/simplestchat-public/browser-assets")
    }
    pub fn guard(&self) -> Workload {
        self.hook(None).unwrap()
    }
    pub fn hook(&self, hook: Option<files::Hook>) -> Result<Workload> {
        Workload::fixture(&self.path, fs::metadata(&self.path).unwrap().uid(), hook)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
pub(super) fn mode(path: &Path, value: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(value)).unwrap();
}
pub(super) fn revision(byte: char) -> String {
    byte.to_string().repeat(40)
}
pub(super) fn capture(entries: &[(&str, &[u8])]) -> Capture {
    Capture(
        entries
            .iter()
            .map(|(name, bytes)| (name.to_string(), bytes.to_vec()))
            .collect(),
    )
}
pub(super) fn publish(fixture: &Fixture, revision: &str, entries: &[(&str, &[u8])], now: u64) {
    let mut guard = fixture.guard();
    Store::open(&mut guard)
        .unwrap()
        .publish_at(vec![(revision.into(), capture(entries))], now)
        .unwrap();
    guard.finish().unwrap();
}
fn initialized(fixture: &Fixture) {
    let mut guard = fixture.guard();
    assert!(Store::open(&mut guard).unwrap().initialize().unwrap());
    guard.finish().unwrap();
}
fn inspect(fixture: &Fixture) -> Result<Summary> {
    let mut guard = fixture.guard();
    let result = Store::open(&mut guard).and_then(|mut store| store.inspect());
    let finished = guard.finish();
    result.and_then(|value| finished.map(|()| value))
}
type SnapshotRow = (String, Vec<u8>, u64, u64, u32, u64, i64, i64, i64, i64);
#[derive(Debug, PartialEq, Eq)]
struct Snapshot(Vec<SnapshotRow>);
fn snapshot(root: &Path) -> Snapshot {
    fn visit(root: &Path, path: &Path, out: &mut Vec<SnapshotRow>) {
        let metadata = fs::symlink_metadata(path).unwrap();
        let bytes = if metadata.is_file() {
            fs::read(path).unwrap()
        } else if metadata.is_symlink() {
            fs::read_link(path)
                .unwrap()
                .as_os_str()
                .as_encoded_bytes()
                .to_vec()
        } else {
            vec![]
        };
        out.push((
            path.strip_prefix(root).unwrap().display().to_string(),
            bytes,
            metadata.dev(),
            metadata.ino(),
            metadata.mode(),
            metadata.nlink(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        ));
        if metadata.is_dir() {
            let mut children = fs::read_dir(path)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            children.sort();
            for child in children {
                visit(root, &child, out);
            }
        }
    }
    let mut rows = vec![];
    visit(root, root, &mut rows);
    Snapshot(rows)
}
#[test]
fn complete_retention_version_and_noop_preserve_the_real_filesystem() {
    let fixture = Fixture::new();
    initialized(&fixture);
    let mut guard = fixture.guard();
    let mut store = Store::open(&mut guard).unwrap();
    assert!(!store.initialize().unwrap());
    assert!(store.select_version(&revision('f')).unwrap());
    guard.finish().unwrap();
    publish(
        &fixture,
        &revision('a'),
        &[("z-abcdef.js", b"same"), ("a-abcdef.css", b"style")],
        100,
    );
    let before = snapshot(&fixture.root());
    publish(
        &fixture,
        &revision('a'),
        &[("z-abcdef.js", b"same"), ("a-abcdef.css", b"style")],
        100,
    );
    assert_eq!(snapshot(&fixture.root()), before);
    publish(&fixture, &revision('b'), &[("z-abcdef.js", b"same")], 101);
    let root = fixture.root();
    let union = root.join("public/assets");
    // Selected b is first; the identical URL's final source is retained a.
    assert_eq!(
        fs::metadata(union.join("z-abcdef.js")).unwrap().ino(),
        fs::metadata(root.join(revision('a')).join("z-abcdef.js"))
            .unwrap()
            .ino()
    );
    assert_ne!(
        fs::metadata(union.join("z-abcdef.js")).unwrap().ino(),
        fs::metadata(root.join(revision('b')).join("z-abcdef.js"))
            .unwrap()
            .ino()
    );
    for (index, letter) in ['c', 'd', 'e'].into_iter().enumerate() {
        publish(
            &fixture,
            &revision(letter),
            &[("z-abcdef.js", b"same")],
            102 + index as u64,
        );
    }
    assert_eq!(inspect(&fixture).unwrap().generations, 4);
    let mut guard = fixture.guard();
    let mut store = Store::open(&mut guard).unwrap();
    store.prune_at(104 + MAX_AGE).unwrap();
    assert_eq!(store.inspect().unwrap().generations, 0);
    guard.finish().unwrap();
    assert_eq!(
        fs::read(root.join("public/app-version.json")).unwrap(),
        format!("{{\"revision\":\"{}\"}}\n", revision('f')).as_bytes()
    );
}
#[test]
fn version_validation_repairs_modes_without_adopting_foreign_bytes() {
    let fixture = Fixture::new();
    initialized(&fixture);
    let selected = revision('a');
    let mut guard = fixture.guard();
    assert!(
        Store::open(&mut guard)
            .unwrap()
            .select_version(&selected)
            .unwrap()
    );
    guard.finish().unwrap();
    let path = fixture.root().join("public/app-version.json");
    let before = snapshot(&fixture.root());
    let mut guard = fixture.guard();
    assert!(
        !Store::open(&mut guard)
            .unwrap()
            .select_version(&selected)
            .unwrap()
    );
    guard.finish().unwrap();
    assert_eq!(snapshot(&fixture.root()), before);
    let inode = fs::metadata(&path).unwrap().ino();
    mode(&path, 0o600);
    let mut guard = fixture.guard();
    assert!(
        Store::open(&mut guard)
            .unwrap()
            .select_version(&selected)
            .unwrap()
    );
    guard.finish().unwrap();
    assert_ne!(fs::metadata(&path).unwrap().ino(), inode);
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o644);
    fs::write(&path, format!(" {{ \"revision\": \"{selected}\" }} ")).unwrap();
    mode(&path, 0o644);
    let mut guard = fixture.guard();
    assert!(
        Store::open(&mut guard)
            .unwrap()
            .select_version(&selected)
            .unwrap()
    );
    guard.finish().unwrap();
    mode(&path, 0o666);
    assert!(inspect(&fixture).is_err());
}
#[test]
fn declaration_capacity_and_immutable_collision_refuse_without_publishing() {
    let fixture = Fixture::new();
    publish(&fixture, &revision('a'), &[("app-abcdef.js", b"one")], 10);
    let before = snapshot(&fixture.root());
    let mut guard = fixture.guard();
    let result = Store::open(&mut guard).unwrap().publish_at(
        vec![(revision('b'), capture(&[("app-abcdef.js", b"two")]))],
        11,
    );
    assert_eq!(result.err().unwrap().code(), "asset_immutable_collision");
    guard.finish().unwrap();
    assert_eq!(snapshot(&fixture.root()), before);
    let mut guard = fixture.guard();
    let result = Store::open(&mut guard).unwrap().publish_at(
        vec![(revision('a'), capture(&[("app-abcdef.js", b"two")]))],
        11,
    );
    assert_eq!(result.err().unwrap().code(), "asset_revision_changed");
    guard.finish().unwrap();
    assert_eq!(snapshot(&fixture.root()), before);
}
fn fail_once(event: &'static str, suffix: &'static str) -> files::Hook {
    let fired = Cell::new(false);
    Rc::new(move |observed, path| {
        if observed == event && path.ends_with(suffix) && !fired.replace(true) {
            Err(fail("test_primary"))
        } else {
            Ok(())
        }
    })
}
#[test]
fn pre_pointer_failures_restore_prior_index_and_keep_old_public_payload() {
    for event in ["rename-file", "renamed-file", "rename-pointer"] {
        let fixture = Fixture::new();
        publish(&fixture, &revision('a'), &[("old-abcdef.js", b"old")], 1);
        let old_index = fs::read(fixture.root().join("index.json")).unwrap();
        let old_pointer = fs::read_link(fixture.root().join("public/assets")).unwrap();
        let suffix = if event == "rename-pointer" {
            "public/assets"
        } else {
            "index.json"
        };
        let mut guard = fixture.hook(Some(fail_once(event, suffix))).unwrap();
        let result = Store::open(&mut guard).unwrap().publish_at(
            vec![(revision('b'), capture(&[("new-abcdef.js", b"new")]))],
            2,
        );
        assert_eq!(result.err().unwrap().code(), "test_primary");
        guard.finish().unwrap();
        assert_eq!(
            fs::read(fixture.root().join("index.json")).unwrap(),
            old_index
        );
        assert_eq!(
            fs::read_link(fixture.root().join("public/assets")).unwrap(),
            old_pointer
        );
        assert_eq!(inspect(&fixture).unwrap().generations, 1);
        assert!(!fixture.root().join(revision('b')).exists());
    }
}
#[test]
fn visible_pointer_failure_leaves_evidence_and_never_rolls_back_live_union() {
    let fixture = Fixture::new();
    publish(&fixture, &revision('a'), &[("old-abcdef.js", b"old")], 1);
    let old = fs::read_link(fixture.root().join("public/assets")).unwrap();
    let mut guard = fixture.hook(Some(fail_once("sync", "public/"))).unwrap();
    let result = Store::open(&mut guard).unwrap().publish_at(
        vec![(revision('b'), capture(&[("new-abcdef.js", b"new")]))],
        2,
    );
    assert_eq!(result.err().unwrap().code(), "test_primary");
    assert!(guard.finish().is_err());
    assert_ne!(
        fs::read_link(fixture.root().join("public/assets")).unwrap(),
        old
    );
    assert_eq!(
        fs::read(fixture.root().join("public/assets/new-abcdef.js")).unwrap(),
        b"new"
    );
    assert!(inspect(&fixture).is_err());
}
#[test]
fn same_kernel_flock_and_journal_gates_are_held_without_store_side_effects() {
    let fixture = Fixture::new();
    let guard = fixture.guard();
    assert_eq!(
        fixture.hook(None).err().unwrap().code(),
        "asset_workload_busy"
    );
    assert!(!fixture.root().exists());
    guard.finish().unwrap();
    for journal in [
        "run/simplestchat-bench/current.json",
        "srv/simplestchat-public/release-state.json",
    ] {
        let path = fixture.path.join(journal);
        for bytes in [
            b"{\"schemaVersion\":true,\"finalized\":true}".as_slice(),
            b"{\"schemaVersion\":1.0,\"finalized\":true}",
            b"{\"schemaVersion\":1,\"finalized\":false}",
        ] {
            fs::write(&path, bytes).unwrap();
            mode(&path, 0o600);
            assert!(fixture.hook(None).is_err());
        }
        fs::write(
            &path,
            b"{\"schemaVersion\":1,\"finalized\":true,\"private\":\"value\"}",
        )
        .unwrap();
        mode(&path, 0o600);
        fixture.guard().finish().unwrap();
        fs::remove_file(path).unwrap();
    }
}
#[test]
fn existing_special_files_and_check_to_open_replacements_are_never_payloads() {
    let fixture = Fixture::new();
    initialized(&fixture);
    let index = fixture.root().join("index.json");
    fs::remove_file(&index).unwrap();
    nix::unistd::mkfifo(
        &index,
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .unwrap();
    let opened = Rc::new(Cell::new(false));
    let observed = opened.clone();
    let guard = fixture
        .hook(Some(Rc::new(move |event, path| {
            if event == "open-read" && path.ends_with("index.json") {
                observed.set(true);
            }
            Ok(())
        })))
        .unwrap();
    let mut guard = guard;
    assert!(Store::open(&mut guard).unwrap().inspect().is_err());
    assert!(guard.finish().is_err());
    assert!(!opened.get());

    let fixture = Fixture::new();
    initialized(&fixture);
    let original = fixture.root().join("index.json");
    let parked = fixture.root().join("parked");
    let fired = Rc::new(Cell::new(false));
    let seen = fired.clone();
    let replacement = original.clone();
    let mut guard = fixture
        .hook(Some(Rc::new(move |event, path| {
            if event == "open-read" && path.ends_with("index.json") && !seen.replace(true) {
                fs::rename(&replacement, &parked).unwrap();
                fs::write(&replacement, b"PRIVATE_REPLACEMENT").unwrap();
                mode(&replacement, 0o644);
            }
            Ok(())
        })))
        .unwrap();
    assert!(Store::open(&mut guard).unwrap().inspect().is_err());
    let _ = guard.finish();
    assert!(fired.get());
    assert_eq!(fs::read(original).unwrap(), b"PRIVATE_REPLACEMENT");
}
#[test]
fn late_cleanup_failures_keep_first_primary_and_attempt_all_other_closes() {
    let fixture = Fixture::new();
    publish(&fixture, &revision('a'), &[("old-abcdef.js", b"old")], 1);
    let armed = Rc::new(Cell::new(false));
    let ready = armed.clone();
    let closes = Rc::new(Cell::new(0usize));
    let count = closes.clone();
    let mut guard = fixture
        .hook(Some(Rc::new(move |event, path| {
            if ready.get() && event == "close" {
                count.set(count.get() + 1);
            }
            if event == "rename-pointer" {
                ready.set(true);
                return Err(fail("first_publication"));
            }
            if ready.get() && event == "remove-directory" && path.contains("union-") {
                return Err(fail("later_cleanup"));
            }
            Ok(())
        })))
        .unwrap();
    let result = Store::open(&mut guard).unwrap().publish_at(
        vec![(revision('b'), capture(&[("new-abcdef.js", b"new")]))],
        2,
    );
    assert_eq!(result.err().unwrap().code(), "first_publication");
    let _ = guard.finish();
    assert!(armed.get() && closes.get() > 8);
    assert!(inspect(&fixture).is_err());
}
#[test]
fn pending_and_old_union_cleanup_failures_are_evidence_not_adoptable_state() {
    for suffix in ["pending", "old-union"] {
        let fixture = Fixture::new();
        publish(&fixture, &revision('a'), &[("old-abcdef.js", b"old")], 1);
        let old_union = fs::read_link(fixture.root().join("public/assets"))
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let fired = Rc::new(Cell::new(false));
        let seen = fired.clone();
        let mut guard = fixture
            .hook(Some(Rc::new(move |event, path| {
                let selected = if suffix == "pending" {
                    path.contains(".pending-")
                } else {
                    path.ends_with(&old_union)
                };
                if event == "remove-directory" && selected && !seen.replace(true) {
                    Err(fail("cleanup_failed"))
                } else {
                    Ok(())
                }
            })))
            .unwrap();
        let result = Store::open(&mut guard).unwrap().publish_at(
            vec![(revision('b'), capture(&[("new-abcdef.js", b"new")]))],
            2,
        );
        assert_eq!(result.err().unwrap().code(), "cleanup_failed");
        let _ = guard.finish();
        assert!(fired.get());
        assert!(inspect(&fixture).is_err());
        assert_eq!(
            fs::read(fixture.root().join("public/assets/new-abcdef.js")).unwrap(),
            b"new"
        );
    }
}
#[test]
fn foreign_substituted_generation_is_not_removed_by_rollback() {
    let fixture = Fixture::new();
    publish(&fixture, &revision('a'), &[("old-abcdef.js", b"old")], 1);
    let generation = fixture.root().join(revision('b'));
    let parked = fixture.root().join("foreign-parked");
    let replacement = generation.clone();
    let fired = Rc::new(Cell::new(false));
    let seen = fired.clone();
    let mut guard = fixture
        .hook(Some(Rc::new(move |event, _| {
            if event == "rename-pointer" && !seen.replace(true) {
                fs::rename(&replacement, &parked).unwrap();
                fs::create_dir(&replacement).unwrap();
                mode(&replacement, 0o755);
                fs::write(replacement.join("foreign"), b"PRIVATE_FOREIGN").unwrap();
                return Err(fail("publication_failed"));
            }
            Ok(())
        })))
        .unwrap();
    let result = Store::open(&mut guard).unwrap().publish_at(
        vec![(revision('b'), capture(&[("new-abcdef.js", b"new")]))],
        2,
    );
    assert_eq!(result.err().unwrap().code(), "publication_failed");
    let _ = guard.finish();
    assert_eq!(
        fs::read(generation.join("foreign")).unwrap(),
        b"PRIVATE_FOREIGN"
    );
}
#[test]
fn numeric_schema_aliases_and_negative_zero_initialize_without_rewriting_any_inode() {
    for schema in ["true", "1.0"] {
        let fixture = Fixture::new();
        publish(&fixture, &revision('a'), &[("app-abcdef.js", b"one")], 0);
        super::oracle_tests::native(
            &fixture,
            &serde_json::json!({"action":"schema","value":schema,"created":"-0"}),
        )
        .unwrap();
        let before = snapshot(&fixture.root());
        let mut guard = fixture.guard();
        assert!(!Store::open(&mut guard).unwrap().initialize().unwrap());
        guard.finish().unwrap();
        assert_eq!(snapshot(&fixture.root()), before);
    }
}
#[test]
fn whole_declared_budget_is_refused_before_any_asset_payload_open() {
    let fixture = Fixture::new();
    publish(
        &fixture,
        &revision('a'),
        &[
            ("a-abcdef.js", b"1"),
            ("b-abcdef.js", b"2"),
            ("c-abcdef.js", b"3"),
            ("d-abcdef.js", b"4"),
            ("e-abcdef.js", b"5"),
        ],
        0,
    );
    let path = fixture.root().join("index.json");
    let value = fs::read_to_string(&path)
        .unwrap()
        .replace("\"size\":1", &format!("\"size\":{MAX_FILE}"));
    fs::write(&path, value).unwrap();
    let payload = Rc::new(Cell::new(false));
    let opened = payload.clone();
    let needle = revision('a');
    let mut guard = fixture
        .hook(Some(Rc::new(move |event, path| {
            if event == "open-read" && path.contains(&needle) {
                opened.set(true);
            }
            Ok(())
        })))
        .unwrap();
    assert_eq!(
        Store::open(&mut guard)
            .unwrap()
            .inspect()
            .err()
            .unwrap()
            .code(),
        "asset_index_total"
    );
    let _ = guard.finish();
    assert!(!payload.get());
    let fixture = Fixture::new();
    let mut guard = fixture.guard();
    assert_eq!(
        Store::open(&mut guard)
            .unwrap()
            .publish_at(
                vec![(revision('a'), capture(&[("a-abcdef.js", b"1")]))],
                u64::MAX
            )
            .err()
            .unwrap()
            .code(),
        "asset_capture_count"
    );
    guard.finish().unwrap();
    assert!(!fixture.root().exists());
}
#[test]
fn pointer_after_effect_error_cannot_trigger_preswitch_rollback() {
    let fixture = Fixture::new();
    publish(&fixture, &revision('a'), &[("old-abcdef.js", b"old")], 1);
    let mut guard = fixture
        .hook(Some(fail_once("renamed-pointer", "public/assets")))
        .unwrap();
    let result = Store::open(&mut guard).unwrap().publish_at(
        vec![(revision('b'), capture(&[("new-abcdef.js", b"new")]))],
        2,
    );
    assert_eq!(result.err().unwrap().code(), "test_primary");
    let _ = guard.finish();
    assert_eq!(
        fs::read(fixture.root().join("public/assets/new-abcdef.js")).unwrap(),
        b"new"
    );
    assert!(fixture.root().join(revision('b')).exists());
    assert!(inspect(&fixture).is_err());
}
#[test]
fn lock_binding_is_rechecked_after_acquisition_and_busy_closes_are_checked() {
    let fixture = Fixture::new();
    fixture.guard().finish().unwrap();
    let lock = fixture.path.join("run/simplestchat-bench/workload.lock");
    let parked = fixture.path.join("retired-lock");
    let replaced = lock.clone();
    let guard = fixture.hook(Some(Rc::new(move |event, _| {
        if event == "locked" {
            fs::rename(&replaced, &parked).unwrap();
            fs::write(&replaced, b"").unwrap();
            mode(&replaced, 0o600);
        }
        Ok(())
    })));
    assert!(guard.is_err());
    assert!(!fixture.root().exists());
    let owner = fixture.guard();
    let close = Rc::new(Cell::new(0));
    let calls = close.clone();
    let contended = Rc::new(Cell::new(false));
    let busy = contended.clone();
    let result = fixture.hook(Some(Rc::new(move |event, _| {
        if event == "busy" {
            busy.set(true);
        }
        if busy.get() && event == "close" {
            calls.set(calls.get() + 1);
            Err(fail("checked_close_failure"))
        } else {
            Ok(())
        }
    })));
    assert!(!result.err().unwrap().busy());
    assert!(contended.get() && close.get() > 0);
    owner.finish().unwrap();
}
#[test]
fn final_close_failures_still_close_every_retained_file_owner() {
    let fixture = Fixture::new();
    let armed = Rc::new(Cell::new(false));
    let active = armed.clone();
    let closes = Rc::new(Cell::new(0usize));
    let count = closes.clone();
    let mut guard = fixture
        .hook(Some(Rc::new(move |event, _| {
            if active.get() && event == "close" {
                count.set(count.get() + 1);
                Err(fail("final_close_failed"))
            } else {
                Ok(())
            }
        })))
        .unwrap();
    Store::open(&mut guard).unwrap().initialize().unwrap();
    let owners = guard.owners();
    armed.set(true);
    assert_eq!(guard.finish().err().unwrap().code(), "final_close_failed");
    assert!(closes.get() >= owners.len());
    for (fd, original) in owners {
        // No unsafe borrowed descriptor is reconstructed. /dev/fd metadata may
        // be absent or refer to a subsequently reused FD, never this owner.
        if let Ok(observed) = fs::metadata(format!("/dev/fd/{fd}")) {
            assert!((observed.dev(), observed.ino()) != (original.dev(), original.ino()));
        }
    }
    fixture.guard().finish().unwrap();
}
#[test]
fn required_captures_and_stable_timestamp_ties_choose_the_exact_bounded_history() {
    let identity = model::Record {
        sha256: model::sha(b"x"),
        size: MAX_FILE,
    };
    let prior = ['a', 'b', 'c', 'd']
        .into_iter()
        .map(|letter| model::Generation {
            revision: revision(letter),
            created: 10,
            expires: 10 + MAX_AGE,
            assets: vec![(format!("{letter}-abcdef.js"), identity.clone())],
        })
        .collect::<Vec<_>>();
    let required = capture(&[("new-abcdef.js", b"new")]);
    let selected = model::select(&prior, &[(revision('e'), required)], 11).unwrap();
    assert_eq!(
        selected
            .iter()
            .map(|item| item.revision.clone())
            .collect::<Vec<_>>(),
        ['e', 'a', 'b', 'c'].map(revision)
    );
    // A required pair is never evicted to make room for historical entries.
    let required = vec![('e', "left-abcdef.js"), ('f', "right-abcdef.js")]
        .into_iter()
        .map(|(letter, name)| {
            (
                revision(letter),
                Capture(vec![(name.into(), vec![1; MAX_FILE as usize])]),
            )
        })
        .collect::<Vec<_>>();
    let selected = model::select(&prior, &required, 11).unwrap();
    assert_eq!(
        selected
            .iter()
            .map(|item| item.revision.clone())
            .collect::<Vec<_>>(),
        ['e', 'f', 'a', 'b'].map(revision)
    );
    assert_eq!(
        selected.iter().map(model::Generation::bytes).sum::<u64>(),
        MAX_TOTAL
    );
}
