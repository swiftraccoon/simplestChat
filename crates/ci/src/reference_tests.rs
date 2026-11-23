//! The deleted Python tests of the CI cache reference, each held by Rust over
//! the same fixture and replayed against the reference's own answers, which
//! `recorded_fingerprint.json` keeps by test. Each test makes the same
//! assertions of Rust that its Python test made of the reference, and every
//! key, receipt and verification is held to the reference's.
//!
//! The reference's compiled-tool closure is `xtask/` and `.cargo/`; Rust's
//! adds `crates/`, where the compiled tool now lives. Where a fixture tracks
//! a file there, Rust's key is the reference's hash with that file's input
//! joined to it, and the tests say so.

use super::*;

/// Every scope the reference keyed, in its order.
fn scopes() -> Vec<Scope> {
    inputs::REFERENCE_SCOPES
        .iter()
        .map(|name| Scope::parse(name).unwrap())
        .collect()
}

/// The repository this crate is built from.
fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf()
}

/// An original revision as the receipts name it.
fn original_revision() -> Option<String> {
    Some("1".repeat(40))
}

/// The scopes whose reference keys omit a shared crate's Rust source: every
/// `.rs` file belongs to the backend and Rust CodeQL closures in both.
const SOURCE_OMITTED_BY: [&str; 4] = [
    "native-asan",
    "native-ubsan",
    "native-replay",
    "codeql-native",
];

/// The scopes whose reference keys omit a shared crate's notes.
const NOTES_OMITTED_BY: [&str; 6] = [
    "backend",
    "native-asan",
    "native-ubsan",
    "native-replay",
    "codeql-native",
    "codeql-rust",
];

/// Rust keys every tracked file under `crates/` in every scope. The
/// reference omitted one wherever its closure did not reach it otherwise,
/// and there alone the two keys differ, by exactly that file's input.
#[test]
fn the_reference_omits_the_shared_crates_that_rust_keys() {
    let fixture = Fixture::recorded("the_reference_omits_the_shared_crates_that_rust_keys");
    let shared: [(&str, &[u8], &[&str]); 2] = [
        (
            "crates/toolkit/src/lib.rs",
            b"original shared crate source\n",
            &SOURCE_OMITTED_BY,
        ),
        (
            "crates/toolkit/README.md",
            b"original shared crate notes\n",
            &NOTES_OMITTED_BY,
        ),
    ];
    for (path, data, _) in shared {
        fixture.write(path, data);
    }
    fixture.git(&["add", "."]);
    let mut originals = Vec::new();
    for scope in scopes() {
        let (rust, reference) = fixture.keys(scope).unwrap();
        let expected = shared
            .iter()
            .filter(|(_, _, omitting)| omitting.contains(&scope.name()))
            .map(|(path, _, _)| *path)
            .collect::<std::collections::BTreeSet<_>>();
        let omitted = fixture.omitted();
        assert_eq!(
            omitted
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            expected
        );
        assert!(omitted.is_sorted(), "{scope:?}: in Git's order");
        assert_eq!(rust != reference, !expected.is_empty(), "{scope:?}");
        originals.push((rust, reference));
    }
    for (path, data, omitting) in shared {
        fixture.write(path, b"changed shared crate input\n");
        for (scope, (rust, reference)) in scopes().into_iter().zip(&originals) {
            let (key, own) = fixture.keys(scope).unwrap();
            assert_ne!(key, *rust, "{scope:?} {path}");
            let held = !omitting.contains(&scope.name());
            assert_eq!(own != *reference, held, "{scope:?} {path}");
        }
        fixture.write(path, data);
    }
    assert_eq!(fixture.replayed(), [27, 18, 0]);
}

/// From `test_frontend_helper_tests_and_guides_reuse_existing_rust_receipts`:
/// independent helper tests, guides and frontend files, changed, made
/// executable, added or removed, leave every Rust key and receipt alone.
#[test]
fn frontend_helper_tests_and_guides_reuse_existing_rust_receipts() {
    let fixture =
        Fixture::recorded("frontend_helper_tests_and_guides_reuse_existing_rust_receipts");
    let independent = [
        "build/tests/community-runner.test.mjs",
        "docs/performance.md",
        "web/e2e/community.cjs",
    ];
    for name in independent {
        fixture.write(name, b"original\n");
    }
    fixture.git(&["add", "."]);
    let originals = scopes()
        .into_iter()
        .map(|scope| (scope, fixture.key(scope)))
        .collect::<Vec<_>>();
    let cache = |scope: Scope| fixture.context.root.join("cache").join(scope.name());
    let receipted = [&originals[1], &originals[2]];
    for (scope, key) in receipted {
        assert!(fixture.save(*scope, key, &cache(*scope)));
    }
    let reused = || {
        for (scope, original) in &originals {
            let key = fixture.key(*scope);
            assert_eq!(key != *original, *scope == Scope::Automation, "{scope:?}");
        }
        for (scope, key) in receipted {
            assert_eq!(
                fixture.verify(*scope, key, &cache(*scope)),
                original_revision()
            );
        }
    };
    for name in independent.into_iter().chain(["web/src/media.ts"]) {
        fixture.write(name, b"changed independent input\n");
    }
    reused();
    fixture.git(&["update-index", "--chmod=+x", independent[0]]);
    reused();
    let additions = [
        "build/tests/new-helper.test.mjs",
        "docs/guides/new-guide.md",
    ];
    for name in additions {
        fixture.write(name, b"new independent input\n");
    }
    fixture.git(&["add", additions[0], additions[1]]);
    reused();
    let removed = [independent[0], independent[1], additions[0], additions[1]];
    fixture.git(&[&["rm", "--quiet", "--force", "--"][..], &removed[..]].concat());
    reused();
    assert_eq!(fixture.replayed(), [55, 0, 0]);
}

/// The compiled, embedded and executed inputs of the Rust jobs, with names
/// like those of inputs they do not have.
const RUST_RECEIPT_INPUTS: [&str; 24] = [
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    ".cargo/config.toml",
    "src/main.rs",
    "vendor/worker.cpp",
    "vendor/README.md",
    "migrations/001_initial.sql",
    "security/authorization/operations.json",
    "web/tests/layer-cap-cases.json",
    "build/shutdown-smoke.mjs",
    "build/diagnostics-smoke.mjs",
    "xtask/src/test_env/server.rs",
    "build/tests/fixture-server.mjs",
    "build/tests/nested/helper.test.mjs",
    "build/tests/shared-fixture.json",
    "docs/shared-fixture.json",
    "docs/tool.rs",
    "README.md",
    "Caddyfile",
    "build/install-openssl.sh",
    "build/install-aws-lc.sh",
    ".github/workflows/ci.yml",
    ".github/actions/native-toolchain/action.yml",
];

/// From `test_rust_receipts_keep_sources_shared_data_and_runtime_inputs_bound`:
/// a similar name cannot hide a changed compiled or executed input. The two
/// Python build helpers among that test's names are the two native
/// installers the Rust jobs run here.
#[test]
fn rust_receipts_keep_sources_shared_data_and_runtime_inputs_bound() {
    let fixture =
        Fixture::recorded("rust_receipts_keep_sources_shared_data_and_runtime_inputs_bound");
    for name in RUST_RECEIPT_INPUTS {
        if !fixture.context.root.join(name).exists() {
            fixture.write(name, b"original\n");
        }
    }
    fixture.git(&["add", "."]);
    let receipted = [Scope::RustLint, Scope::RustTest].map(|scope| (scope, fixture.key(scope)));
    let cache = |scope: Scope| fixture.context.root.join("cache").join(scope.name());
    for (scope, key) in &receipted {
        assert!(fixture.save(*scope, key, &cache(*scope)));
    }
    for name in RUST_RECEIPT_INPUTS {
        let original = fs::read(fixture.context.root.join(name)).unwrap();
        fixture.write(name, b"changed actual input\n");
        for (scope, key) in &receipted {
            assert_ne!(fixture.key(*scope), *key, "{name} {scope:?}");
            assert_eq!(fixture.verify(*scope, key, &cache(*scope)), None, "{name}");
            assert_eq!(fixture.refused(), "ci_cache_inputs_changed");
        }
        fixture.write(name, &original);
    }
    assert_eq!(fixture.replayed(), [100, 0, 48]);
}

/// From `test_changed_source_policy_workflow_mode_and_deleted_inputs_invalidate`:
/// content and the tracked executable mode both belong to the native key,
/// and a deleted input refuses it.
#[test]
fn changed_source_policy_workflow_mode_and_deleted_inputs_invalidate() {
    let fixture =
        Fixture::recorded("changed_source_policy_workflow_mode_and_deleted_inputs_invalidate");
    let key = fixture.key(Scope::NativeAsan);
    for name in [
        "vendor/worker.cpp",
        "security/native/toolchain.json",
        ".github/workflows/security.yml",
    ] {
        fixture.write(name, b"changed\n");
        assert_ne!(fixture.key(Scope::NativeAsan), key, "{name}");
        fixture.write(name, b"original\n");
    }
    fixture.git(&[
        "update-index",
        "--chmod=+x",
        "build/native-security.Dockerfile",
    ]);
    assert_ne!(fixture.key(Scope::NativeAsan), key);
    fs::remove_file(fixture.context.root.join("vendor/worker.cpp")).unwrap();
    fixture.key_refused(Scope::NativeAsan);
    assert_eq!(fixture.refused(), "ci_cache_source_missing");
    assert_eq!(fixture.replayed(), [6, 0, 1]);
}

/// From `test_native_keys_cover_all_real_build_inputs_without_unrelated_rust_or_docs`:
/// application sources and guides leave the native keys alone and change
/// the Rust analysis's.
#[test]
fn native_keys_cover_all_real_build_inputs_without_unrelated_rust_or_docs() {
    let name = "native_keys_cover_all_real_build_inputs_without_unrelated_rust_or_docs";
    let fixture = Fixture::recorded(name);
    let originals =
        [Scope::NativeAsan, Scope::CodeqlNative].map(|scope| (scope, fixture.key(scope)));
    let rust = fixture.key(Scope::CodeqlRust);
    for name in ["src/main.rs", "docs/example.md"] {
        fixture.write(name, b"changed\n");
    }
    for (scope, key) in &originals {
        assert_eq!(fixture.key(*scope), *key, "{scope:?}");
    }
    assert_ne!(fixture.key(Scope::CodeqlRust), rust);
    assert_eq!(fixture.replayed(), [6, 0, 0]);
}

/// A file of a shared crate that the Python tests counted as unrelated to
/// the native work. Rust's native closures hold it as part of the compiled
/// tool's sources.
const SHARED_GATE: &str = "crates/security/src/image/gate.rs";

/// From `test_native_suites_bind_each_helper_and_ignore_unrelated_review_changes`:
/// each native helper changes the three native keys, and other analyzers'
/// reviews and workflows change none of them. The reference ignored the
/// image gate's source too; Rust's keys change with it and from then on.
#[test]
fn native_suites_bind_each_helper_and_ignore_unrelated_review_changes() {
    let name = "native_suites_bind_each_helper_and_ignore_unrelated_review_changes";
    let fixture = Fixture::recorded(name);
    let unrelated = inputs::CODEQL_POLICY_DATA
        .iter()
        .copied()
        .chain([
            ".github/workflows/ci.yml",
            ".github/workflows/codeql.yml",
            "security/authorization/operations.json",
            "security/image-policy.json",
            SHARED_GATE,
            "ops/ansible/files/deploy-public.sh",
        ])
        .collect::<Vec<_>>();
    let native = inputs::NATIVE_FILES
        .iter()
        .copied()
        .chain([
            "vendor/integrity.json",
            "vendor/native-components.json",
            "security/native/corpus.json",
            "security/native/corpus/rtp/header-short.hex",
        ])
        .collect::<Vec<_>>();
    for name in native.iter().chain(&unrelated) {
        fixture.write(name, b"original\n");
    }
    fixture.git(&["add", "."]);
    let suites = [Scope::NativeAsan, Scope::NativeUbsan, Scope::NativeReplay];
    let originals = suites.map(|scope| fixture.keys(scope).unwrap());
    let mut shared = false;
    let mut settled = Vec::new();
    for name in &unrelated {
        fixture.write(name, b"changed\n");
        shared |= *name == SHARED_GATE;
        settled.clear();
        for (scope, (rust, reference)) in suites.iter().zip(&originals) {
            let (key, own) = fixture.keys(*scope).unwrap();
            assert_eq!(own, *reference, "{name}");
            assert_eq!(key != *rust, shared, "{name}");
            settled.push(key);
        }
    }
    for name in &native {
        fixture.write(name, b"changed\n");
        for ((scope, (_, reference)), before) in suites.iter().zip(&originals).zip(&settled) {
            let (key, own) = fixture.keys(*scope).unwrap();
            assert_ne!(own, *reference, "{name}");
            assert_ne!(key, *before, "{name}");
        }
        fixture.write(name, b"original\n");
    }
    assert_eq!(fixture.replayed(), [72, 72, 0]);
}

/// From `test_codeql_review_data_changes_do_not_invalidate_evaluated_databases`:
/// a finding review changes the keys of the jobs that read it and none of
/// an evaluated database's, a native suite's or the backend's.
#[test]
fn codeql_review_data_changes_do_not_invalidate_evaluated_databases() {
    let fixture =
        Fixture::recorded("codeql_review_data_changes_do_not_invalidate_evaluated_databases");
    for name in inputs::CODEQL_POLICY_DATA {
        assert!(repository().join(name).is_file(), "{name}");
        fixture.write(name, b"original\n");
    }
    fixture.git(&["add", "."]);
    let originals = scopes()
        .into_iter()
        .map(|scope| (scope, fixture.key(scope)))
        .collect::<Vec<_>>();
    for name in inputs::CODEQL_POLICY_DATA {
        fixture.write(name, b"new exact finding review\n");
        for (scope, original) in &originals {
            let kept = scope.codeql() || scope.native() || *scope == Scope::Backend;
            assert_eq!(fixture.key(*scope) == *original, kept, "{name} {scope:?}");
        }
        fixture.write(name, b"original\n");
    }
    assert_eq!(fixture.replayed(), [54, 0, 0]);
}

/// From `test_native_codeql_binds_its_actual_build_analysis_and_policy_inputs`:
/// the traced build's setup and the query identity each change the native
/// analysis's key. Of the Python test's names the deleted vendor verifier
/// and process helper are gone with their files.
#[test]
fn native_codeql_binds_its_actual_build_analysis_and_policy_inputs() {
    let fixture =
        Fixture::recorded("native_codeql_binds_its_actual_build_analysis_and_policy_inputs");
    for name in NATIVE_CODEQL_INPUTS {
        assert!(repository().join(name).is_file(), "{name}");
        fixture.write(name, b"original\n");
    }
    fixture.git(&["add", "."]);
    let original = fixture.key(Scope::CodeqlNative);
    for name in NATIVE_CODEQL_INPUTS {
        fixture.write(name, b"changed\n");
        assert_ne!(fixture.key(Scope::CodeqlNative), original, "{name}");
        fixture.write(name, b"original\n");
    }
    assert_eq!(fixture.replayed(), [10, 0, 0]);
}

/// From `test_rust_analysis_keeps_all_targets_embedded_data_and_tool_inputs`:
/// new targets and shared fixtures change the Rust analysis's key, even
/// outside its usual directories, and so does a target's tracked mode.
#[test]
fn rust_analysis_keeps_all_targets_embedded_data_and_tool_inputs() {
    let fixture =
        Fixture::recorded("rust_analysis_keeps_all_targets_embedded_data_and_tool_inputs");
    let names = inputs::CODEQL_NATIVE_FILES
        .iter()
        .copied()
        .chain([
            "Cargo.lock",
            "Cargo.toml",
            "rust-toolchain.toml",
            ".cargo/config.toml",
            "src/main.rs",
            "src/nested/fixture.txt",
            "load_tests/src/browser_profile.rs",
            "tests/authorization.rs",
            "new_target/module.rs",
            "web/new_target/helper.rs",
            "new_target/Cargo.toml",
            "new_target/rust-project.json",
            "migrations/001_example.sql",
            "vendor/mediasoup-sys-0.19.0/build.rs",
            "vendor/mediasoup-sys-0.19.0/meson.build",
            "vendor/seclists-passwords/10k-most-common.txt",
            "security/authorization/operations.json",
            "security/codeql/new-query.ql",
            "web/tests/layer-cap-cases.json",
        ])
        .collect::<Vec<_>>();
    for name in &names {
        fixture.write(name, b"original\n");
    }
    fixture.git(&["add", "."]);
    let key = fixture.key(Scope::CodeqlRust);
    for name in &names {
        fixture.write(name, b"changed\n");
        assert_ne!(fixture.key(Scope::CodeqlRust), key, "{name}");
        fixture.write(name, b"original\n");
    }
    fixture.git(&["update-index", "--chmod=+x", "new_target/module.rs"]);
    assert_ne!(fixture.key(Scope::CodeqlRust), key);
    assert_eq!(fixture.replayed(), [27, 0, 0]);
}

/// From `test_rust_analysis_ignores_unrelated_docs_deployment_and_frontend_tooling`:
/// an unchanged Rust database survives documentation, deployment and
/// frontend tooling work.
#[test]
fn rust_analysis_ignores_unrelated_docs_deployment_and_frontend_tooling() {
    let name = "rust_analysis_ignores_unrelated_docs_deployment_and_frontend_tooling";
    let fixture = Fixture::recorded(name);
    let unrelated = [
        "README.md",
        "docs/testing.md",
        "ops/ansible/files/deploy-public.sh",
        "ops/ansible/templates/public-app.env.j2",
        "ops/ansible/release.yml",
        ".github/workflows/ci.yml",
        "security/image-policy.json",
        "Dockerfile",
        "web/e2e/package-lock.json",
        "web/package.json",
        "web/tests/participant-hovercard.test.mjs",
        "load_tests/benchmark-local.mjs",
    ];
    for name in unrelated {
        fixture.write(name, b"original\n");
    }
    fixture.git(&["add", "."]);
    let key = fixture.key(Scope::CodeqlRust);
    for name in unrelated {
        fixture.write(name, b"changed\n");
        assert_eq!(fixture.key(Scope::CodeqlRust), key, "{name}");
    }
    assert_eq!(fixture.replayed(), [13, 0, 0]);
}

/// From `test_native_codeql_ignores_unrelated_ci_tools_but_binds_native_data_and_modes`:
/// application, deployment and image tooling leave the native extraction's
/// key alone, and its native data and modes change it. The reference
/// ignored the image gate's source; Rust's key changes with it.
#[test]
fn native_codeql_ignores_unrelated_ci_tools_but_binds_native_data_and_modes() {
    let name = "native_codeql_ignores_unrelated_ci_tools_but_binds_native_data_and_modes";
    let fixture = Fixture::recorded(name);
    let unrelated = [
        ".github/workflows/ci.yml",
        ".github/actions/native-toolchain/action.yml",
        SHARED_GATE,
        "ops/ansible/files/deploy-public.sh",
        "security/image-policy.json",
        "Dockerfile",
    ];
    let native = [
        "vendor/native-components.json",
        "vendor/integrity.json",
        "vendor/mediasoup-sys-0.19.0/subprojects/libuv.wrap",
        "vendor/mediasoup-sys-0.19.0/subprojects/packagefiles/libuv/meson.build",
        "security/codeql/native-coverage/qlpack.yml",
        "build/codeql-native-build.sh",
    ];
    for name in unrelated.iter().chain(&native) {
        fixture.write(name, b"original\n");
    }
    fixture.git(&["add", "."]);
    let (rust, reference) = fixture.keys(Scope::CodeqlNative).unwrap();
    let mut shared = false;
    let mut settled = rust.clone();
    for name in unrelated {
        fixture.write(name, b"changed\n");
        shared |= name == SHARED_GATE;
        let (key, own) = fixture.keys(Scope::CodeqlNative).unwrap();
        assert_eq!(own, reference, "{name}");
        assert_eq!(key != rust, shared, "{name}");
        settled = key;
    }
    for name in native {
        fixture.write(name, b"changed\n");
        let (key, own) = fixture.keys(Scope::CodeqlNative).unwrap();
        assert_ne!(own, reference, "{name}");
        assert_ne!(key, settled, "{name}");
        fixture.write(name, b"original\n");
    }
    fixture.git(&["update-index", "--chmod=+x", native[5]]);
    let (key, own) = fixture.keys(Scope::CodeqlNative).unwrap();
    assert_ne!(own, reference);
    assert_ne!(key, settled);
    assert_eq!(fixture.replayed(), [14, 14, 0]);
}

/// From `test_runner_architecture_trust_and_hosted_image_generation_are_bound`:
/// a local success cannot cross into another local image, a pull request,
/// another branch, a hosted image or another instruction set, and a local
/// image named without its digest is refused.
#[test]
fn runner_architecture_trust_and_hosted_image_generation_are_bound() {
    let name = "runner_architecture_trust_and_hosted_image_generation_are_bound";
    let mut fixture = Fixture::recorded(name);
    let originals = [Scope::RustLint, Scope::RustTest].map(|scope| (scope, fixture.key(scope)));
    let image = format!("docker.io/owned/runner@sha256:{}", "b".repeat(64));
    let updates: [&[(&str, &str)]; 4] = [
        &[("LOCAL_CI_RUNNER_IMAGE", image.as_str())],
        &[("GITHUB_EVENT_NAME", "pull_request")],
        &[("GITHUB_REF", "refs/heads/feature")],
        &[
            ("ACT", ""),
            ("ImageOS", "ubuntu24"),
            ("ImageVersion", "20261003.1"),
        ],
    ];
    for update in updates {
        let saved = update
            .iter()
            .map(|(name, value)| {
                let environment = &mut fixture.context.environment;
                (
                    *name,
                    environment.insert((*name).to_owned(), (*value).to_owned()),
                )
            })
            .collect::<Vec<_>>();
        for (scope, key) in &originals {
            assert_ne!(fixture.key(*scope), *key, "{update:?}");
        }
        for (name, saved) in saved {
            match saved {
                Some(saved) => fixture.context.environment.insert(name.to_owned(), saved),
                None => fixture.context.environment.remove(name),
            };
        }
    }
    let architecture = std::mem::replace(
        &mut fixture.context.architecture,
        "other-architecture".to_owned(),
    );
    for (scope, key) in &originals {
        assert_ne!(fixture.key(*scope), *key);
    }
    fixture.context.architecture = architecture;
    fixture.context.environment.insert(
        "LOCAL_CI_RUNNER_IMAGE".to_owned(),
        "owned/runner:latest".to_owned(),
    );
    fixture.key_refused(Scope::RustTest);
    assert_eq!(fixture.refused(), "ci_cache_local_image");
    assert_eq!(fixture.replayed(), [13, 0, 1]);
}

/// From `test_receipt_reuse_names_original_revision_and_refuses_stale_inputs`:
/// reuse names the revision that proved it, and changed inputs can neither
/// reuse nor publish the old key.
#[test]
fn receipt_reuse_names_original_revision_and_refuses_stale_inputs() {
    let mut fixture =
        Fixture::recorded("receipt_reuse_names_original_revision_and_refuses_stale_inputs");
    let directory = fixture.context.root.join("cache");
    let key = fixture.key(Scope::RustTest);
    assert!(fixture.save(Scope::RustTest, &key, &directory));
    let sha = |fixture: &mut Fixture, digit: &str| {
        let revision = digit.repeat(40);
        fixture
            .context
            .environment
            .insert("GITHUB_SHA".to_owned(), revision);
    };
    sha(&mut fixture, "2");
    assert_eq!(
        fixture.verify(Scope::RustTest, &key, &directory),
        original_revision()
    );
    sha(&mut fixture, "1");
    fixture.write("src/main.rs", b"different backend\n");
    assert_eq!(fixture.verify(Scope::RustTest, &key, &directory), None);
    assert_eq!(fixture.refused(), "ci_cache_inputs_changed");
    let changed = fixture.context.root.join("changed-cache");
    assert!(!fixture.save(Scope::RustTest, &key, &changed));
    assert_eq!(fixture.refused(), "ci_cache_inputs_changed");
    assert!(!changed.exists());
    assert_eq!(fixture.replayed(), [5, 0, 2]);
}

/// From `test_rust_reference_requirements_invalidate_original_success`: the
/// selected Python's reviewed lock, as the recorder read it, belongs to the
/// Rust tests' receipt.
#[test]
fn rust_reference_requirements_invalidate_original_success() {
    let fixture = Fixture::recorded("rust_reference_requirements_invalidate_original_success");
    let requirements = text(recording(), "requirements");
    let path = "ops/ansible/requirements.txt";
    fixture.write(path, requirements.as_bytes());
    fixture.git(&["add", "."]);
    let original = fixture.key(Scope::RustTest);
    let directory = fixture.context.root.join("cache");
    assert!(fixture.save(Scope::RustTest, &original, &directory));
    fixture.write(
        path,
        format!("{requirements}\n# changed reviewed input\n").as_bytes(),
    );
    assert_ne!(fixture.key(Scope::RustTest), original);
    assert_eq!(fixture.verify(Scope::RustTest, &original, &directory), None);
    assert_eq!(fixture.refused(), "ci_cache_inputs_changed");
    assert_eq!(fixture.replayed(), [4, 0, 1]);
}

/// From `test_backend_cache_checks_every_binary_before_installing_any`: a
/// restored success installs both executables, and a changed or linked one
/// installs neither.
#[test]
fn backend_cache_checks_every_binary_before_installing_any() {
    let fixture = Fixture::recorded("backend_cache_checks_every_binary_before_installing_any");
    fixture.backend();
    let output = fixture.context.root.join("target/debug");
    let directory = fixture.context.root.join("cache");
    let key = fixture.key(Scope::Backend);
    assert!(fixture.save(Scope::Backend, &key, &directory));
    for name in inputs::BINARIES {
        fs::remove_file(output.join(name)).unwrap();
        fs::remove_file(output.join(format!("{name}.d"))).unwrap();
    }
    assert_eq!(
        fixture.verify(Scope::Backend, &key, &directory),
        original_revision()
    );
    for name in inputs::BINARIES {
        assert_eq!(
            fs::read(output.join(name)).unwrap(),
            b"owned executable fixture"
        );
        fs::remove_file(output.join(name)).unwrap();
    }
    fs::write(directory.join("load_test"), b"modified executable").unwrap();
    assert_eq!(fixture.verify(Scope::Backend, &key, &directory), None);
    assert_eq!(fixture.refused(), "ci_cache_binary_changed");
    assert!(fs::read_dir(&output).unwrap().next().is_none());
    fs::remove_file(directory.join("load_test")).unwrap();
    std::os::unix::fs::symlink(directory.join("simplestChat"), directory.join("load_test"))
        .unwrap();
    assert_eq!(fixture.verify(Scope::Backend, &key, &directory), None);
    assert_eq!(fixture.replayed(), [5, 0, 2]);
}

/// From `test_embedded_ops_catalog_changes_every_compiled_tool_key_and_original_receipt`:
/// the embedded catalog changes every key and voids the receipt it proved,
/// while adjacent Ops data changes only the conservative scopes.
#[test]
fn embedded_ops_catalog_changes_every_compiled_tool_key_and_original_receipt() {
    let name = "embedded_ops_catalog_changes_every_compiled_tool_key_and_original_receipt";
    let fixture = Fixture::recorded(name);
    let catalog = "ops/ops-build-inputs.lock.json";
    let adjacent = "ops/unrelated-local-selection.json";
    fixture.write(catalog, b"original reviewed catalog\n");
    fixture.write(adjacent, b"original unrelated JSON\n");
    fixture.git(&["add", "."]);
    let originals = scopes()
        .into_iter()
        .map(|scope| (scope, fixture.key(scope)))
        .collect::<Vec<_>>();
    let directory = fixture.context.root.join("catalog-receipt");
    let tested = &originals[2].1;
    assert!(fixture.save(Scope::RustTest, tested, &directory));
    assert_eq!(
        fixture.verify(Scope::RustTest, tested, &directory),
        original_revision()
    );
    fixture.write(catalog, b"changed reviewed catalog\n");
    for (scope, original) in &originals {
        assert_ne!(fixture.key(*scope), *original, "{scope:?}");
    }
    assert_eq!(fixture.verify(Scope::RustTest, tested, &directory), None);
    assert_eq!(fixture.refused(), "ci_cache_inputs_changed");
    fixture.write(catalog, b"original reviewed catalog\n");
    fixture.write(adjacent, b"changed unrelated JSON\n");
    for (scope, original) in &originals {
        let conservative = matches!(scope, Scope::Automation | Scope::RustLint | Scope::RustTest);
        assert_eq!(fixture.key(*scope) != *original, conservative, "{scope:?}");
    }
    assert_eq!(fixture.replayed(), [30, 0, 1]);
}

/// From `test_backend_closure_preserves_build_inputs_without_operational_invalidations`:
/// guides, Python tests and deployment files leave the backend's key alone,
/// and each compiler input changes it.
#[test]
fn backend_closure_preserves_build_inputs_without_operational_invalidations() {
    let name = "backend_closure_preserves_build_inputs_without_operational_invalidations";
    let fixture = Fixture::recorded(name);
    let workflow = ".github/workflows/ci.yml";
    let compiled = inputs::BACKEND_FILES
        .iter()
        .copied()
        .chain([
            "Cargo.lock",
            "Cargo.toml",
            "rust-toolchain.toml",
            ".cargo/config.toml",
            "load_tests/src/main.rs",
            "load_tests/src/metrics/measurement.rs",
            "migrations/new.sql",
            "src/embedded.txt",
            "other-target/helper.rs",
            "web/new-target/helper.rs",
            "web/tests/layer-cap-cases.json",
        ])
        .filter(|name| *name != workflow)
        .collect::<Vec<_>>();
    let unrelated = [
        "README.md",
        "docs/testing.md",
        "ops/ansible/tests/monitoring-rules.test.yml",
        "ops/ansible/files/deploy-public.sh",
        "security/exceptions.json",
        "web/e2e/package-lock.json",
        "web/package.json",
        "Dockerfile",
    ];
    for name in compiled.iter().chain(&unrelated) {
        fixture.write(name, b"original\n");
    }
    fixture.git(&["add", "."]);
    let original = fixture.key(Scope::Backend);
    for name in unrelated {
        fixture.write(name, b"unrelated edit\n");
        assert_eq!(fixture.key(Scope::Backend), original, "{name}");
    }
    for name in &compiled {
        fixture.write(name, b"changed compiler input\n");
        assert_ne!(fixture.key(Scope::Backend), original, "{name}");
        fixture.write(name, b"original\n");
    }
    assert_eq!(fixture.replayed(), [29, 0, 0]);
}

/// From `test_backend_setup_defaults_stay_stable_and_compiler_overrides_invalidate`:
/// the pinned setup's own exports keep the backend's key, every compiler
/// override changes it, and another output directory is refused.
#[test]
fn backend_setup_defaults_stay_stable_and_compiler_overrides_invalidate() {
    let name = "backend_setup_defaults_stay_stable_and_compiler_overrides_invalidate";
    let mut fixture = Fixture::recorded(name);
    fixture.context.environment.remove("CARGO_HOME");
    fixture.context.environment.remove("CARGO_INCREMENTAL");
    let before = fixture.key(Scope::Backend);
    let home = Path::new(fixture.context.env("HOME")).join(".cargo");
    for (name, value) in [
        ("CARGO_HOME", home.display().to_string()),
        ("CARGO_INCREMENTAL", "0".to_owned()),
        ("CARGO_TERM_COLOR", "always".to_owned()),
        ("CACHE_ON_FAILURE", "false".to_owned()),
    ] {
        fixture.context.environment.insert(name.to_owned(), value);
    }
    assert_eq!(fixture.key(Scope::Backend), before);
    let overrides = inputs::BACKEND_ENVIRONMENT
        .iter()
        .copied()
        .filter(|name| *name != "CARGO_TARGET_DIR")
        .chain(["CARGO_PROFILE_DEV_OPT_LEVEL", "CARGO_BUILD_RUSTFLAGS"]);
    for name in overrides {
        let saved = fixture
            .context
            .environment
            .insert(name.to_owned(), "changed".to_owned());
        assert_ne!(fixture.key(Scope::Backend), before, "{name}");
        match saved {
            Some(saved) => fixture.context.environment.insert(name.to_owned(), saved),
            None => fixture.context.environment.remove(name),
        };
    }
    let output = fixture
        .context
        .root
        .join("other-output")
        .display()
        .to_string();
    fixture
        .context
        .environment
        .insert("CARGO_TARGET_DIR".to_owned(), output);
    fixture.key_refused(Scope::Backend);
    assert_eq!(fixture.refused(), "ci_backend_target_directory");
    fixture.context.environment.remove("CARGO_TARGET_DIR");
    fixture
        .context
        .environment
        .insert("CARGO_INCREMENTAL".to_owned(), "1".to_owned());
    assert_ne!(fixture.key(Scope::Backend), before);
    assert_eq!(fixture.replayed(), [49, 0, 1]);
}

/// From `test_backend_dependency_guard_admits_keyed_directories_with_tracked_children`:
/// a rule's directory is admitted only once its children are tracked and
/// keyed, and then the success restores.
#[test]
fn backend_dependency_guard_admits_keyed_directories_with_tracked_children() {
    let name = "backend_dependency_guard_admits_keyed_directories_with_tracked_children";
    let fixture = Fixture::recorded(name);
    fixture.write("migrations/001_create_users.sql", b"original migration\n");
    fixture.backend();
    let root = fixture.context.root.clone();
    for name in inputs::BINARIES {
        let words = [
            format!("{}:", root.join("target/debug").join(name).display()),
            root.join("src/main.rs").display().to_string(),
            root.join("migrations").display().to_string(),
        ];
        let words = words.map(|word| word.replace(' ', "\\ "));
        let rule = format!("{}\n\n", words.join(" \\\n  "));
        fixture.write(&format!("target/debug/{name}.d"), rule.as_bytes());
    }
    let unpublished = root.join("unpublished");
    let key = fixture.key(Scope::Backend);
    assert!(!fixture.save(Scope::Backend, &key, &unpublished));
    assert_eq!(fixture.refused(), "ci_backend_dep_tree");
    assert!(!unpublished.exists());
    fixture.git(&["add", "migrations/001_create_users.sql"]);
    let key = fixture.key(Scope::Backend);
    let directory = root.join("cache");
    assert!(fixture.save(Scope::Backend, &key, &directory));
    for name in inputs::BINARIES {
        fs::remove_file(root.join("target/debug").join(name)).unwrap();
    }
    assert_eq!(
        fixture.verify(Scope::Backend, &key, &directory),
        original_revision()
    );
    for name in inputs::BINARIES {
        let installed = fs::read(root.join("target/debug").join(name)).unwrap();
        assert_eq!(installed, b"owned executable fixture");
    }
    assert_eq!(fixture.replayed(), [5, 0, 1]);
}

/// From `test_backend_dependency_guard_rejects_wrong_targets_and_unsupported_rules`:
/// a rule for another target, an external source, a further line and an
/// unbound native or generated file each refuse publication. The reference
/// judged the rule alone; Rust's `save` judges it before writing anything.
#[test]
fn backend_dependency_guard_rejects_wrong_targets_and_unsupported_rules() {
    let name = "backend_dependency_guard_rejects_wrong_targets_and_unsupported_rules";
    let fixture = Fixture::recorded(name);
    fixture.backend();
    let path = fixture.context.root.join("target/debug/simplestChat.d");
    let original = fs::read_to_string(&path).unwrap();
    let key = fingerprint::key(&fixture.context, Scope::Backend).unwrap();
    let unbound = "target/debug/build/aws-lc-sys-1234567890abcdef/out/unbound.rs";
    for (index, malformed) in [
        original.replace("/simplestChat:", "/another-binary:"),
        format!("{} /unbound/compiler/source.rs\n", original.trim_end()),
        format!("{original}# env-dep:UNBOUND=value\n"),
        original.replace("src/main.rs", "target/openssl-3.5.8/lib/libssl.a"),
        original.replace("src/main.rs", unbound),
    ]
    .into_iter()
    .enumerate()
    {
        fs::write(&path, malformed).unwrap();
        let directory = fixture.context.root.join(format!("unpublished-{index}"));
        fixture.dependencies_refused(&key, &directory);
        assert!(fixture.refused().starts_with("ci_backend_dep"), "{index}");
    }
    assert_eq!(fixture.replayed(), [5, 0, 5]);
}

/// The inputs of the restore checks and of the Rust tests that the Python
/// test indexed without Git.
const RESTORE_PATHS: [&str; 6] = [
    "ops/src/release/attestation/restore.rs",
    "ops/src/release/attestation/restore/tests.rs",
    "ops/tests/release_backup_cli.rs",
    "ops/src/release/attestation/restore/database_tests.rs",
    "xtask/src/ci/automation.rs",
    ".github/workflows/ci.yml",
];

/// From `test_restore_inputs_invalidate_existing_checks_and_rust_receipts_without_git`:
/// over a fixed index and the runner identity the Python test gave, each
/// restore input changes the automation and Rust test keys and voids their
/// receipts until it is restored.
#[test]
fn restore_inputs_invalidate_existing_checks_and_rust_receipts_without_git() {
    let name = "restore_inputs_invalidate_existing_checks_and_rust_receipts_without_git";
    let mut fixture = Fixture::recorded(name);
    let root = fixture.context.root.clone();
    let index = RESTORE_PATHS
        .map(|name| format!("100644 {} 0\t{name}\0", "a".repeat(40)))
        .concat();
    fixture.write("index", index.as_bytes());
    fixture.write(
        "tools/git",
        concat!(
            "#!/bin/sh\n",
            "case \"$*\" in\n",
            "'ls-files --stage -z') exec /bin/cat \"$CI_TEST_INDEX\";;\n",
            "*) exit 97;;\nesac\n"
        )
        .as_bytes(),
    );
    fs::set_permissions(root.join("tools/git"), fs::Permissions::from_mode(0o700)).unwrap();
    for (name, value) in [
        ("PATH", root.join("tools").display().to_string()),
        ("CI_TEST_INDEX", root.join("index").display().to_string()),
        (
            "LOCAL_CI_RUNNER_IMAGE",
            format!("owned@sha256:{}", "a".repeat(64)),
        ),
    ] {
        fixture.context.environment.insert(name.to_owned(), value);
    }
    for name in RESTORE_PATHS {
        fixture.write(name, b"original restore CI input\n");
    }
    for scope in [Scope::Automation, Scope::RustTest] {
        let key = fixture.key(scope);
        let receipt = root.join(format!("receipt-{}", scope.name()));
        assert!(fixture.save(scope, &key, &receipt));
        assert_eq!(fixture.verify(scope, &key, &receipt), original_revision());
        for name in RESTORE_PATHS {
            fixture.write(name, b"changed restore CI input\n");
            assert_ne!(fixture.key(scope), key, "{scope:?} {name}");
            assert_eq!(fixture.verify(scope, &key, &receipt), None, "{name}");
            assert_eq!(fixture.refused(), "ci_cache_inputs_changed");
            fixture.write(name, b"original restore CI input\n");
            assert_eq!(fixture.verify(scope, &key, &receipt), original_revision());
        }
    }
    assert_eq!(fixture.replayed(), [42, 0, 12]);
}
