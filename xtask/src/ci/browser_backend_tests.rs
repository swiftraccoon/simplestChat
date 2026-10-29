//! Rust-only browser artifact policy; the nine reference scopes stay unchanged.
//! Inert binaries and depinfo test reuse without running an application or compiler.

use super::*;
use serde_json::Value;

const BUILD: &str = "cargo build --locked -p simplestchat -p simplestchat-load-test --bins";

fn key(fixture: &Fixture) -> String {
    fingerprint::key(&fixture.context, Scope::BrowserBackend).unwrap()
}

fn dependency(fixture: &Fixture, name: &str, extra: &str) -> Vec<u8> {
    let escaped = |path: &Path| path.display().to_string().replace(' ', "\\ ");
    let target = fixture.context.root.join("target/debug").join(name);
    let source = fixture.context.root.join("src/main.rs");
    let extra = fixture.context.root.join(extra);
    format!(
        "{}: {} {}\n",
        escaped(&target),
        escaped(&source),
        escaped(&extra)
    )
    .into_bytes()
}

fn remove_binaries(fixture: &Fixture) {
    for name in inputs::BINARIES {
        fs::remove_file(fixture.context.root.join("target/debug").join(name)).unwrap();
    }
}

fn assert_no_binaries(fixture: &Fixture) {
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
fn product_native_and_current_policy_inputs_invalidate_only_the_new_role() {
    let fixture = Fixture::new();
    let relevant = [
        "src/lib.rs",
        "load_tests/src/main.rs",
        "load_tests/Cargo.toml",
        "migrations/021_invitation_secrets.sql",
        "vendor/worker.cpp",
        ".cargo/config.toml",
        "security/authorization/policy.json",
    ];
    for name in relevant
        .into_iter()
        .chain(inputs::BROWSER_BACKEND_FILES.iter().copied())
    {
        if name != ".github/workflows/ci.yml" {
            fixture.write(name, b"original role input\n");
        }
    }
    let unrelated = [
        "ops/src/release/maintain.rs",
        "ops/tests/maintenance.rs",
        "ops/ops-build-inputs.lock.json",
        "xtask/src/ci/ops_artifact/hosted/native_build.rs",
        "xtask/src/ci/automation.rs",
        "docs/independent.md",
        "web/src/room.ts",
    ];
    for name in unrelated {
        fixture.write(name, b"unrelated original\n");
    }
    fixture.git(&["add", "."]);
    let original = key(&fixture);
    assert!(original.starts_with("ci-verified-v1-browser-backend-main-"));
    assert_ne!(
        original,
        fingerprint::key(&fixture.context, Scope::Backend).unwrap()
    );
    assert_eq!(Scope::parse("browser-backend"), Some(Scope::BrowserBackend));
    assert!(Scope::parse("browser-backend-neighbor").is_none());
    assert!(!inputs::REFERENCE_SCOPES.contains(&"browser-backend"));
    for name in relevant
        .into_iter()
        .chain(inputs::BROWSER_BACKEND_FILES.iter().copied())
    {
        if name == ".github/workflows/ci.yml" {
            continue;
        }
        let before = fs::read(fixture.context.root.join(name)).unwrap();
        fixture.write(name, b"changed required input\n");
        assert_ne!(key(&fixture), original, "{name}");
        fixture.write(name, &before);
    }
    for name in unrelated {
        fixture.write(name, b"changed unrelated input\n");
        assert_eq!(key(&fixture), original, "{name}");
    }
    fixture.write("src/new-role-source.rs", b"new product source\n");
    assert_eq!(key(&fixture), original);
    fixture.git(&["add", "src/new-role-source.rs"]);
    assert_ne!(key(&fixture), original);
}

#[test]
fn current_environment_runner_and_workflow_prefix_still_bound_reuse() {
    let mut fixture = Fixture::new();
    let original = key(&fixture);
    for name in [
        "CC",
        "OPENSSL_DIR",
        "RUSTFLAGS",
        "CARGO_PROFILE_DEV_OPT_LEVEL",
    ] {
        let before = fixture
            .context
            .environment
            .insert(name.into(), "changed".into());
        assert_ne!(key(&fixture), original, "{name}");
        if let Some(before) = before {
            fixture.context.environment.insert(name.into(), before);
        } else {
            fixture.context.environment.remove(name);
        }
    }
    fixture.context.environment.insert(
        "LOCAL_CI_RUNNER_IMAGE".into(),
        format!("docker.io/owned/runner@sha256:{}", "b".repeat(64)),
    );
    assert_ne!(key(&fixture), original);
    fixture.context.environment.insert(
        "LOCAL_CI_RUNNER_IMAGE".into(),
        format!("docker.io/owned/runner@sha256:{}", "a".repeat(64)),
    );
    fixture
        .context
        .environment
        .insert("GITHUB_EVENT_NAME".into(), "pull_request".into());
    let untrusted = key(&fixture);
    assert!(untrusted.starts_with("ci-verified-v1-browser-backend-untrusted-"));
    assert_ne!(untrusted, original);
    fixture
        .context
        .environment
        .insert("GITHUB_EVENT_NAME".into(), "push".into());
    let workflow = ".github/workflows/ci.yml";
    let before = fs::read_to_string(fixture.context.root.join(workflow)).unwrap();
    assert!(before.contains(BUILD));
    fixture.write(
        workflow,
        before
            .replace(BUILD, &format!("{BUILD} --release"))
            .as_bytes(),
    );
    assert_ne!(key(&fixture), original);
    fixture.write(workflow, before.as_bytes());
    fixture.context.environment.insert(
        "CARGO_TARGET_DIR".into(),
        fixture
            .context
            .root
            .join("foreign-target")
            .display()
            .to_string(),
    );
    assert!(fingerprint::key(&fixture.context, Scope::BrowserBackend).is_err());
}

#[test]
fn new_receipts_retain_four_artifact_checks_and_reject_old_scope_or_tampering() {
    let mut fixture = Fixture::new();
    fixture.backend();
    let original = key(&fixture);
    let legacy = fixture.context.root.join("legacy-cache");
    let legacy_key = fingerprint::key(&fixture.context, Scope::Backend).unwrap();
    receipts::save(&fixture.context, Scope::Backend, &legacy_key, &legacy).unwrap();
    let directory = fixture.context.root.join("browser-cache");
    receipts::save(
        &fixture.context,
        Scope::BrowserBackend,
        &original,
        &directory,
    )
    .unwrap();
    let receipt: Value =
        serde_json::from_slice(&fs::read(directory.join("receipt.json")).unwrap()).unwrap();
    assert_eq!(receipt["artifacts"].as_object().unwrap().len(), 4);
    remove_binaries(&fixture);
    assert!(receipts::verify(&fixture.context, Scope::BrowserBackend, &original, &legacy).is_err());
    assert_no_binaries(&fixture);
    fixture
        .context
        .environment
        .insert("GITHUB_SHA".into(), "2".repeat(40));
    assert_eq!(
        receipts::verify(
            &fixture.context,
            Scope::BrowserBackend,
            &original,
            &directory
        )
        .unwrap(),
        "1".repeat(40)
    );
    for name in inputs::BINARIES {
        assert_eq!(
            fs::read(fixture.context.root.join("target/debug").join(name)).unwrap(),
            b"owned executable fixture"
        );
    }
    remove_binaries(&fixture);
    fs::write(directory.join("load_test"), b"hostile changed executable").unwrap();
    assert!(
        receipts::verify(
            &fixture.context,
            Scope::BrowserBackend,
            &original,
            &directory
        )
        .is_err()
    );
    assert_no_binaries(&fixture);
}

#[test]
fn excluded_tracked_and_untracked_dependencies_refuse_before_save_or_install() {
    for extra in [
        "ops/src/foreign.rs",
        "xtask/src/ci/ops_artifact/foreign.rs",
        "docs/foreign.md",
        "src/untracked.txt",
    ] {
        let fixture = Fixture::new();
        fixture.write(extra, b"hostile dependency\n");
        if extra != "src/untracked.txt" {
            fixture.git(&["add", extra]);
        }
        fixture.backend();
        let original = key(&fixture);
        fixture.write(
            "target/debug/load_test.d",
            &dependency(&fixture, "load_test", extra),
        );
        let unpublished = fixture.context.root.join("unpublished");
        let error = receipts::save(
            &fixture.context,
            Scope::BrowserBackend,
            &original,
            &unpublished,
        )
        .unwrap_err();
        assert!(error.to_string().contains("ci_backend_dep_unkeyed"));
        assert!(!unpublished.exists());
        fixture.backend();
        let directory = fixture.context.root.join("browser-cache");
        receipts::save(
            &fixture.context,
            Scope::BrowserBackend,
            &original,
            &directory,
        )
        .unwrap();
        let hostile = dependency(&fixture, "load_test", extra);
        fs::write(directory.join("load_test.d"), &hostile).unwrap();
        let receipt_path = directory.join("receipt.json");
        let mut receipt: Value = serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
        receipt["artifacts"]["load_test.d"] = io::sha256(&hostile).into();
        fs::write(receipt_path, serde_json::to_vec(&receipt).unwrap()).unwrap();
        remove_binaries(&fixture);
        let error = receipts::verify(
            &fixture.context,
            Scope::BrowserBackend,
            &original,
            &directory,
        )
        .unwrap_err();
        assert!(error.to_string().contains("ci_backend_dep_unkeyed"));
        assert_no_binaries(&fixture);
    }
}

#[test]
fn browser_build_selects_only_app_and_full_load_packages_without_weakening_tests() {
    use serde::Deserialize;
    #[derive(Deserialize)]
    struct Workflow {
        jobs: BTreeMap<String, Value>,
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(".github/workflows/ci.yml");
    let workflow: Workflow = serde_saphyr::from_slice(&fs::read(path).unwrap()).unwrap();
    let steps = workflow.jobs["browser"]["steps"].as_array().unwrap();
    let build = steps
        .iter()
        .find(|step| step["name"] == "Build server and load generator")
        .unwrap();
    assert_eq!(build["run"], BUILD);
    let cache = steps
        .iter()
        .find(|step| step["uses"] == "./.github/actions/native-toolchain")
        .unwrap();
    assert_eq!(cache["with"]["cache-key"], "browser-backend-v1");
    let scopes = steps
        .iter()
        .filter(|step| !step["env"]["CI_CACHE_SCOPE"].is_null())
        .map(|step| step["env"]["CI_CACHE_SCOPE"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(scopes, ["browser-backend"; 3]);
    let tests = workflow.jobs["rust-test"]["steps"].as_array().unwrap();
    let build = tests
        .iter()
        .find(|step| step["name"] == "Build server, load generator and test harnesses")
        .unwrap();
    assert_eq!(
        build["run"],
        "cargo build --locked --workspace --all-targets --all-features --profile test"
    );
}
