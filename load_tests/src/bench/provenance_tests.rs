//! Owned Git fixtures and actual JavaScript exports; no benchmark is started.

use super::*;
use crate::bench::process::NativeHost;
use std::io::Write;
use std::os::unix::fs::symlink;
use std::process::{Command as Process, Stdio};

struct Fixture {
    root: PathBuf,
    binary: PathBuf,
    environment: BTreeMap<OsString, OsString>,
}

impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("benchmark provenance {}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let fixture = Self {
            binary: root.join("fixture binary"),
            root,
            environment: [
                ("PATH".into(), std::env::var_os("PATH").unwrap()),
                ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
                ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ]
            .into(),
        };
        fixture.write("Cargo.toml", b"[package]\nname='fixture'\n");
        fixture.write("Cargo.lock", b"# fixture lock\n");
        fixture.write("src/lib.rs", b"pub fn fixture() {}\n");
        fixture.write("src/deleted.rs", b"// remove later\n");
        fixture.write("vendor/native/input.cc", b"// native fixture\n");
        fixture.write("load_tests/src/main.rs", b"// generator main\n");
        fixture.write("load_tests/src/nested/module.rs", b"// generator module\n");
        fixture.write(".gitignore", b"src/ignored.rs\n.cargo/config.toml\n");
        fs::write(&fixture.binary, b"frozen executable fixture").unwrap();
        fixture.git(&["init", "-q"]);
        fixture.git(&["config", "user.name", "Owned fixture"]);
        fixture.git(&["config", "user.email", "fixture@example.invalid"]);
        fixture.git(&["config", "commit.gpgsign", "false"]);
        fixture.git(&[
            "add",
            "Cargo.toml",
            "Cargo.lock",
            "src",
            "vendor",
            ".gitignore",
        ]);
        fixture.git(&["commit", "-q", "-m", "Initial fixture"]);
        fixture
    }

    fn write(&self, path: &str, bytes: &[u8]) {
        let path = self.root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn git(&self, arguments: &[&str]) {
        let output = Process::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(arguments)
            .env_clear()
            .envs(&self.environment)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "Owned Git fixture command must succeed"
        );
    }

    fn oracle(&self, operation: &str, arguments: Value) -> Value {
        let reference = std::env::var_os("BENCH_REFERENCE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .unwrap()
                    .into()
            });
        let script = r#"
import {readFileSync} from 'node:fs';
import {pathToFileURL} from 'node:url';
import {join} from 'node:path';
const policy=await import(pathToFileURL(join(process.argv[1],'load_tests/benchmark-local.mjs')));
const request=JSON.parse(readFileSync(0,'utf8'));
process.stdout.write(JSON.stringify(await policy[request.operation](...request.arguments)));
"#;
        let mut child = Process::new("node")
            .args(["--input-type=module", "-e", script])
            .arg(reference)
            .current_dir(&self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(
                &serde_json::to_vec(&json!({"operation":operation,"arguments":arguments})).unwrap(),
            )
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "Actual JavaScript identity export must succeed"
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    async fn tree(&self) -> SourceTreeIdentity {
        let tree = source_tree_identity(&NativeHost::default(), &self.environment, &self.root)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(&tree).unwrap(),
            self.oracle("sourceTreeIdentity", json!([self.root]))
        );
        tree
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[tokio::test]
async fn actual_js_tree_identity_matches_untracked_staging_deletion_and_commit_transitions() {
    let fixture = Fixture::new();
    let initial = fixture.tree().await;
    fixture.write("src/new.rs", b"// new source\n");
    let untracked = fixture.tree().await;
    assert_ne!(initial.source_tree_sha256, untracked.source_tree_sha256);
    fixture.git(&["add", "src/new.rs"]);
    assert_eq!(fixture.tree().await, untracked);
    fixture.git(&["commit", "-q", "-m", "Track source"]);
    assert_eq!(fixture.tree().await, untracked);
    fs::remove_file(fixture.root.join("src/deleted.rs")).unwrap();
    let deleted = fixture.tree().await;
    assert_eq!(deleted.source_tree_missing_paths, ["src/deleted.rs"]);
    fixture.git(&["add", "-u"]);
    assert_eq!(fixture.tree().await, deleted);
    fixture.git(&["commit", "-q", "-m", "Delete source"]);
    let committed = fixture.tree().await;
    assert_eq!(committed.source_tree_sha256, deleted.source_tree_sha256);
    assert!(committed.source_tree_missing_paths.is_empty());
}

#[tokio::test]
async fn private_exclusions_path_boundaries_and_executable_modes_match_actual_js() {
    let fixture = Fixture::new();
    let initial = fixture.tree().await;
    for path in [
        "CLAUDE.md",
        "src/CLAUDE.md",
        "vendor/CLAUDE.local.md",
        "src/ignored.rs",
        ".cargo/config.toml",
        "results/private.json",
    ] {
        fixture.write(path, b"private or excluded\n");
    }
    assert_eq!(fixture.tree().await, initial);
    fixture.write("src/line\nbreak.rs", b"first");
    fixture.write("src/other.rs", b"second");
    let paths = fixture.tree().await;
    fixture.write("src/line\nbreak.rs", b"second");
    fixture.write("src/other.rs", b"first");
    let swapped = fixture.tree().await;
    assert_ne!(paths.source_tree_sha256, swapped.source_tree_sha256);
    fs::set_permissions(
        fixture.root.join("src/other.rs"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert_ne!(
        fixture.tree().await.source_tree_sha256,
        swapped.source_tree_sha256
    );
    fixture.write("build.rs", b"// build input\n");
    fixture.write(".cargo/config", b"[build]\n");
    assert_ne!(
        fixture.tree().await.source_tree_sha256,
        initial.source_tree_sha256
    );
}

#[tokio::test]
async fn symlink_targets_are_hashed_without_reading_external_contents() {
    let fixture = Fixture::new();
    let outside = fixture.root.join("outside scope");
    fs::write(&outside, b"first outside content").unwrap();
    symlink(&outside, fixture.root.join("src/linked.rs")).unwrap();
    let initial = fixture.tree().await;
    fs::write(&outside, b"changed outside content").unwrap();
    assert_eq!(fixture.tree().await, initial);
    fs::remove_file(fixture.root.join("src/linked.rs")).unwrap();
    symlink("nonexistent target", fixture.root.join("src/linked.rs")).unwrap();
    assert_ne!(
        fixture.tree().await.source_tree_sha256,
        initial.source_tree_sha256
    );
}

#[test]
fn generator_identity_matches_actual_js_for_add_rename_and_symlink_exclusion() {
    let fixture = Fixture::new();
    let verify = || {
        let identity = generator_source_identity(&fixture.root).unwrap();
        assert_eq!(
            json!(identity),
            fixture.oracle("generatorSourceIdentity", json!([fixture.root]))
        );
        identity
    };
    let initial = verify();
    fixture.write("load_tests/src/new.rs", b"// newly discovered module\n");
    let added = verify();
    assert_ne!(initial, added);
    fs::rename(
        fixture.root.join("load_tests/src/new.rs"),
        fixture.root.join("load_tests/src/renamed.rs"),
    )
    .unwrap();
    let renamed = verify();
    assert_ne!(added, renamed);
    fixture.write("load_tests/src/README.md", b"outside runtime scope\n");
    symlink(
        "/unreadable/outside.rs",
        fixture.root.join("load_tests/src/linked.rs"),
    )
    .unwrap();
    symlink(
        "/unreadable/outside",
        fixture.root.join("load_tests/src/linked-directory"),
    )
    .unwrap();
    assert_eq!(verify(), renamed);
}

#[tokio::test]
async fn server_identity_and_legacy_diff_hash_match_actual_js_independently_of_binary_changes() {
    let fixture = Fixture::new();
    fixture.write("src/lib.rs", b"pub fn changed() {}\n");
    let host = NativeHost::default();
    let original = identity(&host, &fixture.environment, &fixture.root, &fixture.binary)
        .await
        .unwrap();
    let reference = fixture.oracle("identity", json!([fixture.root, fixture.binary]));
    assert_eq!(serde_json::to_value(&original).unwrap(), reference);
    assert_eq!(
        json!(server_revision_label(&original)),
        fixture.oracle("serverRevisionLabel", json!([reference]))
    );
    verify_executable(&fixture.binary, &original.binary_sha256, "Generator").unwrap();
    fs::write(&fixture.binary, b"different executable bytes").unwrap();
    assert!(verify_executable(&fixture.binary, &original.binary_sha256, "Generator").is_err());
    let changed = identity(&host, &fixture.environment, &fixture.root, &fixture.binary)
        .await
        .unwrap();
    assert_eq!(original.source_tree, changed.source_tree);
    assert_ne!(original.binary_sha256, changed.binary_sha256);
    let link = fixture.root.join("binary link");
    symlink(&fixture.binary, &link).unwrap();
    assert_eq!(
        hash_file(&link).unwrap(),
        hash_file(&fixture.binary).unwrap()
    );
}

#[tokio::test]
async fn ancestor_symlinks_and_special_inputs_refuse_without_following_them() {
    let fixture = Fixture::new();
    fs::rename(fixture.root.join("src"), fixture.root.join("moved source")).unwrap();
    symlink("moved source", fixture.root.join("src")).unwrap();
    assert!(
        source_tree_identity(&NativeHost::default(), &fixture.environment, &fixture.root)
            .await
            .is_err()
    );
    fs::remove_dir_all(fixture.root.join("load_tests")).unwrap();
    symlink("moved source", fixture.root.join("load_tests")).unwrap();
    assert!(generator_source_identity(&fixture.root).is_err());
    assert!(hash_file(&fixture.root).is_err());
    assert!(paths("src/valid.rs\0../private\0").is_err());
    assert!(paths("src/valid.rs").is_err());
}

struct ProbeHost {
    code: Option<i32>,
    signal: Option<String>,
    cancel: bool,
    close_error: bool,
    stops: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

struct ProbeChild {
    exit: Option<super::super::process::Exit>,
    close_error: bool,
    stops: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl Host for ProbeHost {
    fn spawn(&self, command: Command) -> Result<Box<dyn super::super::process::Child>> {
        assert_eq!(command.executable, Path::new("git"));
        assert_eq!(
            command.arguments,
            ["-C", "/literal $(text) workspace", "rev-parse", "HEAD"]
        );
        assert_eq!(command.timeout, Duration::from_secs(20));
        assert_eq!(command.stdout_limit, 64);
        assert_eq!(command.stderr_limit, MIB);
        assert!(!command.cleanup);
        assert!(command.input.is_empty());
        assert!(matches!(command.stdout, Sink::Capture));
        assert!(matches!(command.stderr, Sink::Capture));
        Ok(Box::new(ProbeChild {
            exit: if self.cancel {
                None
            } else {
                Some(super::super::process::Exit {
                    code: self.code,
                    signal: self.signal.clone(),
                })
            },
            close_error: self.close_error,
            stops: self.stops.clone(),
        }))
    }
    async fn cancelled(&self) -> u8 {
        143
    }
}

#[async_trait::async_trait]
impl super::super::process::Child for ProbeChild {
    fn pid(&self) -> u32 {
        1
    }
    fn take_stdout(&mut self) -> Option<Box<dyn super::super::process::Stream>> {
        None
    }
    async fn poll(&mut self) -> Result<Option<super::super::process::Exit>> {
        Ok(self.exit.clone())
    }
    async fn stop(&mut self, grace: Duration) -> Result<super::super::process::Exit> {
        assert_eq!(grace, Duration::ZERO);
        self.stops.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        ensure!(!self.close_error, "Owned command close failed");
        Ok(super::super::process::Exit {
            code: Some(0),
            signal: None,
        })
    }
    fn output(&self) -> &[u8] {
        b"PRIVATE output sentinel"
    }
    fn errors(&self) -> &[u8] {
        b"PRIVATE error sentinel"
    }
}

#[tokio::test]
async fn failed_signaled_cancelled_or_unclosed_identity_commands_stop_without_echoing_private_output()
 {
    for (code, signal, cancel, close_error) in [
        (Some(9), None, false, false),
        (None, Some("SIGTERM".into()), false, false),
        (Some(0), None, true, false),
        (Some(0), None, false, true),
    ] {
        let stops = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let host = ProbeHost {
            code,
            signal,
            cancel,
            close_error,
            stops: stops.clone(),
        };
        let error = git(
            &host,
            &BTreeMap::new(),
            Path::new("/literal $(text) workspace"),
            &["rev-parse", "HEAD"],
            64,
        )
        .await
        .unwrap_err();
        assert!(!error.to_string().contains("PRIVATE"));
        assert_eq!(stops.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
