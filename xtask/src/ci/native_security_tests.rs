//! Regressions for the fixed native-patch gate using owned files and fake tools.

use super::*;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "simplestchat-native-policy-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&root).unwrap();
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        for name in WRAPS
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .chain(FILES.iter().map(|(name, _)| format!("packagefiles/{name}")))
        {
            let destination = root.join(PREFIX).join(&name);
            fs::create_dir_all(destination.parent().unwrap()).unwrap();
            fs::copy(repository.join(PREFIX).join(name), destination).unwrap();
        }
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::write(root.join("target/debug/simplestChat"), OPENSSL).unwrap();
        Self(root)
    }

    fn verify(&self) -> Result<()> {
        verify(&self.0, fake)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn fake(program: &str, arguments: &[&str]) -> Result<Vec<u8>> {
    Ok(match (program, arguments) {
        ("cargo", ["tree", "--locked", "-i", "lru"]) => b"lru v0.18.5\n".to_vec(),
        ("cargo", ["tree", "--locked", "-i", "mediasoup-sys"]) => {
            b"mediasoup-sys v0.19.0 (/owned/checkout/vendor/mediasoup-sys-0.19.0)\n".to_vec()
        }
        ("ldd", ["target/debug/simplestChat"]) => b"libc.so.6 => /lib/libc.so.6\n".to_vec(),
        _ => panic!("unexpected command boundary"),
    })
}

#[test]
fn pinned_sources_and_static_linkage_preserve_the_fixed_command_boundary() {
    let fixture = Fixture::new();
    let mut calls = Vec::new();
    verify(&fixture.0, |program, arguments| {
        calls.push((
            program.to_owned(),
            arguments
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>(),
        ));
        fake(program, arguments)
    })
    .unwrap();
    assert_eq!(calls.len(), 3);
    assert_eq!(
        calls[2],
        ("ldd".into(), vec!["target/debug/simplestChat".into()])
    );
}

#[test]
fn dependency_graph_must_contain_the_reviewed_versions_and_vendor_location() {
    let fixture = Fixture::new();
    for (selected, replacement, code) in [
        ("lru", "lru v0.18.4\n", "ci_native_lru"),
        ("lru", "lru v0.18.50\n", "ci_native_lru"),
        (
            "mediasoup-sys",
            "mediasoup-sys v0.19.0\n",
            "ci_native_mediasoup",
        ),
        (
            "mediasoup-sys",
            "mediasoup-sys v0.19.0 (/other/vendor/mediasoup-sys-0.19.0-extra)\n",
            "ci_native_mediasoup",
        ),
    ] {
        let error = verify(&fixture.0, |program, arguments| {
            if arguments.last() == Some(&selected) {
                Ok(replacement.as_bytes().to_vec())
            } else {
                fake(program, arguments)
            }
        })
        .unwrap_err();
        assert_eq!(error.to_string(), code);
    }
}

#[test]
fn every_source_pin_and_overlay_is_checked() {
    let fixture = Fixture::new();
    for (name, pin) in WRAPS {
        let path = fixture.0.join(PREFIX).join(name);
        let original = fs::read_to_string(&path).unwrap();
        assert!(original.contains(pin));
        fs::write(&path, original.replace(pin, "source changed")).unwrap();
        assert_eq!(
            fixture.verify().unwrap_err().to_string(),
            "ci_native_source_pin"
        );
        fs::write(path, original).unwrap();
    }
    for (name, _) in FILES {
        let path = fixture.0.join(PREFIX).join("packagefiles").join(name);
        let original = fs::read(&path).unwrap();
        fs::write(&path, b"unreviewed overlay").unwrap();
        assert_eq!(
            fixture.verify().unwrap_err().to_string(),
            "ci_native_overlay_hash"
        );
        fs::write(path, original).unwrap();
    }
}

#[test]
fn old_abseil_and_fallback_sources_are_rejected_even_with_current_pins_present() {
    let fixture = Fixture::new();
    let path = fixture.0.join(PREFIX).join("abseil-cpp.wrap");
    let original = fs::read_to_string(&path).unwrap();
    for (extra, code) in [
        (
            "source_filename = abseil-cpp-20240722.0.tar.gz",
            "ci_native_old_abseil",
        ),
        (
            "source_fallback_url = https://example.invalid/archive",
            "ci_native_abseil_fallback",
        ),
    ] {
        fs::write(&path, format!("{original}\n{extra}\n")).unwrap();
        assert_eq!(fixture.verify().unwrap_err().to_string(), code);
    }
}

#[test]
fn openssl_scanning_handles_chunk_boundaries_and_does_not_hide_forbidden_versions() {
    let fixture = Fixture::new();
    let path = fixture.0.join("target/debug/simplestChat");
    for boundary in 65510..65570 {
        let mut bytes = vec![0; boundary];
        bytes.extend_from_slice(OPENSSL);
        fs::write(&path, &bytes).unwrap();
        binary(&path).unwrap();
        bytes.extend_from_slice(OLD_OPENSSL);
        fs::write(&path, &bytes).unwrap();
        assert_eq!(
            binary(&path).unwrap_err().to_string(),
            "ci_native_old_openssl"
        );
    }
    fs::write(&path, b"different version").unwrap();
    assert_eq!(
        binary(&path).unwrap_err().to_string(),
        "ci_native_openssl_version"
    );
    fs::remove_file(&path).unwrap();
    symlink(fixture.0.join(PREFIX).join("abseil-cpp.wrap"), &path).unwrap();
    assert!(binary(&path).is_err());
}

#[test]
fn dynamic_linkage_and_tool_failures_cannot_pass_the_gate() {
    let fixture = Fixture::new();
    for library in ["libssl.so.4", "libcrypto.so.4"] {
        let error = verify(&fixture.0, |program, arguments| {
            if program == "ldd" {
                Ok(library.as_bytes().to_vec())
            } else {
                fake(program, arguments)
            }
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "ci_native_dynamic_openssl");
    }
    assert!(verify(&fixture.0, |_, _| anyhow::bail!("fixed tool failure")).is_err());
    assert_eq!(
        run(&fixture.0, &["unexpected".into()]).unwrap(),
        std::process::ExitCode::from(2)
    );
}

#[test]
fn read_only_children_are_bounded_and_failed_tools_do_not_echo_their_output() {
    let fixture = Fixture::new();
    let success = capture_with_deadline(
        &fixture.0,
        "/bin/sh",
        &["-c", "printf owned"],
        Duration::from_secs(2),
    )
    .unwrap();
    assert_eq!(success, b"owned");
    let failed = capture_with_deadline(
        &fixture.0,
        "/bin/sh",
        &["-c", "printf PRIVATE >&2; exit 7"],
        Duration::from_secs(2),
    )
    .unwrap_err();
    assert_eq!(failed.to_string(), "ci_native_tool_failed");
    let oversized = capture_with_deadline(
        &fixture.0,
        "/bin/sh",
        &["-c", "head -c 65537 /dev/zero >&2"],
        Duration::from_secs(2),
    )
    .unwrap_err();
    assert_eq!(oversized.to_string(), "ci_native_tool_output");
    let timeout = capture_with_deadline(
        &fixture.0,
        "/bin/sh",
        &["-c", "sleep 60"],
        Duration::from_millis(100),
    )
    .unwrap_err();
    assert_eq!(timeout.to_string(), "ci_native_tool_timeout");
}
