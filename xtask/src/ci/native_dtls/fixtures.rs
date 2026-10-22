//! Tiny owned source/tool trees; no compiler, package installer or native test.

use super::plan::Context;
use crate::tooling::Environment;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
pub(super) const DIAGNOSTIC: &str = concat!(
    "producer:11112233-4455-6677-8899-aabbccddeeff, ",
    "transport:00112233-4455-6677-8899-aabbccddeeff, kind:audio, encodingIdx:2, ",
    "workerMs:2500, timeoutMs:1500, mediaObserved:1, lastMediaMs:1000, paused:0"
);
pub(super) struct Fixture {
    pub root: PathBuf,
    pub context: Context,
}
impl Fixture {
    pub fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "native dtls fixture {} {}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let checkout = root.join("checkout with spaces");
        let temporary = root.join("temporary");
        let openssl = root.join("openssl");
        let bin = root.join("bin");
        for path in [&checkout, &temporary, &bin, &openssl.join("lib/pkgconfig")] {
            fs::create_dir_all(path).unwrap();
        }
        fs::write(openssl.join("lib/pkgconfig/openssl.pc"), "owned fixture").unwrap();
        for name in ["python named", "clang", "clang++", "pkg-config"] {
            let path = bin.join(name);
            fs::write(&path, b"#!/usr/bin/python3\nraise SystemExit(0)\n").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let environment = Environment::from([
            ("PATH".into(), bin.into()),
            ("PYTHON".into(), "python named".into()),
            ("OPENSSL_DIR".into(), openssl.into()),
            ("RUNNER_TEMP".into(), temporary.into()),
            (
                "PRIVATE_SENTINEL".into(),
                "private-not-in-native-environment".into(),
            ),
            ("PYTHONPATH".into(), "private-module-injection".into()),
            (
                "PIP_EXTRA_INDEX_URL".into(),
                "https://private.invalid/".into(),
            ),
        ]);
        let context = Context::new(&checkout, &checkout, environment, "Linux", false).unwrap();
        let worker = checkout.join("vendor/mediasoup-sys-0.19.0");
        for (name, value) in [
            (
                "meson.build",
                "production = ['all.cpp']\nmediasoup_worker_test = executable(\n",
            ),
            ("test/src/dtls.cpp", "[dtls]"),
            ("test/src/nested/media.cpp", "[media-diagnostics]"),
            ("test/src/bandwidth.cpp", "[bandwidth-constraints]"),
            ("test/src/tests.cpp", "runner"),
            ("test/src/testHelpers.cpp", "helper"),
            ("python-invoke-requirements.txt", "owned fixture"),
        ] {
            let path = worker.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, value).unwrap();
        }
        Self { root, context }
    }
    pub fn prefixes(&self) -> Vec<PathBuf> {
        fs::read_dir(&self.context.temporary)
            .unwrap()
            .map(|value| value.unwrap().path())
            .filter(|value| {
                value
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("simplestchat-native-dtls.")
            })
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
