//! Actual owned files with synthetic root identity/ownership only. No production
//! paths are substituted through a public API and no installed helper executes.

use super::{files::Access, *};
use crate::test_support::Fixture;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
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
            ("usr/local", 0o755),
            ("usr/local/libexec", 0o755),
            ("usr/local/libexec/simplestchat-public", 0o755),
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
        for name in request::BASE
            .into_iter()
            .chain(request::FETCH)
            .chain(["force_release.py"])
        {
            write(
                &root.join(format!("usr/local/libexec/simplestchat-public/{name}")),
                b"PRIVATE_INSTALLED_HELPER_CONTENT\n",
                0o644,
            );
        }
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
    pub fn request(&self, extra: &[&str]) -> String {
        let digest = Sha256::digest(b"PRIVATE_INSTALLED_HELPER_CONTENT\n")
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let values: BTreeMap<_, _> = request::BASE
            .into_iter()
            .chain(extra.iter().copied())
            .map(|name| (name, &digest))
            .collect();
        serde_json::to_string(&values).unwrap()
    }
    pub fn check(&mut self, raw: &str) -> Result<Receipt> {
        let expected = ExpectedHelpers::decode(Some(raw))?;
        check_with(self, &expected)
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
fn complete_sets_return_only_fresh_public_receipts_and_never_read_configuration() {
    let mut host = FixtureHost::new();
    for extra in [&[][..], &request::FETCH[..], &["force_release.py"][..]] {
        let raw = host.request(extra);
        let result = host.check(&raw).unwrap();
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 3);
        assert_eq!(value["schemaVersion"], 1);
        assert_eq!(value["prepared"], true);
        assert_eq!(result.run_id.len(), 32);
        assert!(
            result
                .run_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        );
        host.settled();
    }
    assert!(
        host.access
            .0
            .borrow()
            .reads
            .iter()
            .all(|path| !path.ends_with("images.json"))
    );
    let first = host.check("{}").unwrap();
    let second = host.check("{}").unwrap();
    assert_ne!(first.run_id, second.run_id);
    assert!(!first.prepared);
    assert!(!format!("{first:?}").contains("PRIVATE"));
    host.settled();
}

#[test]
fn ordinary_reconciliation_does_not_require_helpers_or_releases() {
    let mut host = FixtureHost::new();
    fs::remove_dir_all(host.root.join("usr/local")).unwrap();
    fs::remove_dir(host.root.join("srv/simplestchat-public/releases")).unwrap();
    assert!(!host.check("{}").unwrap().prepared);
    assert_eq!(host.access.0.borrow().reads, ["etc/os-release"]);
    host.settled();
}

#[test]
fn request_admission_and_identity_precede_filesystem_access() {
    let mut host = FixtureHost::new();
    host.euid = 42;
    assert_eq!(code(host.check("[]")), Some("invalid_helper_set"));
    assert_eq!(code(host.check("{}")), Some("root_required"));
    host.euid = 0;
    host.system = b"Darwin".to_vec();
    assert_eq!(code(host.check("{}")), Some("unsupported_platform"));
    host.system = b"Linux".to_vec();
    host.machine = b"aarch64".to_vec();
    assert_eq!(code(host.check("{}")), Some("unsupported_platform"));
    assert_eq!(host.opened, 0);
    assert!(ExpectedHelpers::decode(Some(&format!("{{}}{}", " ".repeat(4094)))).is_ok());
    assert_eq!(
        ExpectedHelpers::decode(Some(&" ".repeat(4097))).unwrap_err(),
        Failure::Request
    );
    for raw in [
        "{\"x\":1,\"x\":2}",
        "NaN",
        "\u{feff}{}",
        "{\"x\":\"\\ud800\"}",
    ] {
        assert_eq!(
            ExpectedHelpers::decode(Some(raw)).unwrap_err(),
            Failure::Unavailable
        );
    }
    let nested = format!("{}0{}", "[".repeat(129), "]".repeat(129));
    assert_eq!(
        ExpectedHelpers::decode(Some(&nested)).unwrap_err(),
        Failure::Unavailable
    );
}

#[test]
fn every_permission_owner_size_type_and_original_byte_digest_is_checked() {
    for (path, expected_mode, failure) in [
        ("etc/simplestchat-public", 0o700, Failure::Path),
        ("srv/simplestchat-public", 0o700, Failure::Path),
        ("srv/simplestchat-public/releases", 0o700, Failure::Path),
        ("usr/local", 0o755, Failure::Path),
        (
            "usr/local/libexec/simplestchat-public/release-public.py",
            0o644,
            Failure::Path,
        ),
        ("etc/simplestchat-public/images.json", 0o600, Failure::Path),
    ] {
        for extra in [0o002, 0o1000, 0o4000] {
            let mut host = FixtureHost::new();
            let raw = host.request(&[]);
            fs::set_permissions(
                host.root.join(path),
                fs::Permissions::from_mode(expected_mode | extra),
            )
            .unwrap();
            assert_eq!(host.check(&raw).unwrap_err(), failure);
            host.settled();
        }
        for owner in [(1, 0), (0, 1)] {
            let mut host = FixtureHost::new();
            let raw = host.request(&[]);
            host.access.0.borrow_mut().owners.insert(path.into(), owner);
            assert_eq!(host.check(&raw).unwrap_err(), failure);
            host.settled();
        }
    }
    for (path, limit) in [
        ("etc/simplestchat-public/images.json", 65536),
        (
            "usr/local/libexec/simplestchat-public/release-public.py",
            1024 * 1024,
        ),
    ] {
        for size in [0, limit + 1] {
            let mut host = FixtureHost::new();
            let raw = host.request(&[]);
            fs::write(host.root.join(path), vec![b'x'; size]).unwrap();
            assert_eq!(host.check(&raw).unwrap_err(), Failure::Size);
            host.settled();
        }
    }
    let mut host = FixtureHost::new();
    let raw = host.request(&[]);
    let source = host
        .root
        .join("usr/local/libexec/simplestchat-public/release-public.py");
    write(&source, b"CHANGED_INSTALLED_HELPER_CONTENT\n", 0o644);
    assert_eq!(host.check(&raw).unwrap_err(), Failure::Digest);
    host.settled();
}

#[test]
fn links_fifos_and_namespace_replacement_refuse_without_following_or_hanging() {
    for kind in ["link", "fifo", "directory", "ancestor"] {
        let mut host = FixtureHost::new();
        let raw = host.request(&[]);
        let path = host.root.join(if kind == "ancestor" {
            "usr/local"
        } else {
            "usr/local/libexec/simplestchat-public/release-public.py"
        });
        let retained = path.with_extension("retained");
        fs::rename(&path, &retained).unwrap();
        match kind {
            "link" | "ancestor" => symlink(&retained, &path).unwrap(),
            "fifo" => {
                nix::unistd::mkfifo(&path, nix::sys::stat::Mode::from_bits_truncate(0o644)).unwrap()
            }
            _ => fs::create_dir(&path).unwrap(),
        }
        assert_eq!(host.check(&raw).unwrap_err(), Failure::Path, "{kind}");
        host.settled();
    }
    for kind in ["replace", "write", "mode", "ancestor"] {
        let mut host = FixtureHost::new();
        let raw = host.request(&[]);
        let root = host.root.clone();
        let mut applied = false;
        host.access.0.borrow_mut().hook = Some(Box::new(move |event, _| {
            if event == "verify" && !applied {
                applied = true;
                let path = root.join("usr/local/libexec/simplestchat-public/release-public.py");
                match kind {
                    "replace" => {
                        fs::rename(&path, path.with_extension("retained")).unwrap();
                        write(&path, b"PRIVATE_INSTALLED_HELPER_CONTENT\n", 0o644);
                    }
                    "write" => write(&path, b"CHANGED_INSTALLED_HELPER_CONTENT\n", 0o644),
                    "mode" => fs::set_permissions(path, fs::Permissions::from_mode(0o666)).unwrap(),
                    _ => {
                        let parent = root.join("usr/local/libexec");
                        fs::rename(&parent, parent.with_extension("retained")).unwrap();
                        fs::create_dir(&parent).unwrap();
                    }
                }
            }
        }));
        assert_eq!(
            host.check(&raw).unwrap_err(),
            Failure::Unavailable,
            "{kind}"
        );
        host.settled();
    }
}

#[test]
fn metadata_read_and_close_failures_attempt_every_acquired_close() {
    let mut control = FixtureHost::new();
    let raw = control.request(&[]);
    control.check(&raw).unwrap();
    let total_metadata = control.access.0.borrow().metadata;
    let total_close = control.access.0.borrow().close_calls;
    for failed in 1..=total_metadata {
        let mut host = FixtureHost::new();
        let raw = host.request(&[]);
        host.access.0.borrow_mut().fail_metadata = Some(failed);
        assert_eq!(host.check(&raw).unwrap_err(), Failure::Unavailable);
        host.settled();
    }
    for failed in 1..=total_close {
        let mut host = FixtureHost::new();
        let raw = host.request(&[]);
        host.access.0.borrow_mut().fail_close = Some(failed);
        assert_eq!(host.check(&raw).unwrap_err(), Failure::Unavailable);
        host.settled();
    }
    let mut host = FixtureHost::new();
    host.access.0.borrow_mut().fail_read = true;
    assert_eq!(host.check("{}").unwrap_err(), Failure::Unavailable);
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
        assert!(!host.check("{}").unwrap().prepared);
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
        b"ID=debian \nVERSION_ID=13\n",
        b"ID=debian\nVERSION_ID=013\n",
        b"\xff",
    ] {
        assert!(platform::distribution(bytes).is_err());
    }
    let mut host = FixtureHost::new();
    fs::remove_file(host.root.join("etc/os-release")).unwrap();
    symlink("../PRIVATE", host.root.join("etc/os-release")).unwrap();
    assert_eq!(host.check("{}").unwrap_err(), Failure::Path);
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
        assert_eq!(host.check("{}").unwrap_err(), Failure::Unavailable);
        host.settled();
    }
    assert_eq!(platform::distribution(b""), Err(Failure::Distribution));
}

#[test]
fn helper_order_hardlinks_and_strict_type_admission_match_the_named_domain() {
    let mut host = FixtureHost::new();
    let raw = host.request(&[]);
    let mut value: Value = serde_json::from_str(&raw).unwrap();
    for bad in [
        Value::Null,
        json!(true),
        json!(1),
        json!("A".repeat(64)),
        json!("g".repeat(64)),
    ] {
        value["release-public.py"] = bad;
        assert_eq!(
            ExpectedHelpers::decode(Some(&value.to_string())).unwrap_err(),
            Failure::HelperDigest
        );
    }
    let file = host
        .root
        .join("usr/local/libexec/simplestchat-public/release-public.py");
    fs::hard_link(&file, host.fixture.0.join("owned-hardlink")).unwrap();
    assert!(host.check(&raw).is_ok());
    host.settled();
    let mut entries = serde_json::from_str::<BTreeMap<String, Value>>(&raw).unwrap();
    entries.insert("force_release.py".into(), json!("a".repeat(64)));
    for name in request::FETCH {
        entries.insert(name.into(), json!("a".repeat(64)));
    }
    assert_eq!(
        ExpectedHelpers::decode(Some(&serde_json::to_string(&entries).unwrap())).unwrap_err(),
        Failure::HelperSet
    );

    let mut values: BTreeMap<String, Value> = serde_json::from_str(&raw).unwrap();
    values.insert("runtime_profile.py".into(), json!("a".repeat(64)));
    let first = values.remove("runtime_profile.py").unwrap();
    let tail = serde_json::to_string(&values).unwrap();
    let ordered = format!("{{\"runtime_profile.py\":{first},{}", &tail[1..]);
    fs::set_permissions(
        host.root
            .join("usr/local/libexec/simplestchat-public/bounded_process.py"),
        fs::Permissions::from_mode(0o666),
    )
    .unwrap();
    assert_eq!(host.check(&ordered).unwrap_err(), Failure::Digest);
    host.settled();
}

#[test]
fn exact_file_limits_and_os_release_refusals_do_not_expand_read_domains() {
    let mut host = FixtureHost::new();
    write(
        &host.root.join("etc/simplestchat-public/images.json"),
        &vec![0xff; 65536],
        0o600,
    );
    let source = host
        .root
        .join("usr/local/libexec/simplestchat-public/release-public.py");
    let bytes = vec![b'x'; 1024 * 1024];
    write(&source, &bytes, 0o644);
    let mut request: Value = serde_json::from_str(&host.request(&[])).unwrap();
    request["release-public.py"] = json!(
        Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    assert!(host.check(&request.to_string()).is_ok());
    host.settled();
    for (bytes, expected) in [
        (Vec::new(), Failure::Distribution),
        (vec![0xff], Failure::Unavailable),
        (vec![b'x'; 65537], Failure::Size),
    ] {
        let mut host = FixtureHost::new();
        write(&host.root.join("etc/os-release"), &bytes, 0o644);
        assert_eq!(host.check("{}").unwrap_err(), expected);
        host.settled();
    }
}
