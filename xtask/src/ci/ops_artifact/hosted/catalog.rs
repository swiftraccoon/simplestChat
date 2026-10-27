//! Reviewed content selections. Runtime requests cannot replace these origins,
//! versions, complete archive digests, exact sizes or component identities.

use super::{context, failure};
use crate::tooling::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const ORIGINAL: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../ops/ops-build-inputs.lock.json"
));
pub(super) const MAX_ARCHIVE: u64 = 256 * 1024 * 1024;
pub(super) const MAX_TOTAL: u64 = 512 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub(super) enum Format {
    #[serde(rename = "rust-gzip")]
    RustGzip,
    #[serde(rename = "ubuntu-deb")]
    UbuntuDeb,
    #[serde(rename = "native-gzip")]
    NativeGzip,
}

pub(super) const MUSL_COMMIT: &str = "7813e9119eb5fe681b6750b8a033a0490bca0941";
const ROOTS: [&str; 12] = [
    "gcc-13-x86-64-linux-gnu",
    "binutils-x86-64-linux-gnu",
    "libc6-dev",
    "make",
    "perl",
    "coreutils",
    "diffutils",
    "sed",
    "grep",
    "mawk",
    "dash",
    "findutils",
];

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Artifact {
    pub name: String,
    pub format: Format,
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
    pub version: String,
    pub archive_root: String,
    pub component: String,
    pub dependencies: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Metadata {
    url: String,
    sha256: String,
    bytes: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct UbuntuIndex {
    suite: String,
    url: String,
    sha256: String,
    bytes: u64,
    xz_members: u8,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Wire {
    schema_version: u8,
    profile: String,
    rust_version: String,
    host: String,
    target: String,
    rust_manifest: Metadata,
    ubuntu_indices: Vec<UbuntuIndex>,
    ubuntu_roots: Vec<String>,
    artifacts: Vec<Artifact>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Identity {
    pub profile: &'static str,
    pub sha256: String,
    pub bytes: usize,
    pub archives: usize,
    pub archive_bytes: u64,
}
pub(super) struct Catalog {
    pub artifacts: Vec<Artifact>,
    pub identity: Identity,
}

fn token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'+' | b'~' | b':')
        })
}

impl Catalog {
    pub fn load() -> Result<Self> {
        Self::decode(ORIGINAL)
    }
    pub(super) fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > 128 * 1024 {
            return Err(failure(1));
        }
        let wire: Wire = serde_json::from_slice(bytes).map_err(|_| failure(1))?;
        if wire.schema_version != 1
            || wire.profile != "ops-hosted-kit-v2"
            || wire.rust_version != context::VERSION
            || wire.host != context::HOST
            || wire.target != context::TARGET
            || wire.rust_manifest.url
                != "https://static.rust-lang.org/dist/channel-rust-1.99.0.toml"
            || !super::super::digest(&wire.rust_manifest.sha256)
            || wire.rust_manifest.bytes == 0
            || wire.rust_manifest.bytes > 4 * 1024 * 1024
            || wire.ubuntu_indices.len() != 3
            || wire.ubuntu_roots != ROOTS
            || wire.artifacts.len() < 5
            || wire.artifacts.len() > 96
        {
            return Err(failure(1));
        }
        let mut suites = BTreeSet::new();
        for index in &wire.ubuntu_indices {
            let origin = match index.suite.as_str() {
                "noble" | "noble-updates" => "https://archive.ubuntu.com/ubuntu/",
                "noble-security" => "https://security.ubuntu.com/ubuntu/",
                _ => return Err(failure(1)),
            };
            if !suites.insert(&index.suite)
                || index.url
                    != format!(
                        "{origin}dists/{}/main/binary-amd64/Packages.xz",
                        index.suite
                    )
                || !super::super::digest(&index.sha256)
                || index.bytes == 0
                || index.bytes > 12 * 1024 * 1024
                || !(1..=8).contains(&index.xz_members)
            {
                return Err(failure(1));
            }
        }
        let mut names = BTreeSet::new();
        let mut rust = BTreeSet::new();
        let mut native = BTreeSet::new();
        let mut total = 0_u64;
        for artifact in &wire.artifacts {
            if !token(&artifact.name)
                || !token(&artifact.version)
                || !names.insert(artifact.name.clone())
                || !super::super::digest(&artifact.sha256)
                || artifact.bytes == 0
                || artifact.bytes > MAX_ARCHIVE
                || artifact.url.len() > 1024
                || artifact.url.contains(['?', '#', '\\'])
            {
                return Err(failure(1));
            }
            total = total
                .checked_add(artifact.bytes)
                .ok_or_else(|| failure(1))?;
            match artifact.format {
                Format::NativeGzip => {
                    let (version, root, url) = match artifact.name.as_str() {
                        "musl" => (
                            "1.2.6+7813e9119eb5".to_owned(),
                            format!("musl-{MUSL_COMMIT}"),
                            format!(
                                concat!(
                                    "https://git.musl-libc.org/cgit/musl/snapshot/",
                                    "musl-{}.tar.gz"
                                ),
                                MUSL_COMMIT
                            ),
                        ),
                        "openssl" => (
                            "4.1.0".to_owned(),
                            "openssl-4.1.0".to_owned(),
                            concat!(
                                "https://github.com/openssl/openssl/releases/download/",
                                "openssl-4.1.0/openssl-4.1.0.tar.gz"
                            )
                            .to_owned(),
                        ),
                        _ => return Err(failure(1)),
                    };
                    if artifact.version != version
                        || artifact.archive_root != root
                        || artifact.url != url
                        || !artifact.component.is_empty()
                        || !artifact.dependencies.is_empty()
                        || !native.insert(artifact.name.clone())
                    {
                        return Err(failure(1));
                    }
                }
                Format::RustGzip => {
                    let (package, target) =
                        if let Some(package) = artifact.name.strip_suffix(context::HOST) {
                            (package.trim_end_matches('-'), context::HOST)
                        } else if let Some(package) = artifact.name.strip_suffix(context::TARGET) {
                            (package.trim_end_matches('-'), context::TARGET)
                        } else {
                            return Err(failure(1));
                        };
                    if artifact.version != context::VERSION
                        || !artifact.dependencies.is_empty()
                        || artifact.archive_root != format!("{package}-1.99.0-{target}")
                        || artifact.url
                            != format!(
                                "https://static.rust-lang.org/dist/2026-10-01/{}.tar.gz",
                                artifact.archive_root
                            )
                        || !rust.insert((artifact.name.clone(), artifact.component.clone()))
                    {
                        return Err(failure(1));
                    }
                }
                Format::UbuntuDeb => {
                    let prefix = "https://archive.ubuntu.com/ubuntu/pool/main/";
                    let path = artifact
                        .url
                        .strip_prefix(prefix)
                        .ok_or_else(|| failure(1))?;
                    if !artifact.archive_root.is_empty()
                        || !artifact.component.is_empty()
                        || !path.ends_with("_amd64.deb") && !path.ends_with("_all.deb")
                        || path
                            .split('/')
                            .any(|p| p.is_empty() || p == "." || p == "..")
                        || !path
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"/._+~%-".contains(&b))
                        || artifact.dependencies.len() > 32
                        || artifact.dependencies.iter().any(|v| !token(v))
                    {
                        return Err(failure(1));
                    }
                }
            }
        }
        let expected = [
            (format!("rustc-{}", context::HOST), "rustc".to_owned()),
            (format!("cargo-{}", context::HOST), "cargo".to_owned()),
            (
                format!("rust-std-{}", context::HOST),
                format!("rust-std-{}", context::HOST),
            ),
            (
                format!("rust-std-{}", context::TARGET),
                format!("rust-std-{}", context::TARGET),
            ),
        ];
        if total > MAX_TOTAL
            || rust != expected.into_iter().collect()
            || native != BTreeSet::from(["musl".to_owned(), "openssl".to_owned()])
        {
            return Err(failure(1));
        }
        for artifact in &wire.artifacts {
            if artifact.dependencies.iter().any(|n| !names.contains(n)) {
                return Err(failure(1));
            }
        }
        Ok(Self {
            identity: Identity {
                profile: "ops-hosted-kit-v2",
                sha256: super::super::sha(bytes),
                bytes: bytes.len(),
                archives: wire.artifacts.len(),
                archive_bytes: total,
            },
            artifacts: wire.artifacts,
        })
    }
}
