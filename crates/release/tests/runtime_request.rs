//! The built simplestchat-release writes the hosted Ops runtime proof's
//! request: canonical identifiers and revision only, one exclusive private
//! file, and the digest of its exact bytes as the step output.

use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU32, Ordering},
};

const MAX_ID: u64 = 9_007_199_254_740_991;
const IDENTIFIERS: [&str; 3] = ["OPS_RUN_ID", "OPS_RUN_ATTEMPT", "OPS_ARTIFACT_ID"];

/// An owned runner directory and step output, removed when the test ends.
struct Fixture {
    root: PathBuf,
    runner: PathBuf,
    request: PathBuf,
    output: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "ops-runtime-request-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let runner = root.join("runner");
        fs::create_dir_all(&runner).unwrap();
        fs::set_permissions(&runner, fs::Permissions::from_mode(0o700)).unwrap();
        let output = root.join("github-output");
        fs::write(&output, b"").unwrap();
        fs::set_permissions(&output, fs::Permissions::from_mode(0o600)).unwrap();
        Self {
            request: runner.join("ops-runtime-request.json"),
            root,
            runner,
            output,
        }
    }

    /// Run the built writer with only this environment and a permissive
    /// umask. No credential or ambient variable reaches it.
    fn invoke(&self, overrides: &[(&str, &str)]) -> Output {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "umask 0 && exec \"$0\" ops-artifact runtime-request"])
            .arg(env!("CARGO_BIN_EXE_simplestchat-release"))
            .current_dir(&self.root)
            .env_clear()
            .env("RUNNER_TEMP", &self.runner)
            .env("GITHUB_OUTPUT", &self.output)
            .env("GITHUB_SHA", "a".repeat(40))
            .env("OPS_RUN_ID", "1")
            .env("OPS_RUN_ATTEMPT", MAX_ID.to_string())
            .env("OPS_ARTIFACT_ID", "42");
        for (name, value) in overrides {
            command.env(name, value);
        }
        command.output().unwrap()
    }

    /// Nothing was published: no request leaf and an untouched step output.
    fn unpublished(&self) -> bool {
        fs::symlink_metadata(&self.request).is_err() && fs::read(&self.output).unwrap().is_empty()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn original_request_sha_private_mode_and_source_identity_are_pinned() {
    // The emitted SHA binds the exact closed bytes, including their final newline.
    let fixture = Fixture::new();
    let completed = fixture.invoke(&[]);
    assert!(completed.status.success(), "{:?}", completed.status);
    let original = fs::read(&fixture.request).unwrap();
    let expected = format!(
        concat!(
            "{{\"schemaVersion\":1,\"selection\":{{",
            "\"repository\":\"swiftraccoon/simplestChat\",\"revision\":\"{}\",",
            "\"packageVersion\":\"{}\",\"target\":\"x86_64-unknown-linux-musl\",",
            "\"protocolVersion\":1,\"buildRunId\":1,\"runAttempt\":{}}},",
            "\"artifactId\":42,\"output\":\"{}\",\"authentication\":\"gh-token\"}}\n"
        ),
        "a".repeat(40),
        env!("CARGO_PKG_VERSION"),
        MAX_ID,
        fixture.runner.join("ops-runtime").to_str().unwrap()
    );
    assert_eq!(String::from_utf8(original.clone()).unwrap(), expected);
    assert_eq!(
        fs::read_to_string(&fixture.output).unwrap(),
        format!("sha256={}\n", sha256(&original))
    );
    let metadata = fs::metadata(&fixture.request).unwrap();
    assert_eq!(metadata.permissions().mode() & 0o7777, 0o600);
    assert_eq!(metadata.uid(), rustix::process::getuid().as_raw());
    assert!(completed.stdout.is_empty());
    assert!(completed.stderr.is_empty());
}

#[test]
fn identifier_failures_precede_request_or_output_publication() {
    // Ambiguous numbers, zero, overflow and non-ASCII forms never select an artifact.
    let fixture = Fixture::new();
    let overflow = (MAX_ID + 1).to_string();
    let long = "9".repeat(17);
    for name in IDENTIFIERS {
        for raw in [
            "", "0", "-1", "+1", "01", "1.0", "1e3", " 1", "1\n", "a", &overflow, &long, "\u{ff19}",
        ] {
            let completed = fixture.invoke(&[(name, raw)]);
            assert!(!completed.status.success(), "{name} {raw:?}");
            assert_eq!(
                completed.stderr, b"Invalid Ops artifact identifier.\n",
                "{name} {raw:?}"
            );
            assert!(completed.stdout.is_empty(), "{name} {raw:?}");
            assert!(fixture.unpublished(), "{name} {raw:?}");
        }
    }
}

#[test]
fn all_identifier_fields_accept_the_exact_upper_boundary() {
    // The full safe-integer boundary remains usable as an integer in every role.
    let fixture = Fixture::new();
    let boundary = MAX_ID.to_string();
    let overrides = IDENTIFIERS.map(|name| (name, boundary.as_str()));
    let completed = fixture.invoke(&overrides);
    assert!(completed.status.success(), "{:?}", completed.status);
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(&fixture.request).unwrap()).unwrap();
    assert_eq!(value["selection"]["buildRunId"], MAX_ID);
    assert_eq!(value["selection"]["runAttempt"], MAX_ID);
    assert_eq!(value["artifactId"], MAX_ID);
}

#[test]
fn noncanonical_source_revision_never_writes_a_request() {
    // The source identity comes only from a canonical event SHA.
    let fixture = Fixture::new();
    let (short, long) = ("a".repeat(39), "a".repeat(41));
    let (upper, letters) = ("A".repeat(40), "g".repeat(40));
    let newline = format!("{}\n", "a".repeat(40));
    for revision in ["", &short, &long, &upper, &letters, &newline] {
        let completed = fixture.invoke(&[("GITHUB_SHA", revision)]);
        assert!(!completed.status.success(), "{revision:?}");
        assert_eq!(
            completed.stderr, b"Invalid Ops source revision.\n",
            "{revision:?}"
        );
        assert!(completed.stdout.is_empty(), "{revision:?}");
        assert!(fixture.unpublished(), "{revision:?}");
    }
}

#[test]
fn exclusive_creation_preserves_regular_symlink_and_fifo_collisions() {
    // No existing request path or symlink target can be adopted or overwritten.
    let fixture = Fixture::new();
    let target = fixture.root.join("keep");
    fs::write(&target, b"owned sentinel").unwrap();
    for kind in ["regular", "symlink", "fifo"] {
        match kind {
            "regular" => fs::write(&fixture.request, b"pending request").unwrap(),
            "symlink" => symlink(&target, &fixture.request).unwrap(),
            _ => {
                let made = Command::new("/usr/bin/mkfifo")
                    .args(["-m", "600"])
                    .arg(&fixture.request)
                    .status()
                    .unwrap();
                assert!(made.success());
            }
        }
        let identity = || {
            let entry = fs::symlink_metadata(&fixture.request).unwrap();
            let modified = (entry.mtime(), entry.mtime_nsec());
            (
                entry.dev(),
                entry.ino(),
                entry.mode(),
                entry.size(),
                modified,
            )
        };
        let before = identity();
        let completed = fixture.invoke(&[]);
        assert!(!completed.status.success(), "{kind}");
        assert_eq!(identity(), before, "{kind}");
        assert_eq!(fs::read(&target).unwrap(), b"owned sentinel", "{kind}");
        assert!(fs::read(&fixture.output).unwrap().is_empty(), "{kind}");
        assert!(completed.stdout.is_empty(), "{kind}");
        assert_eq!(
            completed.stderr, b"Ops runtime request is unwritten.\n",
            "{kind}"
        );
        fs::remove_file(&fixture.request).unwrap();
    }
}
