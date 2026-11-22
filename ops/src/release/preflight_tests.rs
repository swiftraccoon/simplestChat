//! Actual owned files with synthetic root identity/ownership only. No production
//! paths are substituted through a public API.

use super::{files::Access, *};
use crate::test_support::Fixture;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    os::{
        fd::{AsRawFd, RawFd},
        unix::fs::{PermissionsExt, symlink},
    },
    path::{Path, PathBuf},
    rc::Rc,
};

type Hook = Box<dyn FnMut(&str, &str)>;
#[derive(Default)]
pub(super) struct State {
    pub reads: Vec<String>,
    pub metadata: usize,
    pub close_calls: usize,
    pub live: BTreeSet<RawFd>,
    pub maximum: usize,
    pub fail_metadata: Option<usize>,
    pub fail_close: Option<usize>,
    pub fail_read: bool,
    pub owners: BTreeMap<String, (u32, u32)>,
    pub hook: Option<Hook>,
}
#[derive(Clone, Default)]
pub(super) struct FixtureAccess(pub Rc<RefCell<State>>);
impl Access for FixtureAccess {
    fn ownership(&self, path: &str, _uid: u32, _gid: u32) -> (u32, u32) {
        self.0.borrow().owners.get(path).copied().unwrap_or((0, 0))
    }
    fn event(&self, event: &'static str, path: &str) -> Result<()> {
        let mut state = self.0.borrow_mut();
        if let Some(hook) = &mut state.hook {
            hook(event, path);
        }
        if event == "read" {
            state.reads.push(path.into());
            if state.fail_read {
                return Err(Failure::Unavailable);
            }
        }
        Ok(())
    }
    fn stamp(&self, file: &File, path: &str) -> Result<files::Stamp> {
        {
            let mut state = self.0.borrow_mut();
            state.live.insert(file.as_raw_fd());
            state.maximum = state.maximum.max(state.live.len());
            state.metadata += 1;
            if state.fail_metadata == Some(state.metadata) {
                return Err(Failure::Unavailable);
            }
        }
        self.event("metadata", path)?;
        let mut stamp = files::NativeAccess.stamp(file, path)?;
        (stamp.uid, stamp.gid) = self.ownership(path, stamp.uid, stamp.gid);
        Ok(stamp)
    }
    fn close(&self, file: File) -> Result<()> {
        let failed = {
            let mut state = self.0.borrow_mut();
            assert!(state.live.remove(&file.as_raw_fd()));
            state.close_calls += 1;
            state.fail_close == Some(state.close_calls)
        };
        nix::unistd::close(file).unwrap();
        if failed {
            Err(Failure::Unavailable)
        } else {
            Ok(())
        }
    }
}

pub(super) struct FixtureHost {
    pub fixture: Fixture,
    pub root: PathBuf,
    pub access: FixtureAccess,
    pub euid: u32,
    pub system: Vec<u8>,
    pub machine: Vec<u8>,
    pub opened: usize,
    pub random_calls: u8,
}
impl FixtureHost {
    pub fn new() -> Self {
        let fixture = Fixture::new();
        let root = fixture.0.join("host");
        for (name, mode) in [
            ("", 0o755),
            ("etc", 0o755),
            ("srv", 0o755),
            ("usr", 0o755),
            ("usr/lib", 0o755),
            ("etc/simplestchat-public", 0o700),
            ("srv/simplestchat-public", 0o700),
            ("srv/simplestchat-public/releases", 0o700),
        ] {
            let path = root.join(name);
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        }
        write(
            &root.join("etc/os-release"),
            b"ID=debian\nVERSION_ID=13\n",
            0o644,
        );
        write(
            &root.join("etc/simplestchat-public/images.json"),
            b"PRIVATE_NOT_JSON\xff\n",
            0o600,
        );
        Self {
            fixture,
            root,
            access: FixtureAccess::default(),
            euid: 0,
            system: b"Linux".to_vec(),
            machine: b"x86_64".to_vec(),
            opened: 0,
            random_calls: 0,
        }
    }
    pub fn check(&mut self) -> Result<Receipt> {
        check_with(self)
    }
    pub fn settled(&self) {
        let state = self.access.0.borrow();
        assert!(
            state.live.is_empty(),
            "a retained/probe File was not explicitly closed"
        );
        assert!(state.maximum <= 48);
    }
}
impl Host for FixtureHost {
    type Access = FixtureAccess;
    fn identity(&mut self) -> Result<platform::Identity> {
        Ok(platform::Identity {
            euid: self.euid,
            system: self.system.clone(),
            machine: self.machine.clone(),
        })
    }
    fn filesystem(&mut self) -> Result<files::Tree<Self::Access>> {
        self.opened += 1;
        files::Tree::open(File::open(&self.root).unwrap(), self.access.clone())
    }
    fn random(&mut self) -> Result<[u8; 16]> {
        self.random_calls += 1;
        Ok([self.random_calls; 16])
    }
}
pub(super) fn write(path: &Path, bytes: &[u8], mode: u32) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}
fn code(result: Result<Receipt>) -> Option<&'static str> {
    result.err().map(Failure::code)
}

#[test]
fn receipts_are_fresh_public_and_never_read_configuration() {
    let mut host = FixtureHost::new();
    let first = host.check().unwrap();
    let second = host.check().unwrap();
    for result in [&first, &second] {
        let value = serde_json::to_value(result).unwrap();
        assert_eq!(
            value.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["runId", "schemaVersion"]
        );
        assert_eq!(value["schemaVersion"], 1);
        assert_eq!(result.run_id.len(), 32);
        assert!(
            result
                .run_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        );
    }
    assert_ne!(first.run_id, second.run_id);
    assert!(
        host.access
            .0
            .borrow()
            .reads
            .iter()
            .all(|path| !path.ends_with("images.json"))
    );
    assert!(!format!("{first:?}").contains("PRIVATE"));
    host.settled();
}

#[test]
fn reconciliation_requires_no_helper_directory_or_release_store() {
    let mut host = FixtureHost::new();
    fs::remove_dir(host.root.join("srv/simplestchat-public/releases")).unwrap();
    host.check().unwrap();
    assert_eq!(host.access.0.borrow().reads, ["etc/os-release"]);
    host.settled();
}

#[test]
fn identity_precedes_filesystem_access() {
    let mut host = FixtureHost::new();
    host.euid = 42;
    assert_eq!(code(host.check()), Some("root_required"));
    host.euid = 0;
    host.system = b"Darwin".to_vec();
    assert_eq!(code(host.check()), Some("unsupported_platform"));
    host.system = b"Linux".to_vec();
    host.machine = b"aarch64".to_vec();
    assert_eq!(code(host.check()), Some("unsupported_platform"));
    assert_eq!(host.opened, 0);
}

#[test]
fn every_permission_owner_size_and_type_is_checked() {
    for (path, expected_mode) in [
        ("etc/simplestchat-public", 0o700),
        ("srv/simplestchat-public", 0o700),
        ("etc/simplestchat-public/images.json", 0o600),
    ] {
        for extra in [0o002, 0o1000, 0o4000] {
            let mut host = FixtureHost::new();
            fs::set_permissions(
                host.root.join(path),
                fs::Permissions::from_mode(expected_mode | extra),
            )
            .unwrap();
            assert_eq!(host.check().unwrap_err(), Failure::Path, "{path}");
            host.settled();
        }
        for owner in [(1, 0), (0, 1)] {
            let mut host = FixtureHost::new();
            host.access.0.borrow_mut().owners.insert(path.into(), owner);
            assert_eq!(host.check().unwrap_err(), Failure::Path, "{path}");
            host.settled();
        }
    }
    for size in [0, 65537] {
        let mut host = FixtureHost::new();
        fs::write(
            host.root.join("etc/simplestchat-public/images.json"),
            vec![b'x'; size],
        )
        .unwrap();
        assert_eq!(host.check().unwrap_err(), Failure::Size);
        host.settled();
    }
    // A second name for the configuration file is not a refusal.
    let mut host = FixtureHost::new();
    fs::hard_link(
        host.root.join("etc/simplestchat-public/images.json"),
        host.fixture.0.join("owned-hardlink"),
    )
    .unwrap();
    host.check().unwrap();
    host.settled();
}

#[test]
fn links_fifos_and_namespace_replacement_refuse_without_following_or_hanging() {
    for kind in ["link", "fifo", "directory", "ancestor"] {
        let mut host = FixtureHost::new();
        let path = host.root.join(if kind == "ancestor" {
            "etc/simplestchat-public"
        } else {
            "etc/simplestchat-public/images.json"
        });
        let retained = path.with_extension("retained");
        fs::rename(&path, &retained).unwrap();
        match kind {
            "link" | "ancestor" => symlink(&retained, &path).unwrap(),
            "fifo" => {
                nix::unistd::mkfifo(&path, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap()
            }
            _ => fs::create_dir(&path).unwrap(),
        }
        assert_eq!(host.check().unwrap_err(), Failure::Path, "{kind}");
        host.settled();
    }
    for kind in ["replace", "write", "mode", "ancestor"] {
        let mut host = FixtureHost::new();
        let root = host.root.clone();
        let mut applied = false;
        host.access.0.borrow_mut().hook = Some(Box::new(move |event, _| {
            if event == "verify" && !applied {
                applied = true;
                let path = root.join("etc/simplestchat-public/images.json");
                match kind {
                    "replace" => {
                        fs::rename(&path, path.with_extension("retained")).unwrap();
                        write(&path, b"PRIVATE_NOT_JSON\xff\n", 0o600);
                    }
                    "write" => write(&path, b"CHANGED_NOT_JSON\xff\n", 0o600),
                    "mode" => fs::set_permissions(path, fs::Permissions::from_mode(0o666)).unwrap(),
                    _ => {
                        let parent = root.join("etc/simplestchat-public");
                        fs::rename(&parent, parent.with_extension("retained")).unwrap();
                        fs::create_dir(&parent).unwrap();
                    }
                }
            }
        }));
        assert_eq!(host.check().unwrap_err(), Failure::Unavailable, "{kind}");
        host.settled();
    }
}

#[test]
fn metadata_read_and_close_failures_attempt_every_acquired_close() {
    let mut control = FixtureHost::new();
    control.check().unwrap();
    let total_metadata = control.access.0.borrow().metadata;
    let total_close = control.access.0.borrow().close_calls;
    for failed in 1..=total_metadata {
        let mut host = FixtureHost::new();
        host.access.0.borrow_mut().fail_metadata = Some(failed);
        assert_eq!(host.check().unwrap_err(), Failure::Unavailable);
        host.settled();
    }
    for failed in 1..=total_close {
        let mut host = FixtureHost::new();
        host.access.0.borrow_mut().fail_close = Some(failed);
        assert_eq!(host.check().unwrap_err(), Failure::Unavailable);
        host.settled();
    }
    let mut host = FixtureHost::new();
    host.access.0.borrow_mut().fail_read = true;
    assert_eq!(host.check().unwrap_err(), Failure::Unavailable);
    host.settled();
}

#[test]
fn standard_os_release_link_fallback_parser_and_mutation_are_bounded() {
    for target in [
        None,
        Some("../usr/lib/os-release"),
        Some("/usr/lib/os-release"),
    ] {
        let mut host = FixtureHost::new();
        fs::remove_file(host.root.join("etc/os-release")).unwrap();
        write(
            &host.root.join("usr/lib/os-release"),
            b"ID='debian'\rVERSION_ID=\"13.1\"\r\n",
            0o644,
        );
        if let Some(target) = target {
            symlink(target, host.root.join("etc/os-release")).unwrap();
        }
        host.check().unwrap();
        host.settled();
    }
    for bytes in [
        b"ID=debian\nVERSION_ID=13.\n".as_slice(),
        b"ID=ubuntu\nID=debian\nVERSION_ID=13..suffix\n",
        b"ID=debian\nVERSION_ID=13.99\n",
    ] {
        assert!(platform::distribution(bytes).is_ok());
    }
    for bytes in [
        b"ID=ubuntu\nVERSION_ID=13\n".as_slice(),
        b"ID=debian\nVERSION_ID=12\n",
        b"ID=debian\n",
        b"ID='debian\"\nVERSION_ID=13\n",
        b"ID=debian \nVERSION_ID=13\n",
        b"ID=debian\nVERSION_ID=013\n",
        b"\xff",
    ] {
        assert!(platform::distribution(bytes).is_err());
    }
    let mut host = FixtureHost::new();
    fs::remove_file(host.root.join("etc/os-release")).unwrap();
    symlink("../PRIVATE", host.root.join("etc/os-release")).unwrap();
    assert_eq!(host.check().unwrap_err(), Failure::Path);
    host.settled();
}

#[test]
fn os_release_selected_namespace_includes_preferred_absence_and_link_binding() {
    for change in ["preferred", "link"] {
        let mut host = FixtureHost::new();
        fs::rename(
            host.root.join("etc/os-release"),
            host.root.join("usr/lib/os-release"),
        )
        .unwrap();
        if change == "link" {
            symlink("../usr/lib/os-release", host.root.join("etc/os-release")).unwrap();
        }
        let root = host.root.clone();
        let mut applied = false;
        host.access.0.borrow_mut().hook = Some(Box::new(move |event, _| {
            if event == "verify" && !applied {
                applied = true;
                let path = root.join("etc/os-release");
                if change == "link" {
                    fs::remove_file(&path).unwrap();
                }
                write(&path, b"ID=debian\nVERSION_ID=13\n", 0o644);
            }
        }));
        assert_eq!(host.check().unwrap_err(), Failure::Unavailable);
        host.settled();
    }
    assert_eq!(platform::distribution(b""), Err(Failure::Distribution));
}

#[test]
fn exact_file_limits_and_os_release_refusals_do_not_expand_read_domains() {
    let mut host = FixtureHost::new();
    write(
        &host.root.join("etc/simplestchat-public/images.json"),
        &vec![0xff; 65536],
        0o600,
    );
    host.check().unwrap();
    host.settled();
    for (bytes, expected) in [
        (Vec::new(), Failure::Distribution),
        (vec![0xff], Failure::Unavailable),
        (vec![b'x'; 65537], Failure::Size),
    ] {
        let mut host = FixtureHost::new();
        write(&host.root.join("etc/os-release"), &bytes, 0o644);
        assert_eq!(host.check().unwrap_err(), expected);
        host.settled();
    }
}
