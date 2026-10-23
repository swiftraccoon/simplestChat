//! Fixed source closures carried forward from the CI cache reference.

use super::Scope;

pub(super) const SCOPES: &[&str] = &[
    "automation",
    "rust-lint",
    "rust-test",
    "backend",
    "native-asan",
    "native-ubsan",
    "native-replay",
    "codeql-native",
    "codeql-rust",
];

pub(super) const BINARIES: &[&str] = &["simplestChat", "load_test"];

pub(super) const NATIVE_PREFIXES: &[&str] = &["vendor/", "security/native/"];

pub(super) const NATIVE_FILES: &[&str] = &[
    ".github/workflows/security.yml",
    "build/ci-local-act.json",
    "build/ci-local-act.patch",
    "build/ci-local-docker.sh",
    "build/ci_local_act.py",
    "build/ci_verified.py",
    "build/install-openssl.sh",
    "build/native-security.Dockerfile",
    "build/native_security.py",
    "build/native_security_cache.py",
    "build/pip-constraints.txt",
    "build/security_codeql_resources.py",
    "build/security_context.py",
    "build/security_source_scope.py",
    "build/security_tools.py",
    "build/security_vendor.py",
    "ops/ansible/files/bounded_process.py",
    "ops/ansible/files/release_json.py",
];

pub(super) const CODEQL_NATIVE_PREFIXES: &[&str] = &["vendor/", "security/codeql/"];

pub(super) const CODEQL_POLICY_DATA: &[&str] = &[
    "security/codeql-native-review-2026-09-30.json",
    "security/codeql-native-review-2026-09-30.md",
    "security/codeql-review-2026-09-30.json",
    "security/codeql-review-2026-09-30.md",
    "security/exceptions.json",
];

pub(super) const CODEQL_NATIVE_FILES: &[&str] = &[
    ".github/workflows/codeql.yml",
    "build/ci_verified.py",
    "build/codeql-native-build.sh",
    "build/install-openssl.sh",
    "build/pip-constraints.txt",
    "build/security_codeql.py",
    "build/security_codeql_cache.py",
    "build/security_codeql_cargo.py",
    "build/security_codeql_local.py",
    "build/security_codeql_resources.py",
    "build/security_codeql_rust_sources.py",
    "build/security_codeql_sources.py",
    "build/security_codeql_tools.py",
    "build/security_codeql_triage.py",
    "build/security_context.py",
    "build/security_findings.py",
    "build/security_policy.py",
    "build/security_source_scope.py",
    "build/security_tools.py",
    "build/security_vendor.py",
    "ops/ansible/files/bounded_process.py",
    "ops/ansible/files/release_json.py",
    "security/codeql-toolchain.json",
];

pub(super) const CODEQL_RUST_PREFIXES: &[&str] = &[
    "src/",
    "tests/",
    "migrations/",
    "vendor/",
    ".cargo/",
    "security/authorization/",
    "security/codeql/",
];

pub(super) const CODEQL_RUST_MANIFESTS: &[&str] = &[
    "Cargo.lock",
    "Cargo.toml",
    "rust-project.json",
    "rust-toolchain",
    "rust-toolchain.toml",
];

pub(super) const BACKEND_PREFIXES: &[&str] = &[
    "src/",
    "tests/",
    "load_tests/",
    "xtask/",
    "migrations/",
    "vendor/",
    ".cargo/",
];

pub(super) const BACKEND_FILES: &[&str] = &[
    ".github/actions/native-toolchain/action.yml",
    ".github/workflows/ci.yml",
    "build/ci-local-act.json",
    "build/ci-local-act.patch",
    "build/ci-local-postgres.sh",
    "build/ci_local_act.py",
    "build/ci_verified.py",
    "build/install-aws-lc.sh",
    "build/install-openssl.sh",
    "build/pip-constraints.txt",
    "build/security_tools.py",
    "ops/ansible/files/bounded_process.py",
    "ops/ansible/files/release_json.py",
    "ops/ops-build-inputs.lock.json",
];

pub(super) const BACKEND_ENVIRONMENT: &[&str] = &[
    "AR",
    "AWS_LC_SYS_STATIC",
    "AWS_LC_SYS_SYSTEM_DIR",
    "AWS_LC_SYS_SYSTEM_SKIP_VERSION_CHECK",
    "CARGO_BUILD_TARGET",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_TARGET_DIR",
    "CC",
    "CFLAGS",
    "CPPFLAGS",
    "CXX",
    "CXXFLAGS",
    "DOCS_RS",
    "LDFLAGS",
    "MACOSX_DEPLOYMENT_TARGET",
    "MESON",
    "MESON_ARGS",
    "MESON_VERSION",
    "NINJA_VERSION",
    "OPENSSL_DIR",
    "OPENSSL_STATIC",
    "PIP_CERT",
    "PIP_CLIENT_CERT",
    "PIP_CONFIG_FILE",
    "PIP_CONSTRAINT",
    "PIP_EXTRA_INDEX_URL",
    "PIP_FIND_LINKS",
    "PIP_INDEX_URL",
    "PIP_NO_BINARY",
    "PIP_NO_INDEX",
    "PIP_ONLY_BINARY",
    "PIP_PREFER_BINARY",
    "PIP_PROXY",
    "PIP_REQUIRE_HASHES",
    "PIP_TRUSTED_HOST",
    "PKG_CONFIG_PATH",
    "PYTHON",
    "PYTHONPATH",
    "RUSTC",
    "RUSTC_WORKSPACE_WRAPPER",
    "RUSTC_WRAPPER",
    "RUSTFLAGS",
    "RUSTUP_TOOLCHAIN",
    "SDKROOT",
];

fn ci_tool_input(name: &str) -> bool {
    name.starts_with("xtask/")
        || name.starts_with(".cargo/")
        || [
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain",
            "rust-toolchain.toml",
            "ops/ops-build-inputs.lock.json",
        ]
        .contains(&name)
}

pub(super) fn backend_input(name: &str) -> bool {
    name.ends_with(".rs")
        || CODEQL_RUST_MANIFESTS.contains(&name.rsplit('/').next().unwrap_or(name))
        || BACKEND_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
        || name.starts_with("security/authorization/")
        || (name.starts_with("web/tests/") && name.ends_with(".json"))
        || BACKEND_FILES.contains(&name)
}

pub(super) fn native_security_input(name: &str) -> bool {
    ci_tool_input(name)
        || NATIVE_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
        || NATIVE_FILES.contains(&name)
}

pub(super) fn native_codeql_input(name: &str) -> bool {
    ci_tool_input(name)
        || CODEQL_NATIVE_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
        || CODEQL_NATIVE_FILES.contains(&name)
}

pub(super) fn rust_codeql_input(name: &str) -> bool {
    ci_tool_input(name)
        || name.ends_with(".rs")
        || CODEQL_RUST_MANIFESTS.contains(&name.rsplit('/').next().unwrap_or(name))
        || CODEQL_RUST_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
        || (name.starts_with("web/tests/") && name.ends_with(".json"))
        || CODEQL_NATIVE_FILES.contains(&name)
}

fn independent_helper_or_guide(name: &str) -> bool {
    name.strip_prefix("build/tests/")
        .is_some_and(|suffix| !suffix.contains('/') && suffix.ends_with(".test.mjs"))
        || (name.starts_with("docs/") && name.ends_with(".md"))
}

pub(super) fn selected(scope: Scope, name: &str) -> bool {
    if scope.native() && !native_security_input(name) {
        return false;
    }
    if scope == Scope::Backend && !backend_input(name) {
        return false;
    }
    if scope.codeql() && CODEQL_POLICY_DATA.contains(&name) {
        return false;
    }
    if scope == Scope::CodeqlNative && !native_codeql_input(name) {
        return false;
    }
    if scope == Scope::CodeqlRust && !rust_codeql_input(name) {
        return false;
    }
    let rust_data = matches!(
        scope,
        Scope::RustLint | Scope::RustTest | Scope::Backend | Scope::CodeqlRust
    ) && name.ends_with(".json");
    if (!matches!(
        scope,
        Scope::Automation | Scope::CodeqlRust | Scope::Backend
    ) && name.starts_with("web/")
        && !rust_data)
        || (matches!(scope, Scope::RustLint | Scope::RustTest) && independent_helper_or_guide(name))
    {
        return false;
    }
    true
}
