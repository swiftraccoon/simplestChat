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
            .and_then(Path::parent)
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
            .and_then(Path::parent)
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
fn every_reference_scope_matches_the_python_oracle_and_worktree_changes() {
    let fixture = Fixture::new();
    for changed in [false, true] {
        if changed {
            fixture.write("src/main.rs", b"changed worktree, unchanged index\n");
        }
        for name in inputs::REFERENCE_SCOPES {
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
    let scopes = inputs::REFERENCE_SCOPES
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
            "cargo build --locked -p simplestchat -p simplestchat-load-test --bins",
            "cargo build --locked --bins",
            true,
        ),
        ("OPENSSL_STATIC: \"1\"", "OPENSSL_STATIC: \"0\"", true),
        (
            "cache-key: browser-backend-v1",
            "cache-key: different-build",
            true,
        ),
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
    // `web` is a directory with tracked children outside every keyed prefix.
    for dependency in [
        "docs/example.md",
        "src/untracked.txt",
        "target/unbound.rs",
        "web",
    ] {
        fixture.backend();
        if !fixture.context.root.join(dependency).is_dir() {
            fixture.write(dependency, b"unbound input\n");
        }
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
    // build.rs lists the embedded `migrations` directory; its tracked children bind it.
    fixture.write("migrations/001_create_users.sql", b"original migration\n");
    fixture.git(&["add", "migrations/001_create_users.sql"]);
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
            fixture
                .context
                .root
                .join("migrations")
                .display()
                .to_string(),
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
    let python = fixture.context.root.join("python-cache");
    fixture.oracle("save", Scope::Backend, &key, &python);
    assert_eq!(
        fs::read(directory.join("receipt.json")).unwrap(),
        fs::read(python.join("receipt.json")).unwrap()
    );
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
fn every_reference_scope_binds_the_compiled_fingerprint_implementation() {
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
    for name in inputs::REFERENCE_SCOPES {
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
        "ops/src/release/attestation/restore/tests.rs",
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
    for name in inputs::REFERENCE_SCOPES {
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
fn restore_test_and_registration_inputs_invalidate_existing_success_receipts() {
    let fixture = Fixture::new();
    let paths = [
        "ops/ansible/tests/test_restore_verify.py",
        "ops/ansible/tests/test_restore_database.py",
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
            "crates/release/src/ops_artifact/maintain_runtime/execute.rs",
            "crates/release/src/ops_artifact/maintain_runtime/vm.rs",
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

#[test]
fn shared_crates_are_keyed_wherever_xtask_is() {
    // The backend inputs and the compiled CI tool behind the native and CodeQL
    // scopes bind every file under `crates/`, as they bind `xtask/`; the
    // browser-backend role binds neither tree as a whole.
    let scopes = inputs::REFERENCE_SCOPES
        .iter()
        .map(|name| Scope::parse(name).unwrap())
        .chain([Scope::BrowserBackend]);
    for scope in scopes {
        for (shared, tool) in [
            ("crates/toolkit/Cargo.toml", "xtask/Cargo.toml"),
            ("crates/toolkit/Cargo.toml", "crates/ci/Cargo.toml"),
            ("crates/toolkit/Cargo.toml", "crates/release/Cargo.toml"),
            (
                "crates/toolkit/src/owned_process.rs",
                "crates/ci/src/lib.rs",
            ),
            (
                "crates/toolkit/src/owned_process.rs",
                "crates/ci/src/fingerprint.rs",
            ),
            ("crates/toolkit/README.md", "xtask/README.md"),
        ] {
            assert_eq!(
                inputs::selected(scope, shared),
                inputs::selected(scope, tool),
                "{scope:?}: {shared}"
            );
        }
        if scope != Scope::BrowserBackend {
            assert!(
                inputs::selected(scope, "crates/toolkit/README.md"),
                "{scope:?}"
            );
        }
    }
    // The key-computing modules live in the CI crate and compile against the
    // whole toolkit crate: main.rs routes its replay child and github_output.rs
    // takes its Environment. The frozen oracle omits `crates/`, so only these
    // Rust-only selections pin the moved implementation's own paths.
    for name in [
        "crates/ci/Cargo.toml",
        "crates/ci/src/main.rs",
        "crates/ci/src/lib.rs",
        "crates/ci/src/inputs.rs",
        "crates/ci/src/fingerprint.rs",
        "crates/ci/src/receipts.rs",
        "crates/ci/src/io.rs",
        "crates/ci/src/github_output.rs",
        "crates/toolkit/Cargo.toml",
        "crates/toolkit/src/lib.rs",
        "crates/toolkit/src/executable.rs",
        "crates/toolkit/src/owned_process.rs",
        "crates/toolkit/src/replay.rs",
        "crates/toolkit/src/tooling.rs",
    ] {
        assert!(inputs::selected(Scope::BrowserBackend, name), "{name}");
    }
    // Every source the crate holds, so a new toolkit module cannot miss the key.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap();
    let mut pending = vec![root.join("crates/toolkit/src")];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let name = path.strip_prefix(root).unwrap().to_str().unwrap();
            assert_eq!(
                inputs::selected(Scope::BrowserBackend, name),
                name.ends_with(".rs"),
                "{name}"
            );
        }
    }
    assert!(!inputs::selected(
        Scope::BrowserBackend,
        "crates/toolkit/README.md"
    ));
}

#[test]
fn only_dev_dependencies_enable_the_test_hooks() {
    // A normal, build or target-specific dependency, a feature forwarding the
    // hook, or a [workspace.dependencies] entry a member inherits would compile
    // the toolkit's signal injection or this crate's descriptor readback into
    // the dependent's ordinary builds.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap();
    let read = |directory: &Path| -> toml::Value {
        toml::from_str(&fs::read_to_string(directory.join("Cargo.toml")).unwrap()).unwrap()
    };
    let workspace = read(root);
    let members = workspace["workspace"]["members"]
        .as_array()
        .unwrap()
        .clone();
    let hooks_enabled = |entry: &toml::Value| {
        entry
            .get("features")
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
            .any(|feature| feature.as_str() == Some("test-hooks"))
    };
    for hooked in ["simplestchat-toolkit", "simplestchat-ci"] {
        let names = |name: &str, entry: &toml::Value| {
            name == hooked || entry.get("package").and_then(toml::Value::as_str) == Some(hooked)
        };
        // Every member inherits a workspace dependency's features as declared.
        let inherited = workspace
            .get("workspace")
            .and_then(|table| table.get("dependencies"))
            .and_then(toml::Value::as_table);
        for (name, entry) in inherited.into_iter().flatten() {
            assert!(
                !(names(name, entry) && hooks_enabled(entry)),
                "workspace.dependencies enables {hooked} test-hooks"
            );
        }
        let mut dependents = 0;
        let mut defining = 0;
        for directory in std::iter::once(root.to_path_buf()).chain(
            members
                .iter()
                .map(|member| root.join(member.as_str().unwrap())),
        ) {
            let manifest = read(&directory);
            let mut tables = vec![&manifest];
            if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
                tables.extend(targets.values());
            }
            let mut keys = Vec::new();
            for table in tables {
                for kind in ["dependencies", "build-dependencies", "dev-dependencies"] {
                    let entries = table.get(kind).and_then(toml::Value::as_table);
                    for (name, entry) in entries.into_iter().flatten() {
                        if !names(name, entry) {
                            continue;
                        }
                        keys.push(name.clone());
                        if kind == "dev-dependencies" {
                            continue;
                        }
                        dependents += 1;
                        assert!(
                            !hooks_enabled(entry),
                            "{}: {kind} enables {hooked} test-hooks",
                            directory.display()
                        );
                    }
                }
            }
            let features = manifest.get("features").and_then(toml::Value::as_table);
            for (feature, enables) in features.into_iter().flatten() {
                for enable in enables.as_array().into_iter().flatten() {
                    let enable = enable.as_str().unwrap();
                    assert!(
                        !keys.iter().any(|key| {
                            [format!("{key}/test-hooks"), format!("{key}?/test-hooks")]
                                .contains(&enable.to_owned())
                        }),
                        "{}: feature {feature} forwards {hooked} test-hooks",
                        directory.display()
                    );
                }
            }
            // The hooked crate itself may define the hook but must not turn it on
            // by default or from any other feature.
            let package = manifest
                .get("package")
                .and_then(|package| package.get("name"));
            if package.and_then(toml::Value::as_str) == Some(hooked) {
                defining += 1;
                for (feature, enables) in features.into_iter().flatten() {
                    let hooks = enables
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|enable| enable.as_str() == Some("test-hooks"));
                    assert!(
                        feature == "test-hooks" || !hooks,
                        "{hooked}'s {feature} feature enables test-hooks"
                    );
                }
            }
        }
        assert!(dependents > 0, "a workspace member depends on {hooked}");
        assert_eq!(defining, 1, "{hooked} is one workspace member");
    }
}

#[test]
fn release_proof_implementors_are_the_eight_proofs_and_two_test_doubles() {
    // The role vocabulary is closed but the public trait is open, so any crate
    // may implement it, and Rust cannot seal a trait that another crate must
    // implement. Pin every implementor under each crates/*/src and
    // crates/*/tests tree and under xtask/src and xtask/tests: its file, its
    // type and the role it reports.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap();
    let header = regex::Regex::new(
        r"impl(?:<[^>]*>)?\s+(?:[A-Za-z_][\w:]*::)?ReleaseProof\s+for\s+([A-Za-z_][\w:]*)",
    )
    .unwrap();
    let role = regex::Regex::new(
        r"fn\s+role\(&self\)\s*->\s*(?:[A-Za-z_][\w:]*::)?ReleaseRole\s*\{\s*([^{}]*?)\s*\}",
    )
    .unwrap();
    let mut found = std::collections::BTreeSet::new();
    let mut pending = vec![root.join("xtask/src"), root.join("xtask/tests")];
    for entry in fs::read_dir(root.join("crates")).unwrap() {
        let crate_root = entry.unwrap().path();
        for tree in ["src", "tests"] {
            let source = crate_root.join(tree);
            if source.is_dir() {
                pending.push(source);
            }
        }
    }
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if path.extension().is_none_or(|extension| extension != "rs") {
                continue;
            }
            let text = fs::read_to_string(&path).unwrap();
            let headers: Vec<_> = header.captures_iter(&text).collect();
            for (index, implementor) in headers.iter().enumerate() {
                let start = implementor.get(0).unwrap().end();
                let end = headers
                    .get(index + 1)
                    .map_or(text.len(), |next| next.get(0).unwrap().start());
                let reported = role
                    .captures(&text[start..end])
                    .map(|body| body[1].to_owned());
                let relative = path.strip_prefix(root).unwrap().to_str().unwrap();
                found.insert((relative.to_owned(), implementor[1].to_owned(), reported));
            }
        }
    }
    let expected = [
        // The benchmark adapter test's double reports each role in turn, and
        // so does the runner test's double that supplies no context.
        (
            "crates/ci/src/container_benchmark/tools.rs",
            "Carried",
            "self.0",
        ),
        ("crates/ci/src/process_tests.rs", "Uncontexted", "self.role"),
        ("ops_artifact/recipe.rs", "Tool", "ReleaseRole::Producer"),
        (
            "ops_artifact/source/git.rs",
            "Git",
            "ReleaseRole::SourceGit",
        ),
        (
            "ops_artifact/verify/tools.rs",
            "Invocation",
            "ReleaseRole::Retained",
        ),
        (
            "ops_artifact/acquire/tools.rs",
            "Invocation",
            "ReleaseRole::Acquire",
        ),
        (
            "ops_artifact/hosted/tools.rs",
            "Invocation",
            "ReleaseRole::Hosted",
        ),
        (
            "ops_artifact/runtime/tools.rs",
            "Invocation",
            "ReleaseRole::Runtime",
        ),
        (
            "ops_artifact/capacity_vps/tools.rs",
            "Invocation",
            "ReleaseRole::Vps",
        ),
        (
            "ops_artifact/deploy_frontend/tools.rs",
            "Invocation",
            "ReleaseRole::Frontend",
        ),
    ]
    .map(|(path, name, role)| {
        // The release plane's proofs live in the simplestchat-release crate.
        let path = if path.starts_with("crates/") {
            path.to_owned()
        } else {
            format!("crates/release/src/{path}")
        };
        (path, name.to_owned(), Some(role.to_owned()))
    });
    assert_eq!(found, std::collections::BTreeSet::from(expected));
}

#[path = "browser_backend_tests.rs"]
mod browser_backend;

#[test]
fn public_types_are_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<crate::command::Program>();
    assert_send_sync::<crate::command::ReleaseRole>();
    assert_send_sync::<crate::command::Command>();
    assert_send_sync::<crate::command::SeparateLogs>();
    assert_send_sync::<crate::bounded_log::Log>();
    assert_send_sync::<crate::bounded_log::Finished>();
    assert_send_sync::<crate::bounded_log::Seal>();
    assert_send_sync::<crate::bounded_log::Captures>();
    assert_send_sync::<crate::process::Native<'static>>();
}
