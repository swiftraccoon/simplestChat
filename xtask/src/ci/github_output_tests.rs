//! Fixed framing, actual append descriptors and fake local Git commands. No
//! GitHub service, cache, application process or global environment is changed.

use super::*;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture {
    directory: PathBuf,
    context: Context,
    output: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "ci output {} {} spaces",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let directory = directory.canonicalize().unwrap();
        let output = directory.join("output $(touch unowned); literal");
        fs::write(&output, b"prior=preserved\n").unwrap();
        let context = Context {
            root: directory.clone(),
            environment: BTreeMap::from([
                ("GITHUB_OUTPUT".into(), output.to_str().unwrap().into()),
                ("ImageOS".into(), "ubuntu24".into()),
                ("ImageVersion".into(), "20261020.1".into()),
                ("GITHUB_REF".into(), "refs/heads/main".into()),
                ("GITHUB_EVENT_NAME".into(), "push".into()),
            ]),
            system: "Linux".into(),
            architecture: "x86_64".into(),
        };
        Self {
            directory,
            context,
            output,
        }
    }

    fn fake_git(&mut self, failure: bool) {
        let tools = self.directory.join("tools");
        fs::create_dir(&tools).unwrap();
        let script = if failure {
            "#!/bin/sh\nprintf '%s\\n' private-child-sentinel >&2\nexit 1\n".into()
        } else {
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" >> git-args\nprintf '100644 {} 0\\tsrc/main.rs\\0'\n",
                "0".repeat(40)
            )
        };
        let git = tools.join("git");
        fs::write(&git, script).unwrap();
        fs::set_permissions(git, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(self.directory.join("src")).unwrap();
        fs::write(self.directory.join("src/main.rs"), b"fn fixture() {}\n").unwrap();
        self.context
            .environment
            .insert("PATH".into(), tools.to_str().unwrap().into());
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.directory).unwrap();
    }
}

#[test]
fn framing_accepts_only_the_exact_scope_trust_and_lowercase_digest() {
    for name in super::super::inputs::SCOPES {
        let scope = Scope::parse(name).unwrap();
        for trusted in [true, false] {
            let trust = if trusted { "main" } else { "untrusted" };
            let key = format!("ci-verified-v1-{name}-{trust}-{}", "a".repeat(64));
            assert_eq!(
                record(scope, trusted, &key).unwrap(),
                format!("key={key}\n").as_bytes()
            );
            for invalid in [
                format!("{key}\nprivate=sentinel"),
                format!("{key}\r"),
                format!(" {key}"),
                format!("{key} "),
                key.replace("aaaa", "AAAA"),
                key[..key.len() - 1].into(),
                format!("{key}0"),
                key.replacen("ci-verified-v1-", "ci-verified-v2-", 1),
                format!("ci-verified-v1-{name}-{trust}-{}", "$".repeat(64)),
            ] {
                let error = record(scope, trusted, &invalid).unwrap_err();
                assert_eq!(error.to_string(), "ci_cache_github_output_key");
            }
            assert!(record(scope, !trusted, &key).is_err());
            let foreign = if scope == Scope::Backend {
                Scope::RustTest
            } else {
                Scope::Backend
            };
            assert!(record(foreign, trusted, &key).is_err());
        }
    }
}

#[test]
fn append_preserves_existing_bytes_permissions_and_literal_path_without_creating_files() {
    let fixture = Fixture::new();
    let before_mode = fs::metadata(&fixture.output).unwrap().permissions().mode();
    let key = format!("ci-verified-v1-rust-lint-main-{}", "b".repeat(64));
    Target::open(&fixture.context)
        .unwrap()
        .append(Scope::RustLint, true, &key)
        .unwrap();
    assert_eq!(
        fs::read(&fixture.output).unwrap(),
        format!("prior=preserved\nkey={key}\n").as_bytes()
    );
    assert_eq!(
        fs::metadata(&fixture.output).unwrap().permissions().mode(),
        before_mode
    );
    assert!(!fixture.directory.join("unowned").exists());
    let before = fs::read(&fixture.output).unwrap();
    assert!(
        Target::open(&fixture.context)
            .unwrap()
            .append(Scope::RustLint, true, "private-sentinel\nother=bad")
            .is_err()
    );
    assert_eq!(fs::read(&fixture.output).unwrap(), before);
}

#[test]
fn missing_relative_unopened_symlinked_and_special_targets_refuse_without_mutation() {
    let mut fixture = Fixture::new();
    let alias = fixture.directory.join("alias");
    symlink(&fixture.output, &alias).unwrap();
    let pipe = fixture.directory.join("owned-pipe");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&pipe)
            .status()
            .unwrap()
            .success()
    );
    let reader = rustix::fs::open(&pipe, OFlags::RDWR | OFlags::NONBLOCK, Mode::empty()).unwrap();
    for (value, error) in [
        (String::new(), "ci_cache_github_output_missing"),
        ("relative-output".into(), "ci_cache_github_output_path"),
        (
            fixture.directory.join("missing").to_str().unwrap().into(),
            "ci_cache_github_output_open",
        ),
        (
            alias.to_str().unwrap().into(),
            "ci_cache_github_output_open",
        ),
        (
            fixture.directory.to_str().unwrap().into(),
            "ci_cache_github_output_open",
        ),
        (
            pipe.to_str().unwrap().into(),
            "ci_cache_github_output_regular",
        ),
    ] {
        fixture
            .context
            .environment
            .insert("GITHUB_OUTPUT".into(), value);
        assert_eq!(
            Target::open(&fixture.context).err().unwrap().to_string(),
            error
        );
    }
    fixture.context.environment.remove("GITHUB_OUTPUT");
    assert_eq!(
        Target::open(&fixture.context).err().unwrap().to_string(),
        "ci_cache_github_output_missing"
    );
    assert!(!fixture.directory.join("missing").exists());
    assert_eq!(fs::read(&fixture.output).unwrap(), b"prior=preserved\n");
    drop(reader);
}

#[test]
fn write_errors_and_short_writes_are_fixed_failures_without_private_error_details() {
    struct Broken {
        attempts: usize,
        partial: bool,
    }
    impl Write for Broken {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.attempts += 1;
            if self.partial && self.attempts == 1 {
                return Ok(bytes.len().min(3));
            }
            Err(std::io::Error::other("private-write-sentinel"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    for partial in [true, false] {
        let mut writer = Broken {
            attempts: 0,
            partial,
        };
        let error = write(&mut writer, b"key=fixed\n").unwrap_err();
        assert_eq!(error.to_string(), "ci_cache_github_output_write");
        assert!(!format!("{error:#}").contains("private-write-sentinel"));
        assert_eq!(writer.attempts, if partial { 2 } else { 1 });
    }
}

#[test]
fn fake_git_fingerprint_uses_the_same_key_and_never_publishes_on_inventory_failure() {
    for failed in [false, true] {
        let mut fixture = Fixture::new();
        fixture.fake_git(failed);
        if failed {
            let error =
                super::super::print_key(&fixture.context, Scope::RustTest, true).unwrap_err();
            assert!(!format!("{error:#}").contains("private-child-sentinel"));
            assert_eq!(fs::read(&fixture.output).unwrap(), b"prior=preserved\n");
        } else {
            let key = super::super::fingerprint::key(&fixture.context, Scope::RustTest).unwrap();
            super::super::print_key(&fixture.context, Scope::RustTest, true).unwrap();
            assert_eq!(
                fs::read(&fixture.output).unwrap(),
                format!("prior=preserved\nkey={key}\n").as_bytes()
            );
            assert_eq!(
                fs::read_to_string(fixture.directory.join("git-args")).unwrap(),
                "ls-files\n--stage\n-z\nls-files\n--stage\n-z\n"
            );
        }
    }
    let mut fixture = Fixture::new();
    fixture.fake_git(false);
    fixture.context.environment.remove("GITHUB_OUTPUT");
    assert!(super::super::print_key(&fixture.context, Scope::RustTest, true).is_err());
    assert!(!fixture.directory.join("git-args").exists());
}

#[test]
fn github_output_flag_only_applies_once_to_the_key_operation() {
    let root = Path::new("/owned-checkout");
    let parse = |arguments: &[&str]| {
        super::super::options(
            root,
            &arguments
                .iter()
                .map(|argument| (*argument).to_owned())
                .collect::<Vec<_>>(),
        )
    };
    assert!(
        parse(&["key", "--scope", "backend", "--github-output"])
            .unwrap()
            .github_output
    );
    assert!(!parse(&["key", "--scope", "backend"]).unwrap().github_output);
    for arguments in [
        vec!["save", "--scope", "backend", "--github-output"],
        vec!["verify", "--scope", "backend", "--github-output"],
        vec![
            "key",
            "--scope",
            "backend",
            "--github-output",
            "--github-output",
        ],
        vec![
            "key",
            "--scope",
            "backend",
            "--github-output",
            "--key",
            "private",
        ],
        vec![
            "key",
            "--scope",
            "backend",
            "--github-output",
            "--directory",
            "/private",
        ],
        vec!["key", "--scope", "unknown", "--github-output"],
    ] {
        assert!(parse(&arguments).is_none(), "{arguments:?}");
    }
}

#[test]
fn workflow_fingerprint_steps_keep_their_scopes_order_guards_and_receipt_consumers() {
    use serde::Deserialize;
    use serde_json::Value;
    #[derive(Deserialize)]
    struct Workflow {
        jobs: BTreeMap<String, Value>,
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let workflow: Workflow =
        serde_saphyr::from_slice(&fs::read(root.join(".github/workflows/ci.yml")).unwrap())
            .unwrap();
    for (job, scope, count) in [
        ("automation", "automation", 16),
        ("rust-lint", "rust-lint", 13),
        ("rust-test", "rust-test", 19),
        ("browser", "backend", 43),
    ] {
        let steps = workflow.jobs[job]["steps"].as_array().unwrap();
        assert_eq!(
            steps.len(),
            count,
            "The cache output change adds or removes no {job} step"
        );
        let matching: Vec<_> = steps
            .iter()
            .enumerate()
            .filter(|(_, step)| step["id"] == "verified-inputs")
            .collect();
        assert_eq!(matching.len(), 1);
        let (index, key) = matching[0];
        assert_eq!(key["env"], serde_json::json!({"CI_CACHE_SCOPE":scope}));
        assert!(key.get("if").is_none());
        assert_eq!(
            key["run"].as_str().unwrap().trim(),
            concat!(
                "cargo run --locked -q -p xtask -- ci key --scope ",
                "\"$CI_CACHE_SCOPE\" --github-output"
            )
        );
        let restore = &steps[index + 1];
        assert_eq!(restore["id"], "verified");
        assert_eq!(
            restore["uses"],
            concat!(
                "actions/cache/restore@",
                "55cc8345863c7cc4c66a329aec7e433d2d1c52a9"
            )
        );
        assert_eq!(
            restore["with"]["key"],
            "${{ steps.verified-inputs.outputs.key }}"
        );
        let verify = &steps[index + 2];
        assert_eq!(verify["if"], "steps.verified.outputs.cache-hit == 'true'");
        assert_eq!(verify["env"]["CI_CACHE_SCOPE"], scope);
        assert_eq!(
            verify["env"]["CI_CACHE_KEY"],
            "${{ steps.verified-inputs.outputs.key }}"
        );
        assert!(verify["run"].as_str().unwrap().starts_with(
            "cargo run --locked -q -p xtask -- ci verify --scope \"$CI_CACHE_SCOPE\""
        ));
        let saves: Vec<_> = steps
            .iter()
            .enumerate()
            .filter(|(_, step)| {
                step["run"].as_str().is_some_and(|run| {
                    run.starts_with("cargo run --locked -q -p xtask -- ci save --scope")
                })
            })
            .collect();
        assert_eq!(saves.len(), 1);
        let (saved_at, receipt) = saves[0];
        assert!(saved_at > index + 2);
        assert_eq!(receipt["if"], "steps.verified.outputs.cache-hit != 'true'");
        assert_eq!(receipt["env"]["CI_CACHE_SCOPE"], scope);
        assert_eq!(
            receipt["env"]["CI_CACHE_KEY"],
            "${{ steps.verified-inputs.outputs.key }}"
        );
        let save = &steps[saved_at + 1];
        assert_eq!(save["if"], "steps.verified.outputs.cache-hit != 'true'");
        assert_eq!(
            save["uses"],
            concat!(
                "actions/cache/save@",
                "55cc8345863c7cc4c66a329aec7e433d2d1c52a9"
            )
        );
        assert_eq!(
            save["with"]["key"],
            "${{ steps.verified.outputs.cache-primary-key }}"
        );
    }
}
