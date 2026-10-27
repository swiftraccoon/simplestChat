//! Protocol and real filesystem fault neighbors; these are not VM success evidence.

use super::*;
use serde_json::json;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::atomic::{AtomicU64, Ordering},
};

fn request_value() -> Value {
    let sources = sources::guest_names()
        .into_iter()
        .map(|name| (name, json!("a".repeat(64))))
        .collect::<serde_json::Map<_, _>>();
    let helpers = sources::helper_names()
        .into_iter()
        .map(|name| (name, json!("b".repeat(64))))
        .collect::<serde_json::Map<_, _>>();
    let pin = json!({"sha256":"c".repeat(64),"bytes":123});
    json!({"schemaVersion":1,"scope":"MaintainLauncherPrerequisiteRequest","runId":"d".repeat(32),"revision":"e".repeat(40),"scenario":"positive","setupSeconds":510,"bodySeconds":1680,"endSeconds":1800,"binary":pin,"manifest":pin,"archive":pin,"sourceBundle":pin,"sourceTree":"f".repeat(40),"sourcePins":sources,"helperPins":helpers,"postgresImage":format!("postgres@sha256:{}","a".repeat(64)),"caddyImage":protocol::CADDY_IMAGE,"engineVersion":"29.0.0"})
}
#[test]
fn exact_request_keeps_fifty_two_sources_fifteen_helpers_and_original_caps() {
    let value = request_value();
    let request = Request::decode(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(value.as_object().unwrap().len(), 18);
    assert_eq!(request.source_pins.len(), 52);
    assert_eq!(request.helper_pins.len(), 15);
    for (pointer, bad) in [
        ("/schemaVersion", json!(true)),
        ("/setupSeconds", json!(511)),
        ("/bodySeconds", json!(1169)),
        ("/endSeconds", json!(1801)),
        ("/endSeconds", json!(1680)),
        ("/scenario", json!("SECRET")),
        ("/binary/bytes", json!(true)),
        ("/archive/bytes", json!(2147483649u64)),
        ("/caddyImage", json!("other@sha256:bad")),
        ("/engineVersion", json!("1.2")),
    ] {
        let mut neighbor = value.clone();
        *neighbor.pointer_mut(pointer).unwrap() = bad;
        assert!(Request::decode(&serde_json::to_vec(&neighbor).unwrap()).is_err());
    }
    let mut extra = value.clone();
    extra["sourcePins"]["SECRET"] = "a".repeat(64).into();
    assert!(Request::decode(&serde_json::to_vec(&extra).unwrap()).is_err());
    let mut missing = value;
    missing["helperPins"]
        .as_object_mut()
        .unwrap()
        .remove("bounded_process.py");
    assert!(Request::decode(&serde_json::to_vec(&missing).unwrap()).is_err());
}
#[test]
fn strict_wire_rejects_duplicates_nonfinite_trailing_and_deep_records() {
    for bytes in [
        b"{\"runId\":1,\"runId\":2}".as_slice(),
        b"{}\n{}",
        b"{\"x\":NaN}",
        b"{\"x\":1e400}",
        b"\xef\xbb\xbf{}",
    ] {
        assert!(protocol::decode(bytes).is_err());
    }
    assert!(
        protocol::decode(format!("{}0{}", "[".repeat(130), "]".repeat(130)).as_bytes()).is_err()
    );
}
#[test]
fn asset_initialization_requires_exact_completed_receipt_even_after_zero_status() {
    for bytes in [
        b"{\"passed\":true,\"changed\":true}\n".as_slice(),
        b"{\"passed\":true,\"changed\":false}\n",
    ] {
        assets_initialized(bytes).unwrap();
    }
    for bytes in [
        b"{\"passed\":true,\"busy\":true,\"changed\":false}".as_slice(),
        b"{\"passed\":false,\"changed\":false}",
        b"{\"passed\":true,\"changed\":0}",
        b"{\"passed\":true}",
        b"{\"passed\":true,\"changed\":true,\"changed\":false}",
        b"{\"passed\":true,\"changed\":true}\n{}",
    ] {
        assert!(assets_initialized(bytes).is_err());
    }
}

#[test]
fn admission_failure_keeps_actual_guest_signal_or_deadline_status_after_finalizers() {
    assert_eq!(failed_status(0), 1);
    assert_eq!(failed_status(124), 124);
    assert_eq!(failed_status(130), 130);
    assert_eq!(failed_status(143), 143);
    assert_eq!(failed_status(7), 1);
}
fn native_value() -> Value {
    let mut facts = serde_json::Map::new();
    for name in observation::BOOLEANS {
        facts.insert(name.into(), json!(false));
    }
    for name in observation::HASHES {
        facts.insert(name.into(), Value::Null);
    }
    facts.insert("adoptedReaped".into(), json!(1000));
    facts.insert("adoptedRawStatuses".into(), json!(vec![-9; 256]));
    facts.insert("wrapperRawStatus".into(), Value::Null);
    facts.insert("launcherRawStatus".into(), Value::Null);
    facts.insert("stdoutBytes".into(), json!(u64::MAX));
    facts.insert("stderrBytes".into(), json!(u64::MAX));
    facts.insert("launcherWorkElapsedNs".into(), Value::Null);
    json!({"schemaVersion":1,"experiment":"maintain-launcher-prerequisite","trial":"normal","passed":false,"phase":"result","failure":"bounds","facts":facts})
}
#[test]
fn truthful_native_refusal_retains_nulls_large_counts_and_actual_signed_statuses() {
    let value = native_value();
    assert_eq!(value["facts"].as_object().unwrap().len(), 40);
    assert_eq!(
        observation::native_frame(&serde_json::to_vec(&value).unwrap()).unwrap(),
        value
    );
    for (pointer, bad) in [
        ("/facts/adoptedReaped", json!(u64::from(u32::MAX) + 1)),
        ("/facts/stdoutBytes", json!(-1)),
        ("/facts/wrapperRawStatus", json!(256)),
        ("/facts/launcherWorkElapsedNs", json!(1050000000001u64)),
        ("/facts/opsSha256", json!("SECRET")),
    ] {
        let mut neighbor = value.clone();
        *neighbor.pointer_mut(pointer).unwrap() = bad;
        assert!(observation::native_frame(&serde_json::to_vec(&neighbor).unwrap()).is_err());
    }
    let mut short = value;
    short["facts"]["adoptedRawStatuses"]
        .as_array_mut()
        .unwrap()
        .pop();
    assert!(observation::native_frame(&serde_json::to_vec(&short).unwrap()).is_err());
}

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "maintain-guest-files-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn exact_inert_handoff_retires_only_its_original_and_refuses_changed_bytes() {
    let fixture = Fixture::new();
    let path = fixture.file("images.json", b"{}\n");
    let sentinel = fixture.file("sentinel", b"foreign");
    let mut control = Control::new(Instant::now()).unwrap();
    files::Original::fixture(&path, 3, 0o600, &mut control)
        .unwrap()
        .retire_exact(b"{}\n", &mut control)
        .unwrap();
    assert!(!path.exists());
    assert_eq!(fs::read(sentinel).unwrap(), b"foreign");
    let wrong = fixture.file("images.json", b"bad");
    assert_eq!(
        files::Original::fixture(&wrong, 3, 0o600, &mut control)
            .unwrap()
            .retire_exact(b"{}\n", &mut control),
        Err(Error::Changed)
    );
    assert_eq!(fs::read(wrong).unwrap(), b"bad");
    control.begin_settling();
    assert_eq!(control.finish(), Err(Error::Changed));
}
#[test]
fn replaced_original_symlink_hardlink_and_fifo_never_become_handoff_authority() {
    let fixture = Fixture::new();
    let path = fixture.file("original", b"{}\n");
    let mut control = Control::new(Instant::now()).unwrap();
    let mut original = files::Original::fixture(&path, 3, 0o600, &mut control).unwrap();
    fs::remove_file(&path).unwrap();
    fixture.file("original", b"{}\n");
    assert_eq!(original.check(&mut control), Err(Error::Changed));
    assert_eq!(original.finish(&mut control), Err(Error::Changed));
    let linked = fixture.0.join("linked");
    std::os::unix::fs::symlink(&path, &linked).unwrap();
    assert!(files::Original::fixture(&linked, 3, 0o600, &mut control).is_err());
    let hard = fixture.0.join("hard");
    fs::hard_link(&path, &hard).unwrap();
    assert!(files::Original::fixture(&path, 3, 0o600, &mut control).is_err());
    assert_eq!(fs::read(hard).unwrap(), b"{}\n");
    let fifo = fixture.0.join("fifo");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();
    assert!(files::Original::fixture(&fifo, 3, 0o600, &mut control).is_err());
    control.begin_settling();
    assert!(control.finish().is_err());
}
#[test]
fn helper_mismatch_and_changed_read_keep_the_original_for_explicit_finish() {
    for changed in [false, true] {
        let fixture = Fixture::new();
        let path = fixture.file("helper", b"original");
        let mut control = Control::new(Instant::now()).unwrap();
        let original = files::Original::fixture(&path, 64, 0o600, &mut control).unwrap();
        if changed {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        }
        let mut originals = Vec::new();
        assert_eq!(
            retain_matching(original, b"unexpected", &mut originals, &mut control),
            Err(Error::Changed)
        );
        assert_eq!(originals.len(), 1);
        assert!(originals[0].file().is_ok());
        control.record(Error::Changed);
        let result = originals[0].finish(&mut control);
        assert_eq!(result.is_err(), changed);
        assert!(matches!(originals[0].file(), Err(Error::Close)));
        assert!(!control.cleanup_failed);
        control.begin_settling();
        assert_eq!(control.finish(), Err(Error::Changed));
    }
}
#[test]
fn full_dirty_join_refuses_same_inode_permission_and_hardlink_changes() {
    use std::os::unix::fs::MetadataExt;
    let fixture = Fixture::new();
    let path = fixture.file("README.md", b"stable");
    let before = fs::symlink_metadata(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    let changed = fs::symlink_metadata(&path).unwrap();
    assert_eq!(
        (before.dev(), before.ino(), before.len()),
        (changed.dev(), changed.ino(), changed.len())
    );
    assert!(!files::same(&before, &changed));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let before = fs::symlink_metadata(&path).unwrap();
    fs::hard_link(&path, fixture.0.join("linked")).unwrap();
    let changed = fs::symlink_metadata(&path).unwrap();
    assert_eq!(
        (before.dev(), before.ino(), before.len()),
        (changed.dev(), changed.ino(), changed.len())
    );
    assert!(!files::same(&before, &changed));
    assert_eq!(fs::read(path).unwrap(), b"stable");
}
#[test]
fn stable_tool_hardlinks_are_admitted_but_private_inputs_and_link_mutations_refuse() {
    let fixture = Fixture::new();
    let path = fixture.file("tool", b"fixed inert executable");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    fs::hard_link(&path, fixture.0.join("tool-peer")).unwrap();
    let mut control = Control::new(Instant::now()).unwrap();
    let mut original = files::Original::fixture_tool(&path, &mut control).unwrap();
    original.check(&mut control).unwrap();
    assert!(files::Original::fixture(&path, 64, 0o755, &mut control).is_err());
    fs::remove_file(fixture.0.join("tool-peer")).unwrap();
    assert_eq!(original.check(&mut control), Err(Error::Changed));
    assert_eq!(original.finish(&mut control), Err(Error::Changed));
    assert!(matches!(original.file(), Err(Error::Close)));
    assert!(!control.cleanup_failed);
    control.begin_settling();
    assert!(control.finish().is_err());
}
#[test]
fn unknown_observation_names_and_missing_success_facts_cannot_be_promoted() {
    let mut facts = Observations::new();
    assert!(facts.set("SECRET", true).is_err());
    assert!(facts.validate_success("positive").is_err());
    assert_eq!(facts.as_value().as_object().unwrap().len(), 24);
    assert!(!facts.as_value().to_string().contains("SECRET"));
}
