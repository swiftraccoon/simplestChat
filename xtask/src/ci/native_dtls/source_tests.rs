//! Owned inert fixtures for the native DTLS controller.

use super::*;
use crate::ci::native_dtls::fixtures::{DIAGNOSTIC, Fixture};
use std::{fs, path::Path, time::Duration};

fn files() -> BTreeMap<String, Vec<u8>> {
    BTreeMap::from([
        (
            "meson.build".into(),
            b"production\r\nmediasoup_worker_test = executable(\r\n".to_vec(),
        ),
        (
            "test/src/one.cpp".into(),
            b"[dtls] [media-diagnostics]".to_vec(),
        ),
        (
            "test/src/sub/two.cpp".into(),
            b"[bandwidth-constraints]".to_vec(),
        ),
        ("test/src/ignored space.cpp".into(), b"unrelated".to_vec()),
    ])
}
async fn oracle(root: &Path, files: &BTreeMap<String, Vec<u8>>) -> Option<Vec<u8>> {
    let worker = root.join("oracle");
    fs::create_dir(&worker).unwrap();
    for (name, bytes) in files {
        let path = worker.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    let reference = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("build/check-native-dtls.sh"),
    )
    .unwrap();
    let script = reference
        .split("\"${dtls_python}\" - \"${dtls_temp}/worker\" <<'PY'\n")
        .nth(1)
        .unwrap()
        .split("\nPY\n")
        .next()
        .unwrap();
    let script_path = root.join("selection.py");
    fs::write(&script_path, script).unwrap();
    let mut command = std::process::Command::new("/usr/bin/python3");
    command
        .arg(&script_path)
        .arg(&worker)
        .stderr(std::process::Stdio::null());
    let (status, _) =
        crate::test_env::helper_with_signals(command, Duration::from_secs(5), Some(4096), None)
            .await
            .unwrap();
    (status == 0).then(|| fs::read(worker.join("meson.build")).unwrap())
}
#[tokio::test]
async fn tagged_selection_matches_unchanged_python_oracle_including_rejections_and_newlines() {
    for case in 0..9 {
        let fixture = Fixture::new();
        let mut files = files();
        match case {
            1 => {
                files.insert("test/src/extra.cpp".into(), b"[dtls]".to_vec());
            }
            2 => {
                files.insert("test/src/.cpp".into(), b"[dtls]".to_vec());
            }
            3 => {
                files.insert("test/src/private space.cpp".into(), b"[dtls]".to_vec());
            }
            4 => {
                files.remove("test/src/sub/two.cpp");
            }
            5 => {
                files.insert("meson.build".into(), b"no anchor".to_vec());
            }
            6 => {
                files.insert("meson.build".into(), ANCHOR.repeat(2).into_bytes());
            }
            7 => {
                files.insert("test/src/unicode.cpp".into(), vec![255]);
            }
            8 => {
                files.insert(
                    "test/src/stray.cpp".into(),
                    b"[dtls]\r[media-diagnostics]".to_vec(),
                );
            }
            _ => {}
        }
        assert_eq!(
            meson(&files).ok(),
            oracle(&fixture.root, &files).await,
            "case{case}"
        );
    }
}
#[test]
fn fixed_log_assertions_preserve_warning_absence_and_complete_context_bytes() {
    assert!(assertions(DIAGNOSTIC.as_bytes(), b"quiet").is_ok());
    assert!(assertions(DIAGNOSTIC.as_bytes(), b"RTC::DtlsTransport::close warning").is_err());
    assert!(
        assertions(
            DIAGNOSTIC
                .replace("mediaObserved:1", "mediaObserved:0")
                .as_bytes(),
            b""
        )
        .is_err()
    );
    assert!(assertions(b"unrelated", b"").is_err());
}
