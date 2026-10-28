//! Whole first-install transactions over actual owned files. Only platform,
//! root/UID projection, clock and fault hooks are private fixtures. No candidate
//! is executed and no developer host's managed root is opened.

use super::*;
use crate::{distribution::fixture as elf, test_support::Fixture as Outer};
use files::{Hook, Ledger};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::PathBuf,
    rc::Rc,
};

use crate::distribution::manifest::tests::fixture as contract;

struct FixtureHost {
    root: PathBuf,
    platforms: Cell<usize>,
    denied: Cell<bool>,
}
impl Host for FixtureHost {
    fn platform(&self) -> Result<()> {
        self.platforms.set(self.platforms.get() + 1);
        if self.denied.get() {
            Err(Failure::Unproven)
        } else {
            Ok(())
        }
    }
    fn lock(&self) -> Result<Workload> {
        Workload::install_fixture(&self.root, rustix::process::getuid().as_raw()).map_err(|error| {
            if error.busy() {
                Failure::Busy
            } else {
                Failure::Unproven
            }
        })
    }
}
struct Fixture {
    _outer: Outer,
    host: FixtureHost,
    value: Value,
    selected: String,
}
fn write(path: &Path, bytes: &[u8], mode: u32) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}
impl Fixture {
    fn new() -> Self {
        let outer = Outer::new();
        let root = outer.0.join("host");
        for (name, mode) in [
            ("", 0o755),
            ("input", 0o700),
            ("etc", 0o755),
            ("etc/simplestchat-public", 0o700),
            ("srv", 0o755),
            ("srv/simplestchat-public", 0o700),
            ("run", 0o755),
            ("usr", 0o755),
            ("usr/local", 0o755),
            ("usr/local/libexec", 0o755),
            (request::DESTINATION, 0o755),
        ] {
            let path = root.join(name);
            fs::create_dir(&path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        }
        let binary = elf::fixture(true, false, false);
        let manifest = serde_json::to_vec(&contract::manifest(&binary)).unwrap();
        let selection = serde_json::to_vec(&contract::selection()).unwrap();
        write(&root.join("input/ops-release.json"), &manifest, 0o600);
        write(&root.join("input/ops-selection.json"), &selection, 0o600);
        write(&root.join("input/simplestchat-ops"), &binary, 0o755);
        let pin = |name: &str, bytes: &[u8]| {
            json!({
                "path":format!("/input/{name}"),"sha256":files::sha(bytes),"bytes":bytes.len(),
            })
        };
        let value = json!({"schemaVersion":1,
            "manifest":pin("ops-release.json", &manifest),
            "selection":pin("ops-selection.json", &selection),
            "binary":pin("simplestchat-ops", &binary)});
        let mut value = Self {
            _outer: outer,
            host: FixtureHost {
                root,
                platforms: 0.into(),
                denied: false.into(),
            },
            value,
            selected: String::new(),
        };
        value.save();
        value
    }
    fn save(&mut self) {
        let bytes = serde_json::to_vec(&self.value).unwrap();
        self.raw(&bytes);
    }
    fn raw(&mut self, bytes: &[u8]) {
        write(
            &self.host.root.join("input/install-request.json"),
            bytes,
            0o600,
        );
        self.selected = files::sha(bytes);
    }
    fn final_path(&self) -> PathBuf {
        self.host
            .root
            .join(request::DESTINATION)
            .join(request::NAME)
    }
    fn temp_path(&self) -> PathBuf {
        self.host
            .root
            .join(request::DESTINATION)
            .join(".simplestchat-ops.installing")
    }
    fn access(&self, hook: Option<Hook>) -> (Access, Rc<RefCell<Ledger>>) {
        let ledger = Rc::new(RefCell::new(Ledger::default()));
        (
            Access {
                root: Some(self.host.root.clone()),
                hook,
                ledger: Some(ledger.clone()),
            },
            ledger,
        )
    }
    fn invoke(&self, hook: Option<Hook>, owner: Owner) -> Result<Receipt> {
        self.invoke_mode(hook, owner, false)
    }
    fn invoke_upgrade(&self, hook: Option<Hook>, owner: Owner) -> Result<Receipt> {
        self.invoke_mode(hook, owner, true)
    }
    fn invoke_mode(&self, hook: Option<Hook>, owner: Owner, upgrade: bool) -> Result<Receipt> {
        let (access, ledger) = self.access(hook);
        let value = transact(
            Path::new("/input/install-request.json"),
            &self.selected,
            access,
            &self.host,
            owner,
            upgrade,
        );
        let ledger = ledger.borrow();
        assert_eq!(
            ledger.opened, ledger.closed,
            "every acquired/probe File gets an explicit close"
        );
        assert!(ledger.live.is_empty());
        value
    }
    fn install(&self) -> Receipt {
        self.invoke(None, Owner::fixture()).unwrap()
    }
    fn prepare_upgrade(&mut self) -> Vec<u8> {
        let mut installed = fs::read(self.host.root.join("input/simplestchat-ops")).unwrap();
        // Both inputs remain inert ELF metadata. This byte is not a header or
        // loader policy field; no fixture executable is ever run.
        installed[3000] ^= 1;
        write(&self.final_path(), &installed, 0o755);
        self.value["schemaVersion"] = 2.into();
        self.value["installedBinary"] = json!({"sha256":files::sha(&installed),
            "bytes":installed.len()});
        self.save();
        installed
    }
}

type Stamp = (u64, u64, u64, u32, u32, u32, i64, i64, i64, i64, Vec<u8>);
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Stamp> {
    fn visit(path: &Path, out: &mut BTreeMap<PathBuf, Stamp>) {
        let stamp = fs::symlink_metadata(path).unwrap();
        let bytes = if stamp.is_file() {
            fs::read(path).unwrap()
        } else if stamp.is_symlink() {
            fs::read_link(path)
                .unwrap()
                .as_os_str()
                .as_encoded_bytes()
                .to_vec()
        } else {
            Vec::new()
        };
        out.insert(
            path.into(),
            (
                stamp.dev(),
                stamp.ino(),
                stamp.nlink(),
                stamp.mode(),
                stamp.uid(),
                stamp.gid(),
                stamp.mtime(),
                stamp.mtime_nsec(),
                stamp.ctime(),
                stamp.ctime_nsec(),
                bytes,
            ),
        );
        if stamp.is_dir() {
            for child in fs::read_dir(path).unwrap() {
                visit(&child.unwrap().path(), out);
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, &mut out);
    out
}

#[test]
fn first_install_then_exact_noop_preserves_entire_filesystem_and_original_pins() {
    let f = Fixture::new();
    let first = f.install();
    assert!(first.changed());
    let value = serde_json::to_value(first).unwrap();
    assert_eq!(
        value,
        json!({"schemaVersion":1,"passed":true,"changed":true,
        "manifestSha256":f.value["manifest"]["sha256"],"manifestBytes":f.value["manifest"]["bytes"],
        "selectionSha256":f.value["selection"]["sha256"],
        "selectionBytes":f.value["selection"]["bytes"],
        "binarySha256":f.value["binary"]["sha256"],"binaryBytes":f.value["binary"]["bytes"]})
    );
    let stamp = fs::metadata(f.final_path()).unwrap();
    assert_eq!(stamp.mode() & 0o7777, 0o755);
    assert_eq!(stamp.nlink(), 1);
    assert_eq!(
        fs::read(f.final_path()).unwrap(),
        fs::read(f.host.root.join("input/simplestchat-ops")).unwrap()
    );
    assert!(!f.temp_path().exists());
    let before = snapshot(&f.host.root);
    assert!(!f.install().changed());
    assert_eq!(snapshot(&f.host.root), before);
}

#[test]
fn explicit_upgrade_exchanges_only_expected_old_then_exact_repeat_is_a_noop() {
    let mut f = Fixture::new();
    let installed = f.prepare_upgrade();
    let old = fs::metadata(f.final_path()).unwrap();
    assert!(
        f.invoke(None, Owner::fixture()).is_err(),
        "install must not upgrade"
    );
    assert_eq!(fs::read(f.final_path()).unwrap(), installed);
    let result = f.invoke_upgrade(None, Owner::fixture()).unwrap();
    assert!(result.changed());
    assert_ne!(old.ino(), fs::metadata(f.final_path()).unwrap().ino());
    assert_eq!(
        fs::read(f.final_path()).unwrap(),
        fs::read(f.host.root.join("input/simplestchat-ops")).unwrap()
    );
    assert!(!f.temp_path().exists());
    let before = snapshot(&f.host.root);
    assert!(!f.invoke_upgrade(None, Owner::fixture()).unwrap().changed());
    assert_eq!(before, snapshot(&f.host.root));
}

#[test]
fn upgrade_request_is_closed_and_old_pin_is_not_an_authentication_claim() {
    let mut f = Fixture::new();
    f.prepare_upgrade();
    for (field, value) in [
        ("schemaVersion", json!(1)),
        ("schemaVersion", json!(2.0)),
        ("schemaVersion", json!(true)),
        ("authenticated", json!(true)),
    ] {
        let mut request = f.value.clone();
        request[field] = value;
        assert!(UpgradeRequest::decode(&serde_json::to_vec(&request).unwrap()).is_err());
    }
    for value in [
        json!(null),
        json!({}),
        json!({"sha256":"a".repeat(64),"bytes":0}),
        json!({"sha256":"a".repeat(64),"bytes":true}),
        json!({"sha256":"A".repeat(64),"bytes":4096}),
        json!({"sha256":"a".repeat(64),"bytes":4096,"path":"/foreign"}),
        json!({"sha256":f.value["binary"]["sha256"],"bytes":4096}),
    ] {
        let mut request = f.value.clone();
        request["installedBinary"] = value;
        assert!(UpgradeRequest::decode(&serde_json::to_vec(&request).unwrap()).is_err());
    }
    let duplicate = serde_json::to_string(&f.value).unwrap().replacen(
        "\"schemaVersion\":2",
        "\"schemaVersion\":2,\"schemaVersion\":2",
        1,
    );
    assert!(UpgradeRequest::decode(duplicate.as_bytes()).is_err());
}

#[test]
fn upgrade_refuses_absence_foreign_identity_modes_links_pending_and_unfinished_work() {
    for kind in [
        "missing", "bytes", "mode", "symlink", "hardlink", "pending", "journal",
    ] {
        let mut f = Fixture::new();
        f.prepare_upgrade();
        match kind {
            "missing" => fs::remove_file(f.final_path()).unwrap(),
            "bytes" => {
                let mut bytes = fs::read(f.final_path()).unwrap();
                bytes[3001] ^= 1;
                write(&f.final_path(), &bytes, 0o755);
            }
            "mode" => {
                fs::set_permissions(f.final_path(), fs::Permissions::from_mode(0o4755)).unwrap()
            }
            "symlink" => {
                fs::rename(f.final_path(), f.final_path().with_extension("saved")).unwrap();
                symlink(f.final_path().with_extension("saved"), f.final_path()).unwrap();
            }
            "hardlink" => {
                fs::hard_link(f.final_path(), f.final_path().with_extension("alias")).unwrap()
            }
            "pending" => write(&f.temp_path(), b"FOREIGN_PENDING", 0o600),
            _ => {
                let held = f.host.lock().unwrap();
                held.finish().unwrap();
                write(
                    &f.host.root.join("run/simplestchat-bench/current.json"),
                    b"{\"schemaVersion\":1,\"finalized\":false}",
                    0o600,
                );
            }
        }
        let before = snapshot(&f.host.root);
        assert!(f.invoke_upgrade(None, Owner::fixture()).is_err(), "{kind}");
        let after = snapshot(&f.host.root);
        if kind != "missing" {
            assert_eq!(before[&f.final_path()], after[&f.final_path()], "{kind}");
        }
        if kind == "pending" {
            assert_eq!(fs::read(f.temp_path()).unwrap(), b"FOREIGN_PENDING");
        }
    }
    let mut f = Fixture::new();
    f.prepare_upgrade();
    let held = f.host.lock().unwrap();
    assert_eq!(
        f.invoke_upgrade(None, Owner::fixture()).unwrap_err(),
        Failure::Busy
    );
    held.finish().unwrap();
}

#[test]
fn upgrade_faults_preserve_old_before_exchange_and_candidate_after_publication() {
    for event in [
        "created",
        "write",
        "fsync-file",
        "writers-closed",
        "after-chmod",
        "before-publish",
        "after-publish",
        "before-retire-upgrade",
        "after-retire-upgrade",
        "fsync-parent",
    ] {
        let mut f = Fixture::new();
        let old = f.prepare_upgrade();
        let fired = Rc::new(Cell::new(false));
        let flag = fired.clone();
        let hook: Hook = Rc::new(move |actual, _| {
            if actual == event && !flag.replace(true) {
                Err(Failure::Unproven)
            } else {
                Ok(())
            }
        });
        assert_eq!(
            f.invoke_upgrade(Some(hook), Owner::fixture()).unwrap_err(),
            Failure::Unproven,
            "{event}"
        );
        assert!(fired.get(), "{event}");
        let expected = if matches!(
            event,
            "after-publish" | "before-retire-upgrade" | "after-retire-upgrade" | "fsync-parent"
        ) {
            fs::read(f.host.root.join("input/simplestchat-ops")).unwrap()
        } else {
            old
        };
        assert_eq!(fs::read(f.final_path()).unwrap(), expected, "{event}");
        assert!(!f.temp_path().exists(), "{event}");
    }
}

#[test]
fn upgrade_rejoins_old_leaf_after_fault_hooks_and_never_retires_foreign_names() {
    for event in ["before-publish", "before-retire-upgrade"] {
        let mut f = Fixture::new();
        let old = f.prepare_upgrade();
        let selected = if event == "before-publish" {
            f.final_path()
        } else {
            f.temp_path()
        };
        let saved = selected.with_extension("saved");
        let flag = Rc::new(Cell::new(false));
        let fired = flag.clone();
        let hook: Hook = Rc::new(move |actual, _| {
            if actual == event && !fired.replace(true) {
                fs::rename(&selected, &saved).unwrap();
                write(&selected, b"FOREIGN_REPLACEMENT", 0o755);
            }
            Ok(())
        });
        assert!(f.invoke_upgrade(Some(hook), Owner::fixture()).is_err());
        assert!(flag.get());
        let selected = if event == "before-publish" {
            f.final_path()
        } else {
            f.temp_path()
        };
        assert_eq!(fs::read(selected).unwrap(), b"FOREIGN_REPLACEMENT");
        assert_eq!(
            fs::read(if event == "before-publish" {
                f.final_path().with_extension("saved")
            } else {
                f.temp_path().with_extension("saved")
            })
            .unwrap(),
            old
        );
        if event == "before-retire-upgrade" {
            assert_eq!(
                files::sha(&fs::read(f.final_path()).unwrap()),
                f.value["binary"]["sha256"]
            );
        }
    }
}

#[test]
fn upgrade_checked_close_failures_remain_negative_after_all_peer_close_attempts() {
    let mut baseline = Fixture::new();
    baseline.prepare_upgrade();
    let (access, ledger) = baseline.access(None);
    transact(
        Path::new("/input/install-request.json"),
        &baseline.selected,
        access,
        &baseline.host,
        Owner::fixture(),
        true,
    )
    .unwrap();
    let total = ledger.borrow().closed;
    for failed in 1..=total {
        let mut f = Fixture::new();
        f.prepare_upgrade();
        let count = Rc::new(Cell::new(0));
        let seen = count.clone();
        let hook: Hook = Rc::new(move |event, _| {
            if event == "close" {
                seen.set(seen.get() + 1);
                if seen.get() == failed {
                    return Err(Failure::Unproven);
                }
            }
            Ok(())
        });
        assert!(
            f.invoke_upgrade(Some(hook), Owner::fixture()).is_err(),
            "close{failed}"
        );
        assert!(count.get() >= failed);
    }
}

#[test]
fn cancellation_after_upgrade_keeps_candidate_and_refuses_unretired_pending_state() {
    let mut f = Fixture::new();
    let old = f.prepare_upgrade();
    let pending = Rc::new(Cell::new(false));
    let signal = pending.clone();
    let mut owner = Owner::fixture();
    let observed = pending.clone();
    owner.pending = Some(Rc::new(move || {
        if observed.get() {
            Err(Failure::Interrupted)
        } else {
            Ok(())
        }
    }));
    let hook: Hook = Rc::new(move |event, _| {
        if event == "after-publish" {
            signal.set(true);
        }
        Ok(())
    });
    assert_eq!(
        f.invoke_upgrade(Some(hook), owner).unwrap_err(),
        Failure::Interrupted
    );
    assert_eq!(
        files::sha(&fs::read(f.final_path()).unwrap()),
        f.value["binary"]["sha256"]
    );
    assert_eq!(fs::read(f.temp_path()).unwrap(), old);
    assert!(f.invoke_upgrade(None, Owner::fixture()).is_err());
    assert_eq!(fs::read(f.temp_path()).unwrap(), old);
}

#[test]
fn exact_request_pin_and_closed_fields_precede_platform_or_managed_access() {
    let mut f = Fixture::new();
    f.selected = "a".repeat(64);
    assert_eq!(
        f.invoke(None, Owner::fixture()).unwrap_err(),
        Failure::Unproven
    );
    assert_eq!(f.host.platforms.get(), 0);
    assert!(!f.host.root.join("run/simplestchat-bench").exists());
    for raw in [
        b"[]".as_slice(),
        b"{\"schemaVersion\":1,\"schemaVersion\":1}",
        b"{\"PRIVATE_SENTINEL\":true}",
        b"\xef\xbb\xbf{}",
        b"\x00{}",
    ] {
        f.raw(raw);
        assert!(f.invoke(None, Owner::fixture()).is_err());
        assert_eq!(f.host.platforms.get(), 0);
    }
    let f = Fixture::new();
    for key in ["manifest", "selection", "binary"] {
        for field in ["path", "sha256", "bytes"] {
            let mut value = f.value.clone();
            value[key].as_object_mut().unwrap().remove(field);
            assert!(Request::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        }
        for bad in [json!(true), json!(1.0), json!(-1), json!(0), json!("1")] {
            let mut value = f.value.clone();
            value[key]["bytes"] = bad;
            assert!(Request::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        }
        for path in [
            "relative",
            "/a/../private",
            "/a//private",
            "/a/./private",
            "/usr/local/libexec/simplestchat-public/input",
            "/private\nvalue",
        ] {
            let mut value = f.value.clone();
            value[key]["path"] = path.into();
            assert!(Request::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        }
    }
}

#[test]
fn every_original_member_pin_and_selection_are_independent_of_candidate_claims() {
    for name in ["manifest", "selection", "binary"] {
        let mut f = Fixture::new();
        f.value[name]["sha256"] = "b".repeat(64).into();
        f.save();
        assert!(f.invoke(None, Owner::fixture()).is_err());
        assert!(!f.final_path().exists());
        assert!(!f.host.root.join("run/simplestchat-bench").exists());
    }
    let mut f = Fixture::new();
    let path = f.host.root.join("input/ops-selection.json");
    let mut selection = contract::selection();
    selection["revision"] = "c".repeat(40).into();
    let bytes = serde_json::to_vec(&selection).unwrap();
    write(&path, &bytes, 0o600);
    f.value["selection"]["sha256"] = files::sha(&bytes).into();
    f.value["selection"]["bytes"] = bytes.len().into();
    f.save();
    assert!(f.invoke(None, Owner::fixture()).is_err());
    assert!(!f.final_path().exists());
}

#[test]
fn special_files_links_modes_and_unknown_pending_state_refuse_without_adoption() {
    for mode in [0o600, 0o777, 0o4755, 0o1755] {
        let f = Fixture::new();
        fs::set_permissions(
            f.host.root.join("input/simplestchat-ops"),
            fs::Permissions::from_mode(mode),
        )
        .unwrap();
        assert!(f.invoke(None, Owner::fixture()).is_err());
        assert!(!f.final_path().exists());
    }
    for kind in ["symlink", "hardlink", "fifo"] {
        let f = Fixture::new();
        let source = f.host.root.join("input/simplestchat-ops");
        match kind {
            "symlink" => {
                fs::rename(&source, source.with_extension("saved")).unwrap();
                symlink(source.with_extension("saved"), &source).unwrap();
            }
            "hardlink" => fs::hard_link(&source, source.with_extension("alias")).unwrap(),
            _ => {
                fs::remove_file(&source).unwrap();
                nix::unistd::mkfifo(&source, nix::sys::stat::Mode::S_IRUSR).unwrap();
            }
        }
        assert!(f.invoke(None, Owner::fixture()).is_err());
        assert!(!f.final_path().exists());
    }
    let f = Fixture::new();
    write(&f.temp_path(), b"FOREIGN_PENDING", 0o600);
    assert!(f.invoke(None, Owner::fixture()).is_err());
    assert_eq!(fs::read(f.temp_path()).unwrap(), b"FOREIGN_PENDING");
}

#[test]
fn existing_different_payload_or_mode_is_never_repaired_or_overwritten() {
    for mode in [0o600, 0o755] {
        let f = Fixture::new();
        write(&f.final_path(), b"FOREIGN_FINAL", mode);
        let before = snapshot(&f.host.root);
        assert!(f.invoke(None, Owner::fixture()).is_err());
        // Workload's first acquisition is permitted; the original leaf stays exact.
        assert_eq!(
            snapshot(&f.host.root)[&f.final_path()],
            before[&f.final_path()]
        );
        assert!(!f.temp_path().exists());
    }
    let f = Fixture::new();
    write(
        &f.final_path(),
        &fs::read(f.host.root.join("input/simplestchat-ops")).unwrap(),
        0o600,
    );
    let before = fs::metadata(f.final_path()).unwrap();
    assert!(f.invoke(None, Owner::fixture()).is_err());
    assert!(files::same(&before, &fs::metadata(f.final_path()).unwrap()));
}

#[test]
fn actual_same_workload_flock_and_journal_gates_cover_even_noop() {
    let f = Fixture::new();
    let held = Workload::install_fixture(&f.host.root, rustix::process::getuid().as_raw()).unwrap();
    assert_eq!(f.invoke(None, Owner::fixture()).unwrap_err(), Failure::Busy);
    assert!(!f.final_path().exists());
    held.finish().unwrap();
    f.install();
    for path in [
        "run/simplestchat-bench/current.json",
        "srv/simplestchat-public/release-state.json",
    ] {
        write(
            &f.host.root.join(path),
            b"{\"schemaVersion\":1,\"finalized\":false}",
            0o600,
        );
        assert!(f.invoke(None, Owner::fixture()).is_err());
        fs::remove_file(f.host.root.join(path)).unwrap();
    }
    assert!(!f.install().changed());
}

#[test]
fn failures_across_whole_copy_and_publication_attempt_all_peer_closes() {
    for event in [
        "created",
        "write",
        "fsync-file",
        "writers-closed",
        "after-chmod",
        "before-publish",
        "after-publish",
        "fsync-parent",
    ] {
        let f = Fixture::new();
        let fired = Rc::new(Cell::new(false));
        let trigger = fired.clone();
        let hook: Hook = Rc::new(move |actual, _| {
            if actual == event && !trigger.replace(true) {
                Err(Failure::Unproven)
            } else {
                Ok(())
            }
        });
        assert_eq!(
            f.invoke(Some(hook), Owner::fixture()).unwrap_err(),
            Failure::Unproven,
            "{event}"
        );
        assert!(fired.get(), "fixture reached {event}");
        if matches!(event, "after-publish" | "fsync-parent") {
            assert_eq!(
                files::sha(&fs::read(f.final_path()).unwrap()),
                f.value["binary"]["sha256"]
            );
        } else {
            assert!(!f.final_path().exists(), "{event}");
        }
        assert!(!f.temp_path().exists(), "{event}");
    }
}

#[test]
fn every_checked_close_failure_is_negative_and_remaining_files_still_close() {
    let baseline = Fixture::new();
    let (access, ledger) = baseline.access(None);
    run(
        Path::new("/input/install-request.json"),
        &baseline.selected,
        access,
        &baseline.host,
        Owner::fixture(),
    )
    .unwrap();
    let calls = ledger.borrow().closed;
    assert!(calls > 10);
    for failed in 1..=calls {
        let f = Fixture::new();
        let calls = Rc::new(Cell::new(0));
        let count = calls.clone();
        let hook: Hook = Rc::new(move |event, _| {
            if event == "close" {
                let next = count.get() + 1;
                count.set(next);
                if next == failed {
                    return Err(Failure::Unproven);
                }
            }
            Ok(())
        });
        assert!(
            f.invoke(Some(hook), Owner::fixture()).is_err(),
            "close{failed}"
        );
        assert!(calls.get() >= failed);
    }
}

#[test]
fn no_replace_and_after_effect_publication_keep_foreign_replacements_untouched() {
    for after in [false, true] {
        let f = Fixture::new();
        let path = f.final_path();
        let saved = path.with_extension("saved");
        let event = if after {
            "after-publish"
        } else {
            "before-publish"
        };
        let original = Rc::new(Cell::new(false));
        let fired = original.clone();
        let hook: Hook = Rc::new(move |actual, _| {
            if actual == event && !fired.replace(true) {
                if after {
                    fs::rename(&path, &saved).unwrap();
                }
                write(&path, b"FOREIGN_REPLACEMENT", 0o755);
                if after {
                    return Err(Failure::Unproven);
                }
            }
            Ok(())
        });
        assert!(f.invoke(Some(hook), Owner::fixture()).is_err());
        assert!(original.get());
        assert_eq!(fs::read(f.final_path()).unwrap(), b"FOREIGN_REPLACEMENT");
        assert!(!f.temp_path().exists());
        if after {
            assert_eq!(
                files::sha(&fs::read(f.final_path().with_extension("saved")).unwrap()),
                f.value["binary"]["sha256"]
            );
        }
    }
}

#[test]
fn cleanup_rechecks_original_name_and_rejects_unexpected_mode_transition() {
    let f = Fixture::new();
    let path = f.temp_path();
    let fired = Rc::new(Cell::new(false));
    let once = fired.clone();
    let hook: Hook = Rc::new(move |event, _| {
        if event == "write" {
            return Err(Failure::Deadline);
        }
        if event == "before-unlink" && !once.replace(true) {
            fs::rename(&path, path.with_extension("saved")).unwrap();
            write(&path, b"FOREIGN_CLEANUP_REPLACEMENT", 0o600);
        }
        Ok(())
    });
    assert_eq!(
        f.invoke(Some(hook), Owner::fixture()).unwrap_err(),
        Failure::Deadline
    );
    assert!(fired.get());
    assert_eq!(
        fs::read(f.temp_path()).unwrap(),
        b"FOREIGN_CLEANUP_REPLACEMENT"
    );
    let f = Fixture::new();
    let path = f.temp_path();
    let hook: Hook = Rc::new(move |event, _| {
        if event == "after-chmod" {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o4755)).unwrap();
        }
        Ok(())
    });
    assert!(f.invoke(Some(hook), Owner::fixture()).is_err());
    assert_eq!(fs::metadata(f.temp_path()).unwrap().mode() & 0o7777, 0o4755);
    assert!(!f.final_path().exists());
}

#[test]
fn same_inode_source_overwrite_after_validation_cannot_publish() {
    let f = Fixture::new();
    let path = f.host.root.join("input/simplestchat-ops");
    let hook: Hook = Rc::new(move |event, _| {
        if event == "pair-verified" {
            let mut bytes = fs::read(&path).unwrap();
            bytes[2048] ^= 1;
            write(&path, &bytes, 0o755);
        }
        Ok(())
    });
    assert!(f.invoke(Some(hook), Owner::fixture()).is_err());
    assert!(!f.final_path().exists());
    assert!(!f.temp_path().exists());
}

#[test]
fn original_parent_and_workload_inode_are_rechecked_after_copy_before_publication() {
    for replacement in ["parent", "lock"] {
        let f = Fixture::new();
        let path = if replacement == "parent" {
            f.host.root.join(request::DESTINATION)
        } else {
            f.host.root.join("run/simplestchat-bench/workload.lock")
        };
        let owned = path.clone();
        let fired = Rc::new(Cell::new(false));
        let seen = fired.clone();
        let hook: Hook = Rc::new(move |event, _| {
            if event == "before-publish" && !seen.replace(true) {
                fs::rename(&owned, owned.with_extension("saved")).unwrap();
                if replacement == "parent" {
                    fs::create_dir(&owned).unwrap();
                    fs::set_permissions(&owned, fs::Permissions::from_mode(0o755)).unwrap();
                    write(&owned.join("foreign"), b"FOREIGN_PARENT", 0o600);
                } else {
                    write(&owned, b"FOREIGN_LOCK", 0o600);
                }
            }
            Ok(())
        });
        assert!(f.invoke(Some(hook), Owner::fixture()).is_err());
        assert!(fired.get());
        assert!(!f.final_path().exists());
        if replacement == "parent" {
            assert_eq!(fs::read(path.join("foreign")).unwrap(), b"FOREIGN_PARENT");
        } else {
            assert_eq!(fs::read(path).unwrap(), b"FOREIGN_LOCK");
        }
    }
}

#[test]
fn deadline_and_cancellation_settle_owned_partial_files_before_returning_known_status() {
    for failure in [Failure::Deadline, Failure::Interrupted, Failure::Terminated] {
        let f = Fixture::new();
        let pending = Rc::new(Cell::new(false));
        let signal = pending.clone();
        let mut owner = Owner::fixture();
        let start = owner.start();
        let now = Rc::new(Cell::new(start));
        let time = now.clone();
        owner.now = Some(Rc::new(move || time.get()));
        let observed = pending.clone();
        owner.pending = Some(Rc::new(move || {
            if observed.get() && failure != Failure::Deadline {
                Err(failure)
            } else {
                Ok(())
            }
        }));
        let hook: Hook = Rc::new(move |event, _| {
            if event == "writers-closed" {
                signal.set(true);
                if failure == Failure::Deadline {
                    now.set(start + std::time::Duration::from_secs(105));
                }
            }
            Ok(())
        });
        assert_eq!(f.invoke(Some(hook), owner).unwrap_err(), failure);
        assert!(!f.final_path().exists());
        assert!(!f.temp_path().exists());
    }
    let f = Fixture::new();
    let mut owner = Owner::fixture();
    let start = owner.start();
    owner.now = Some(Rc::new(move || start + control::ENTRY));
    assert_eq!(f.invoke(None, owner).unwrap_err(), Failure::Deadline);
    assert!(!f.host.root.join("run/simplestchat-bench").exists());
}

#[test]
fn known_body_error_precedes_late_signal_or_final_clock_failure() {
    let f = Fixture::new();
    let pending = Rc::new(Cell::new(false));
    let signal = pending.clone();
    let mut owner = Owner::fixture();
    let observed = pending.clone();
    owner.pending = Some(Rc::new(move || {
        if observed.get() {
            Err(Failure::Terminated)
        } else {
            Ok(())
        }
    }));
    let hook: Hook = Rc::new(move |event, _| {
        if event == "write" {
            return Err(Failure::Unproven);
        }
        if event == "close" {
            signal.set(true);
        }
        Ok(())
    });
    // Ignore early transient closes; arm late cancellation only after body error.
    let armed = Rc::new(Cell::new(false));
    let arm = armed.clone();
    let original = hook;
    let hook: Hook = Rc::new(move |event, path| {
        if event == "write" {
            arm.set(true);
        }
        if event != "close" || arm.get() {
            original(event, path)
        } else {
            Ok(())
        }
    });
    assert_eq!(f.invoke(Some(hook), owner).unwrap_err(), Failure::Unproven);
}

#[cfg(target_os = "linux")]
#[test]
fn actual_thread_directed_signals_restore_mask_after_every_owned_close() {
    use nix::sys::signal::{SigSet, Signal, raise};
    let before = SigSet::thread_get_mask().unwrap();
    let f = Fixture::new();
    let active = Rc::new(Cell::new(false));
    let flag = active.clone();
    let count = Rc::new(Cell::new(0));
    let seen = count.clone();
    let hook: Hook = Rc::new(move |event, _| {
        if event == "writers-closed" {
            flag.set(true);
            raise(Signal::SIGINT).unwrap();
        }
        if event == "close" && flag.get() && seen.get() < 2 {
            seen.set(seen.get() + 1);
            raise(Signal::SIGTERM).unwrap();
        }
        Ok(())
    });
    assert_eq!(
        f.invoke(Some(hook), Owner::native().unwrap()).unwrap_err(),
        Failure::Interrupted
    );
    assert_eq!(count.get(), 2);
    let after = SigSet::thread_get_mask().unwrap();
    assert_eq!(
        before.contains(Signal::SIGINT),
        after.contains(Signal::SIGINT)
    );
    assert_eq!(
        before.contains(Signal::SIGTERM),
        after.contains(Signal::SIGTERM)
    );
    assert!(!f.final_path().exists());
    assert!(!f.temp_path().exists());
}

#[test]
fn receipts_and_failures_do_not_disclose_input_paths_or_private_claims() {
    let f = Fixture::new();
    let value = serde_json::to_string(&f.install()).unwrap();
    assert!(!value.contains("input"));
    assert!(!value.contains("PRIVATE"));
    assert!(!value.contains("authenticated"));
    assert!(!value.contains("path"));
    for failure in [
        Failure::Busy,
        Failure::Unproven,
        Failure::Deadline,
        Failure::Interrupted,
        Failure::Terminated,
    ] {
        assert!(!format!("{failure:?} {failure}").contains("PRIVATE"));
    }
}

#[test]
fn explicit_modes_are_independent_of_umask_in_one_owned_test_child() {
    const CHILD: &str = "SIMPLESTCHAT_INSTALL_UMASK_TEST_CHILD";
    if std::env::var(CHILD).as_deref() == Ok("1") {
        let previous = nix::sys::stat::umask(nix::sys::stat::Mode::empty());
        let f = Fixture::new();
        f.install();
        assert_eq!(fs::metadata(f.final_path()).unwrap().mode() & 0o7777, 0o755);
        assert_eq!(
            fs::metadata(f.host.root.join("run/simplestchat-bench/workload.lock"))
                .unwrap()
                .mode()
                & 0o7777,
            0o600,
        );
        let _ = nix::sys::stat::umask(previous);
        return;
    }
    // The only process launched is this exact test executable with one named
    // private fixture test. No installer CLI, candidate or managed host runs.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            concat!(
                module_path!(),
                "::explicit_modes_are_independent_of_umask_in_one_owned_test_child"
            ),
        ])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
