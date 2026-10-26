use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    context: Context,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "simplestchat-ci space-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let mut context = Context::current(&root).unwrap();
        for (name, value) in [
            ("ACT", "true".to_owned()),
            (
                "LOCAL_CI_RUNNER_IMAGE",
                format!("docker.io/owned/runner@sha256:{}", "a".repeat(64)),
            ),
            ("GITHUB_REF", "refs/heads/main".to_owned()),
            ("GITHUB_EVENT_NAME", "push".to_owned()),
            ("GITHUB_SHA", "1".repeat(40)),
        ] {
            context.environment.insert(name.to_owned(), value);
        }
        context.environment.remove("CARGO_TARGET_DIR");
        let fixture = Self { context };
        fixture.git(&["init", "--quiet"]);
        for name in [
            "src/main.rs",
            "docs/example.md",
            "vendor/worker.cpp",
            "xtask/src/check.rs",
            "build/native_security.py",
            "security/exceptions.json",
            "security/native/toolchain.json",
            ".github/workflows/security.yml",
            "web/src/media.ts",
            "web/tests/media.test.mjs",
            "web/tests/layer-cap-cases.json",
            "web/e2e/package-lock.json",
        ] {
            fixture.write(name, b"original\n");
        }
        let workflow = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join(".github/workflows/ci.yml");
        fixture.write(".github/workflows/ci.yml", &fs::read(workflow).unwrap());
        fixture.git(&["add", "."]);
        fixture
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        let path = self.context.root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn git(&self, arguments: &[&str]) {
        let status = Command::new("git")
            .args(arguments)
            .current_dir(&self.context.root)
            .env_clear()
            .envs(&self.context.environment)
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn oracle(&self, operation: &str, scope: Scope, key: &str, directory: &Path) -> Vec<u8> {
        let build = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("build");
        let script = concat!(
            "import sys; from pathlib import Path; sys.path.insert(0,sys.argv[1]); ",
            "import ci_verified as c; import platform; platform.system=lambda:sys.argv[7]; ",
            "platform.machine=lambda:sys.argv[8]; root=Path(sys.argv[2]); scope=sys.argv[3]; op=sys.argv[4]; ",
            "key=sys.argv[5]; directory=Path(sys.argv[6]); ",
            "result = c.cache_key(root,scope) if op=='key' else ",
            "c.save(root,scope,key,directory) if op=='save' else c.verify(root,scope,key,directory); ",
            "print(result) if result is not None else None"
        );
        let result = Command::new("python3")
            .arg("-c")
            .arg(script)
            .arg(build)
            .arg(&self.context.root)
            .arg(scope.name())
            .arg(operation)
            .arg(key)
            .arg(directory)
            .arg(&self.context.system)
            .arg(&self.context.architecture)
            .current_dir(&self.context.root)
            .env_clear()
            .envs(&self.context.environment)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        result.stdout
    }

    fn backend(&self) {
        for name in inputs::BINARIES {
            let path = self.context.root.join("target/debug").join(name);
            self.write(&format!("target/debug/{name}"), b"owned executable fixture");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            let target = format!("{}:", path.display()).replace(' ', "\\ ");
            let source = self
                .context
                .root
                .join("src/main.rs")
                .display()
                .to_string()
                .replace(' ', "\\ ");
            let rule = format!("{target} \\\n  {source}\n\n");
            self.write(&format!("target/debug/{name}.d"), rule.as_bytes());
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.context.root).unwrap();
    }
}

#[test]
fn every_scope_matches_the_python_oracle_and_worktree_changes() {
    let fixture = Fixture::new();
    for changed in [false, true] {
        if changed {
            fixture.write("src/main.rs", b"changed worktree, unchanged index\n");
        }
        for name in inputs::SCOPES {
            let scope = Scope::parse(name).unwrap();
            let expected = fixture.oracle("key", scope, "", Path::new("."));
            assert_eq!(
                format!("{}\n", fingerprint::key(&fixture.context, scope).unwrap()).as_bytes(),
                expected,
                "scope {name}, changed {changed}"
            );
        }
    }
}

#[test]
fn receipt_bytes_and_original_revision_interoperate_with_python() {
    let mut fixture = Fixture::new();
    let scope = Scope::RustTest;
    let key = fingerprint::key(&fixture.context, scope).unwrap();
    let rust = fixture.context.root.join("rust-receipt");
    let python = fixture.context.root.join("python-receipt");
    receipts::save(&fixture.context, scope, &key, &rust).unwrap();
    fixture.oracle("save", scope, &key, &python);
    assert_eq!(
        fs::read(rust.join("receipt.json")).unwrap(),
        fs::read(python.join("receipt.json")).unwrap()
    );
    fixture
        .context
        .environment
        .insert("GITHUB_SHA".to_owned(), "2".repeat(40));
    assert_eq!(
        receipts::verify(&fixture.context, scope, &key, &python).unwrap(),
        "1".repeat(40)
    );
    assert_eq!(
        fixture.oracle("verify", scope, &key, &rust),
        format!("{}\n", "1".repeat(40)).as_bytes()
    );
    assert_eq!(
        fs::metadata(rust.join("receipt.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn python_json_framing_escapes_non_ascii_without_compacting_objects() {
    assert_eq!(io::json_string("aé😀\n"), "\"a\\u00e9\\ud83d\\ude00\\n\"");
    assert_eq!(
        io::json_object(&BTreeMap::from([
            ("b".to_owned(), "2".to_owned()),
            ("a".to_owned(), "1".to_owned())
        ])),
        "{\"a\": \"1\", \"b\": \"2\"}"
    );
}

#[test]
fn backend_receipts_validate_both_artifacts_before_any_installation() {
    let fixture = Fixture::new();
    fixture.backend();
    let scope = Scope::Backend;
    let key = fingerprint::key(&fixture.context, scope).unwrap();
    let directory = fixture.context.root.join("cache");
    receipts::save(&fixture.context, scope, &key, &directory).unwrap();
    for name in inputs::BINARIES {
        fs::remove_file(fixture.context.root.join("target/debug").join(name)).unwrap();
    }
    fs::write(directory.join("load_test"), b"changed executable").unwrap();
    assert!(
        receipts::verify(&fixture.context, scope, &key, &directory)
            .unwrap_err()
            .to_string()
            .contains("ci_cache_binary_changed")
    );
    for name in inputs::BINARIES {
        assert!(
            !fixture
                .context
                .root
                .join("target/debug")
                .join(name)
                .exists()
        );
    }
}

fn assert_key_parity(fixture: &Fixture, scope: Scope) -> String {
    let actual = fingerprint::key(&fixture.context, scope).unwrap();
    assert_eq!(
        format!("{actual}\n").as_bytes(),
        fixture.oracle("key", scope, "", Path::new("."))
    );
    actual
}

#[test]
fn input_closures_and_index_modes_preserve_oracle_decisions() {
    let fixture = Fixture::new();
    let scopes = inputs::SCOPES
        .iter()
        .map(|name| Scope::parse(name).unwrap())
        .collect::<Vec<_>>();
    let before = scopes
        .iter()
        .map(|scope| assert_key_parity(&fixture, *scope))
        .collect::<Vec<_>>();
    for name in [
        "web/src/media.ts",
        "web/tests/media.test.mjs",
        "docs/example.md",
    ] {
        fixture.write(name, b"independent frontend or guide\n");
    }
    for (scope, original) in scopes.iter().zip(&before) {
        let current = assert_key_parity(&fixture, *scope);
        assert_eq!(
            current != *original,
            *scope == Scope::Automation,
            "{scope:?}"
        );
    }
    fixture.write(
        "web/tests/layer-cap-cases.json",
        b"shared Rust/browser data\n",
    );
    for scope in [
        Scope::RustLint,
        Scope::RustTest,
        Scope::Backend,
        Scope::CodeqlRust,
    ] {
        assert_ne!(
            assert_key_parity(&fixture, scope),
            before[scopes.iter().position(|value| *value == scope).unwrap()]
        );
    }
    let original = assert_key_parity(&fixture, Scope::NativeAsan);
    fixture.git(&["update-index", "--chmod=+x", "build/native_security.py"]);
    assert_ne!(assert_key_parity(&fixture, Scope::NativeAsan), original);
    fs::remove_file(fixture.context.root.join("vendor/worker.cpp")).unwrap();
    assert!(
        fingerprint::key(&fixture.context, Scope::NativeAsan)
            .unwrap_err()
            .to_string()
            .contains("ci_cache_source_missing")
    );
}

#[test]
fn runner_trust_isa_and_backend_overrides_retain_exact_keys() {
    let mut fixture = Fixture::new();
    let original = assert_key_parity(&fixture, Scope::RustTest);
    for (variable, value) in [
        ("GITHUB_EVENT_NAME", "pull_request"),
        ("GITHUB_REF", "refs/heads/feature"),
        (
            "LOCAL_CI_RUNNER_IMAGE",
            &format!("docker.io/owned/runner@sha256:{}", "b".repeat(64)),
        ),
    ] {
        let saved = fixture
            .context
            .environment
            .insert(variable.to_owned(), value.to_owned());
        assert_ne!(assert_key_parity(&fixture, Scope::RustTest), original);
        if let Some(saved) = saved {
            fixture
                .context
                .environment
                .insert(variable.to_owned(), saved);
        } else {
            fixture.context.environment.remove(variable);
        }
    }
    fixture.context.architecture = "other-architecture".to_owned();
    assert_ne!(assert_key_parity(&fixture, Scope::RustTest), original);
    fixture
        .context
        .environment
        .insert("ACT".to_owned(), String::new());
    fixture
        .context
        .environment
        .insert("ImageOS".to_owned(), "ubuntu24".to_owned());
    fixture
        .context
        .environment
        .insert("ImageVersion".to_owned(), "20261003.1".to_owned());
    assert_ne!(assert_key_parity(&fixture, Scope::RustTest), original);
    let baseline = assert_key_parity(&fixture, Scope::Backend);
    for variable in [
        "RUSTFLAGS",
        "RUSTC",
        "CC",
        "PIP_INDEX_URL",
        "CARGO_PROFILE_DEV_OPT_LEVEL",
        "CARGO_BUILD_RUSTFLAGS",
    ] {
        let saved = fixture
            .context
            .environment
            .insert(variable.to_owned(), "changed".to_owned());
        assert_ne!(
            assert_key_parity(&fixture, Scope::Backend),
            baseline,
            "{variable}"
        );
        if let Some(saved) = saved {
            fixture
                .context
                .environment
                .insert(variable.to_owned(), saved);
        } else {
            fixture.context.environment.remove(variable);
        }
    }
    fixture.context.environment.insert(
        "CARGO_TARGET_DIR".to_owned(),
        fixture.context.root.join("other").display().to_string(),
    );
    assert!(
        fingerprint::key(&fixture.context, Scope::Backend)
            .unwrap_err()
            .to_string()
            .contains("ci_backend_target_directory")
    );
}

#[test]
fn workflow_projection_preserves_build_and_rejects_late_global_or_alias_inputs() {
    let fixture = Fixture::new();
    let path = fixture.context.root.join(".github/workflows/ci.yml");
    let source = fs::read_to_string(&path).unwrap();
    let original = assert_key_parity(&fixture, Scope::Backend);
    for (before, after, changed) in [
        (
            "name: Build production container",
            "name: Build the production image",
            false,
        ),
        ("test:stress", "test:stress-extra", false),
        (
            "cargo build --locked --workspace --bins",
            "cargo build --locked --bins",
            true,
        ),
        ("OPENSSL_STATIC: \"1\"", "OPENSSL_STATIC: \"0\"", true),
        ("cache-key: rust-build", "cache-key: different-build", true),
    ] {
        assert!(source.contains(before));
        fs::write(&path, source.replace(before, after)).unwrap();
        assert_eq!(
            assert_key_parity(&fixture, Scope::Backend) != original,
            changed
        );
    }
    for text in [
        format!("{source}\nenv:\n  RUSTFLAGS: changed\n"),
        source.replace("  browser:\n", "  browser: &shared\n"),
        source.replace("    env:\n", "    env: *shared\n"),
        source.replace("Save the exact backend success", "renamed boundary"),
        source.replace("  web:\n", "    container: changed\n\n  web:\n"),
    ] {
        fs::write(&path, text).unwrap();
        assert!(
            fingerprint::key(&fixture.context, Scope::Backend)
                .unwrap_err()
                .to_string()
                .contains("ci_backend_workflow")
        );
    }
}

#[test]
fn backend_receipt_bytes_and_depinfo_interoperate_with_python() {
    let fixture = Fixture::new();
    fixture.backend();
    let key = assert_key_parity(&fixture, Scope::Backend);
    let rust = fixture.context.root.join("rust-receipt");
    let python = fixture.context.root.join("python-receipt");
    receipts::save(&fixture.context, Scope::Backend, &key, &rust).unwrap();
    fixture.oracle("save", Scope::Backend, &key, &python);
    assert_eq!(
        fs::read(rust.join("receipt.json")).unwrap(),
        fs::read(python.join("receipt.json")).unwrap()
    );
    for name in inputs::BINARIES {
        fs::remove_file(fixture.context.root.join("target/debug").join(name)).unwrap();
    }
    assert_eq!(
        receipts::verify(&fixture.context, Scope::Backend, &key, &python).unwrap(),
        "1".repeat(40)
    );
    for name in inputs::BINARIES {
        let output = fixture.context.root.join("target/debug").join(name);
        assert_eq!(fs::read(&output).unwrap(), b"owned executable fixture");
        assert_eq!(
            fs::metadata(output).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    assert_eq!(
        fixture.oracle("verify", Scope::Backend, &key, &rust),
        format!("{}\n", "1".repeat(40)).as_bytes()
    );
}

#[test]
fn codeql_cannot_reuse_a_stamp_and_changed_inputs_cannot_publish_or_verify() {
    let fixture = Fixture::new();
    let directory = fixture.context.root.join("cache");
    for scope in [Scope::CodeqlNative, Scope::CodeqlRust] {
        let key = fingerprint::key(&fixture.context, scope).unwrap();
        for result in [
            receipts::save(&fixture.context, scope, &key, &directory),
            receipts::verify(&fixture.context, scope, &key, &directory).map(|_| ()),
        ] {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("ci_cache_codeql_requires_database")
            );
        }
    }
    let key = fingerprint::key(&fixture.context, Scope::RustTest).unwrap();
    receipts::save(&fixture.context, Scope::RustTest, &key, &directory).unwrap();
    fixture.write("src/main.rs", b"changed input\n");
    assert!(
        receipts::verify(&fixture.context, Scope::RustTest, &key, &directory)
            .unwrap_err()
            .to_string()
            .contains("ci_cache_inputs_changed")
    );
    let unpublished = fixture.context.root.join("unpublished");
    assert!(receipts::save(&fixture.context, Scope::RustTest, &key, &unpublished).is_err());
    assert!(!unpublished.exists());
}

#[test]
fn git_boundary_uses_only_the_explicit_read_only_inventories() {
    let mut fixture = Fixture::new();
    fixture.write(
        "tools/git",
        concat!(
            "#!/bin/sh\n",
            "printf '%s\\n' \"$*\" >> \"$CI_TEST_GIT_LOG\"\n",
            "case \"$*\" in\n",
            "'ls-files --stage -z') printf '100644 aaaaaaaa 0\\tsrc/main.rs\\0';;\n",
            "'ls-files -z') printf 'src/main.rs\\0';;\n",
            "*) exit 97;;\nesac\n"
        )
        .as_bytes(),
    );
    fs::set_permissions(
        fixture.context.root.join("tools/git"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let log = fixture.context.root.join("git.log");
    fixture.context.environment.insert(
        "PATH".to_owned(),
        fixture.context.root.join("tools").display().to_string(),
    );
    fixture
        .context
        .environment
        .insert("CI_TEST_GIT_LOG".to_owned(), log.display().to_string());
    assert!(fingerprint::key(&fixture.context, Scope::RustTest).is_ok());
    assert_eq!(fixture.context.tracked(false).unwrap(), b"src/main.rs\0");
    assert_eq!(
        fs::read_to_string(log).unwrap(),
        "ls-files --stage -z\nls-files -z\n"
    );
}

#[test]
fn dependency_refusals_precede_backend_cache_publication() {
    let fixture = Fixture::new();
    for dependency in ["docs/example.md", "src/untracked.txt", "target/unbound.rs"] {
        fixture.backend();
        fixture.write(dependency, b"unbound input\n");
        for name in inputs::BINARIES {
            let path = fixture.context.root.join("target/debug").join(name);
            let target = format!("{}:", path.display()).replace(' ', "\\ ");
            let source = fixture
                .context
                .root
                .join("src/main.rs")
                .display()
                .to_string()
                .replace(' ', "\\ ");
            let unbound = fixture
                .context
                .root
                .join(dependency)
                .display()
                .to_string()
                .replace(' ', "\\ ");
            fixture.write(
                &format!("target/debug/{name}.d"),
                format!("{target} {source} {unbound}\n").as_bytes(),
            );
        }
        let key = fingerprint::key(&fixture.context, Scope::Backend).unwrap();
        let directory = fixture.context.root.join("unpublished");
        assert!(
            receipts::save(&fixture.context, Scope::Backend, &key, &directory)
                .unwrap_err()
                .to_string()
                .contains("ci_backend_dep_unkeyed")
        );
        assert!(!directory.exists());
    }
}

#[test]
fn generated_dependencies_are_bound_and_restored_depinfo_is_authenticated() {
    let mut fixture = Fixture::new();
    fixture.backend();
    let paths = [
        "target/debug/build/mediasoup-sys-1234567890abcdef/out/fbs.rs",
        "target/debug/build/aws-lc-sys-1234567890abcdef/out/bindings.rs",
        "target/openssl/lib/libssl.a",
        "target/aws-lc/lib/libcrypto-awslc.a",
    ];
    for path in paths {
        fixture.write(path, b"generated compiler input\n");
    }
    fixture.context.environment.insert(
        "OPENSSL_DIR".to_owned(),
        fixture
            .context
            .root
            .join("target/openssl")
            .display()
            .to_string(),
    );
    fixture.context.environment.insert(
        "AWS_LC_SYS_SYSTEM_DIR".to_owned(),
        fixture
            .context
            .root
            .join("target/aws-lc")
            .display()
            .to_string(),
    );
    for name in inputs::BINARIES {
        let target = format!(
            "{}:",
            fixture
                .context
                .root
                .join("target/debug")
                .join(name)
                .display()
        );
        let mut words = vec![
            target,
            fixture
                .context
                .root
                .join("src/main.rs")
                .display()
                .to_string(),
            fixture.context.root.join("vendor").display().to_string(),
        ];
        words.extend(
            paths
                .iter()
                .map(|path| fixture.context.root.join(path).display().to_string()),
        );
        fixture.write(
            &format!("target/debug/{name}.d"),
            words
                .iter()
                .map(|word| word.replace(' ', "\\ "))
                .collect::<Vec<_>>()
                .join(" \\\n  ")
                .as_bytes(),
        );
    }
    let key = assert_key_parity(&fixture, Scope::Backend);
    let directory = fixture.context.root.join("cache");
    receipts::save(&fixture.context, Scope::Backend, &key, &directory).unwrap();
    for path in paths {
        fs::remove_file(fixture.context.root.join(path)).unwrap();
    }
    for name in inputs::BINARIES {
        fs::remove_file(fixture.context.root.join("target/debug").join(name)).unwrap();
    }
    receipts::verify(&fixture.context, Scope::Backend, &key, &directory).unwrap();
    for name in inputs::BINARIES {
        fs::remove_file(fixture.context.root.join("target/debug").join(name)).unwrap();
    }
    fs::write(directory.join("load_test.d"), b"changed dependency rule\n").unwrap();
    assert!(
        receipts::verify(&fixture.context, Scope::Backend, &key, &directory)
            .unwrap_err()
            .to_string()
            .contains("ci_cache_binary_changed")
    );
    for name in inputs::BINARIES {
        assert!(
            !fixture
                .context
                .root
                .join("target/debug")
                .join(name)
                .exists()
        );
    }
}

#[test]
fn symlinked_sources_or_restored_executables_never_become_proof() {
    let fixture = Fixture::new();
    let source = fixture.context.root.join("src/main.rs");
    fs::remove_file(&source).unwrap();
    std::os::unix::fs::symlink(fixture.context.root.join("docs/example.md"), &source).unwrap();
    assert!(
        fingerprint::key(&fixture.context, Scope::RustTest)
            .unwrap_err()
            .to_string()
            .contains("ci_cache_source_missing")
    );
    fs::remove_file(&source).unwrap();
    fixture.write("src/main.rs", b"original\n");
    fixture.backend();
    let key = fingerprint::key(&fixture.context, Scope::Backend).unwrap();
    let directory = fixture.context.root.join("cache");
    receipts::save(&fixture.context, Scope::Backend, &key, &directory).unwrap();
    for name in inputs::BINARIES {
        fs::remove_file(fixture.context.root.join("target/debug").join(name)).unwrap();
    }
    fs::remove_file(directory.join("load_test")).unwrap();
    std::os::unix::fs::symlink(directory.join("simplestChat"), directory.join("load_test"))
        .unwrap();
    assert!(receipts::verify(&fixture.context, Scope::Backend, &key, &directory).is_err());
    for name in inputs::BINARIES {
        assert!(
            !fixture
                .context
                .root
                .join("target/debug")
                .join(name)
                .exists()
        );
    }
}

#[test]
fn every_scope_binds_the_compiled_fingerprint_implementation() {
    let fixture = Fixture::new();
    for path in [
        "xtask/src/ci/mod.rs",
        "xtask/Cargo.toml",
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
    ] {
        fixture.write(path, b"original compiler input\n");
    }
    fixture.git(&["add", "."]);
    for name in inputs::SCOPES {
        let scope = Scope::parse(name).unwrap();
        let original = assert_key_parity(&fixture, scope);
        fixture.write("xtask/src/ci/mod.rs", b"new fingerprint implementation\n");
        assert_ne!(assert_key_parity(&fixture, scope), original, "{name}");
        fixture.write("xtask/src/ci/mod.rs", b"original compiler input\n");
    }
}

#[test]
fn regular_file_guards_accept_empty_sources_and_reject_symlinks_and_oversize_inputs() {
    let fixture = Fixture::new();
    let empty = fixture.context.root.join("empty");
    fs::write(&empty, b"").unwrap();
    assert_eq!(io::digest(&empty).unwrap(), io::sha256(b""));
    assert!(
        io::bounded(&empty, 10)
            .unwrap_err()
            .to_string()
            .contains("tool_file_size")
    );
    let large = fixture.context.root.join("too-large");
    fs::File::create(&large)
        .unwrap()
        .set_len(1024 * 1024 * 1024 + 1)
        .unwrap();
    assert!(
        io::digest(&large)
            .unwrap_err()
            .to_string()
            .contains("ci_cache_file_size")
    );
    let alias = fixture.context.root.join("alias");
    std::os::unix::fs::symlink(empty, &alias).unwrap();
    assert!(io::digest(&alias).is_err());
}

#[test]
fn oversized_git_diagnostics_are_rejected_before_waiting_for_the_child() {
    let mut fixture = Fixture::new();
    fixture.write(
        "tools/git",
        b"#!/bin/sh\nprintf '%065537d' 0 >&2\nexec /bin/sleep 5\n",
    );
    fs::set_permissions(
        fixture.context.root.join("tools/git"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    fixture.context.environment.insert(
        "PATH".to_owned(),
        fixture.context.root.join("tools").display().to_string(),
    );
    let start = std::time::Instant::now();
    assert!(
        fixture
            .context
            .tracked(true)
            .unwrap_err()
            .to_string()
            .contains("ci_cache_git")
    );
    assert!(start.elapsed() < Duration::from_secs(3));
}

#[test]
fn backend_default_and_unicode_environment_values_match_python_json() {
    let mut fixture = Fixture::new();
    fixture.context.environment.remove("CARGO_HOME");
    fixture.context.environment.remove("CARGO_INCREMENTAL");
    let original = assert_key_parity(&fixture, Scope::Backend);
    fixture.context.environment.insert(
        "CARGO_HOME".to_owned(),
        Path::new(fixture.context.env("HOME"))
            .join(".cargo")
            .display()
            .to_string(),
    );
    fixture
        .context
        .environment
        .insert("CARGO_INCREMENTAL".to_owned(), "0".to_owned());
    assert_eq!(assert_key_parity(&fixture, Scope::Backend), original);
    fixture
        .context
        .environment
        .insert("RUSTFLAGS".to_owned(), "é😀\nquoted=\"yes\"".to_owned());
    assert_ne!(assert_key_parity(&fixture, Scope::Backend), original);
    fixture
        .context
        .environment
        .insert("CARGO_HOME".to_owned(), String::new());
    fixture
        .context
        .environment
        .insert("HOME".to_owned(), String::new());
    assert_key_parity(&fixture, Scope::Backend);
}

#[test]
fn operations_workspace_inputs_change_rust_keys_with_exact_python_parity() {
    let fixture = Fixture::new();
    let inputs = [
        "Cargo.toml",
        "ops/Cargo.toml",
        "ops/src/lib.rs",
        "ops/src/release/manifest.rs",
        "ops/src/release/manifest_tests.rs",
        "ops/src/release/attestation/restore.rs",
        "ops/src/release/attestation/restore/oracle_tests.rs",
        "ops/tests/release_backup_cli.rs",
    ];
    for path in inputs {
        fixture.write(path, b"original operations input\n");
    }
    fixture.git(&["add", "."]);
    for scope in [
        Scope::Automation,
        Scope::RustLint,
        Scope::RustTest,
        Scope::Backend,
        Scope::CodeqlRust,
    ] {
        let original = assert_key_parity(&fixture, scope);
        for path in inputs {
            fixture.write(path, b"changed operations input\n");
            assert_ne!(
                assert_key_parity(&fixture, scope),
                original,
                "{} {path}",
                scope.name()
            );
            fixture.write(path, b"original operations input\n");
        }
        fixture.write("ops/src/new_policy.rs", b"new tracked policy\n");
        assert_eq!(assert_key_parity(&fixture, scope), original);
        fixture.git(&["add", "ops/src/new_policy.rs"]);
        assert_ne!(
            assert_key_parity(&fixture, scope),
            original,
            "{} new policy",
            scope.name()
        );
        fixture.git(&["rm", "--cached", "--quiet", "ops/src/new_policy.rs"]);
        fs::remove_file(fixture.context.root.join("ops/src/new_policy.rs")).unwrap();
        assert_eq!(assert_key_parity(&fixture, scope), original);
    }
}

#[test]
fn embedded_ops_catalog_changes_every_compiled_tool_key_with_exact_python_parity() {
    let fixture = Fixture::new();
    let catalog = "ops/ops-build-inputs.lock.json";
    let adjacent = "ops/unrelated-local-selection.json";
    fixture.write(catalog, b"original reviewed catalog\n");
    fixture.write(adjacent, b"original unrelated JSON\n");
    fixture.git(&["add", "."]);
    for name in inputs::SCOPES {
        let scope = Scope::parse(name).unwrap();
        let original = assert_key_parity(&fixture, scope);
        fixture.write(catalog, b"changed reviewed catalog\n");
        assert_ne!(assert_key_parity(&fixture, scope), original, "{name}");
        fixture.write(catalog, b"original reviewed catalog\n");
        fixture.write(adjacent, b"changed unrelated JSON\n");
        let conservative = matches!(scope, Scope::Automation | Scope::RustLint | Scope::RustTest);
        assert_eq!(
            assert_key_parity(&fixture, scope) != original,
            conservative,
            "{name}"
        );
        fixture.write(adjacent, b"original unrelated JSON\n");
        assert_eq!(assert_key_parity(&fixture, scope), original, "{name}");
    }
}

#[test]
fn restore_reference_and_registration_inputs_invalidate_existing_success_receipts() {
    let fixture = Fixture::new();
    let paths = [
        "ops/ansible/tests/release_restore_oracle.py",
        "ops/ansible/tests/test_release_restore_oracle.py",
        "xtask/src/ci/automation.rs",
        ".github/workflows/ci.yml",
    ];
    for path in paths {
        fixture.write(path, b"original restore comparison input\n");
    }
    fixture.git(&["add", "."]);
    for scope in [Scope::Automation, Scope::RustTest] {
        let key = assert_key_parity(&fixture, scope);
        let directory = fixture
            .context
            .root
            .join(format!("restore-{}", scope.name()));
        receipts::save(&fixture.context, scope, &key, &directory).unwrap();
        assert_eq!(
            receipts::verify(&fixture.context, scope, &key, &directory).unwrap(),
            "1".repeat(40)
        );
        for path in paths {
            fixture.write(path, b"changed restore comparison input\n");
            assert_ne!(
                assert_key_parity(&fixture, scope),
                key,
                "{} {path}",
                scope.name()
            );
            assert!(
                receipts::verify(&fixture.context, scope, &key, &directory).is_err(),
                "changed {path} cannot reuse the old {} receipt",
                scope.name()
            );
            fixture.write(path, b"original restore comparison input\n");
            assert_eq!(
                receipts::verify(&fixture.context, scope, &key, &directory).unwrap(),
                "1".repeat(40)
            );
        }
    }
}
#[test]
fn rust_maintenance_controller_and_actual_workflow_invalidate_existing_success_fingerprints() {
    for scope in [
        Scope::Automation,
        Scope::RustLint,
        Scope::RustTest,
        Scope::CodeqlRust,
    ] {
        for name in [
            "xtask/src/ci/ops_artifact/maintain_runtime/execute.rs",
            "xtask/src/ci/ops_artifact/maintain_runtime/vm.rs",
            "ops/src/owned_process.rs",
            "ops/src/release/attestation/control_signals.rs",
        ] {
            assert!(inputs::selected(scope, name), "{scope:?}: {name}");
        }
    }
    let workflow = ".github/workflows/maintenance-launcher-runtime.yml";
    for scope in [Scope::Automation, Scope::RustLint, Scope::RustTest] {
        assert!(inputs::selected(scope, workflow));
    }
    // Runtime launch policy is an Actions/automation input, not a source needed
    // to reconstruct the Rust analyzer's compiler extraction.
    assert!(!inputs::selected(Scope::CodeqlRust, workflow));
}
