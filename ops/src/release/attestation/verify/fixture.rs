//! Standalone test-only cached bytes shared by unit and actual CLI fixtures.
//!
//! Fake maintained-verifier results exercise policy and process wiring only.
//! They establish no signatures, API freshness, network, image or host proof.

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256, Sha384};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Cursor, Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);
const REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const PREDICATE_TYPE: &str = "https://github.com/swiftraccoon/simplestChat/attestations/release/v1";
const WORKFLOW: &str = ".github/workflows/ci.yml";
const REFERENCE: &str = "refs/heads/main";
const ISSUER: &str = "https://token.actions.githubusercontent.com";
const BOUND: [&str; 8] = [
    "image.tar",
    "release.json",
    "outcome.json",
    "source.json",
    "sbom.spdx.json",
    "image-security.json",
    "runtime-proof.json",
    "vex.openvex.json",
];
const SUBJECTS: [&str; 4] = [
    "image.tar",
    "sbom.spdx.json",
    "runtime-proof.json",
    "vex.openvex.json",
];
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Selection {
    pub(super) schema_version: u8,
    pub(super) repository: String,
    pub(super) artifact_id: u64,
    pub(super) build_run_id: u64,
    pub(super) ci_run_id: u64,
    pub(super) run_attempt: u64,
    pub(super) revision: String,
    pub(super) artifact_zip_bytes: u64,
    pub(super) zip_sha256: String,
}
pub(super) struct Fixture {
    pub(super) root: PathBuf,
    pub(super) candidate: PathBuf,
    pub(super) bin: PathBuf,
    pub(super) cwd: PathBuf,
    pub(super) home: PathBuf,
    pub(super) selection: Selection,
    pub(super) claim: Value,
    pub(super) response: Vec<u8>,
}
fn encoded(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}
fn checksum(value: &[u8]) -> String {
    Sha256::digest(value)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub(super) fn read(path: &Path) -> io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let result = (|| {
        if file.metadata()?.len() > 2 * 1024 * 1024 {
            return Err(io::Error::other("bounded inert fixture"));
        }
        let mut bytes = Vec::new();
        let mut buffer = vec![0; 65536];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                return Ok(bytes);
            }
            if bytes.len() + count > 2 * 1024 * 1024 {
                return Err(io::Error::other("bounded inert fixture"));
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
    })();
    let closed = nix::unistd::close(file).map_err(io::Error::from);
    result.and_then(|value| closed.map(|()| value))
}
fn write(path: &Path, bytes: &[u8], fresh: bool, mode: u32) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).mode(mode);
    if fresh {
        options.create_new(true);
    } else {
        options.truncate(true);
    }
    let mut file = options.open(path)?;
    let result = file.write_all(bytes).and_then(|()| file.flush());
    let closed = nix::unistd::close(file).map_err(io::Error::from);
    result.and(closed)
}
fn directory(path: &Path) {
    fs::DirBuilder::new().mode(0o700).create(path).unwrap();
}
fn archive(config: &[u8]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, bytes) in [
        (
            "manifest.json",
            encoded(&json!([{"Config":"config.json",
            "RepoTags":[format!("simplestchat-release/production:{REVISION}")],
            "Layers":["layer.tar"]}])),
        ),
        ("config.json", config.to_vec()),
        ("layer.tar", b"inert layer bytes, never executed".to_vec()),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o644);
        header.set_size(bytes.len() as u64);
        header.set_cksum();
        builder
            .append_data(&mut header, name, Cursor::new(bytes))
            .unwrap();
    }
    builder.into_inner().unwrap()
}
impl Fixture {
    pub(super) fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "PRIVATE-release-verification-{}-{}-{}",
            std::process::id(),
            module_path!().replace("::", "-"),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        directory(&root);
        let candidate = root.join("candidate");
        let bin = root.join("bin");
        let cwd = root.join("empty-cwd");
        let home = root.join("home");
        for path in [&candidate, &bin, &cwd, &home] {
            directory(path);
        }
        let config = encoded(&json!({"architecture":"amd64","os":"linux","config":{
            "User":"10001:10001","Cmd":["/app/simplestChat"],
            "Labels":{"org.opencontainers.image.revision":REVISION}}}));
        let image_id = format!("sha256:{}", checksum(&config));
        let image = archive(&config);
        let image_sha = checksum(&image);
        let input_hashes: BTreeMap<_, _> = [
            "Dockerfile",
            ".dockerignore",
            "Cargo.lock",
            "web/package-lock.json",
            "build/pip-constraints.txt",
        ]
        .into_iter()
        .map(|name| (name, checksum(name.as_bytes())))
        .collect();
        let mut files = BTreeMap::from([
            ("image.tar", image),
            (
                "release.json",
                encoded(&json!({"schemaVersion":1,"revision":REVISION,
                "platform":"linux/amd64","archiveSha256":image_sha,
                "imageTag":format!("simplestchat-release/production:{REVISION}"),
                "migrations":{"1":Sha384::digest(b"SELECT 1;").iter()
                    .map(|b|format!("{b:02x}")).collect::<String>()},
                "createdAt":"2026-09-13T00:00:00Z"})),
            ),
            (
                "outcome.json",
                encoded(&json!({"schemaVersion":1,"passed":true,"error":null,
                "revision":REVISION,"exportedImageId":image_id})),
            ),
            (
                "source.json",
                encoded(&json!({"revision":REVISION,"inputsSha256":
                input_hashes})),
            ),
            ("sbom.spdx.json", b"opaque SPDX fixture bytes".to_vec()),
            (
                "runtime-proof.json",
                encoded(&json!({"schemaVersion":1,"required":false,
                "passed":true,"archiveSha256":image_sha,"imageId":image_id,
                "revision":REVISION,"platform":"linux/amd64"})),
            ),
            ("vex.openvex.json", b"{\"statements\":[]}".to_vec()),
            ("release-predicate.json", b"{}".to_vec()),
            (
                "release-attestation.jsonl",
                b"inert bundle; never a signature\n".to_vec(),
            ),
        ]);
        files.insert(
            "image-security.json",
            encoded(&json!({"passed":true,
            "revision":REVISION,"platform":"linux/amd64","imageId":image_id,
            "archiveSha256":image_sha,"vexRequired":false})),
        );
        for (name, bytes) in files {
            write(&candidate.join(name), &bytes, true, 0o600).unwrap();
        }
        write(&candidate.join("artifact.zip"), &[], true, 0o600).unwrap();
        write(&home.join("response.json"), &[], true, 0o600).unwrap();
        write(&home.join("argv"), &[], true, 0o600).unwrap();
        write(
            &bin.join("gh"),
            concat!(
                "#!/bin/sh\n",
                "[ \"${LC_ALL-}\" = C ] || exit 91\n",
                "[ \"${UNRELATED+x}\" != x ] || exit 92\n",
                "if IFS= read -r input; then exit 93; fi\n",
                "printf '%s\\n' --call-- \"$@\" >> \"$HOME/argv\" || exit 94\n",
                "exec /bin/cat \"$HOME/response.json\"\n",
            )
            .as_bytes(),
            true,
            0o700,
        )
        .unwrap();
        let mut fixture = Self {
            root,
            candidate,
            bin,
            cwd,
            home,
            selection: Selection {
                schema_version: 1,
                repository: "owner/repo".into(),
                artifact_id: 123,
                build_run_id: 789,
                ci_run_id: 789,
                run_attempt: 1,
                revision: REVISION.into(),
                artifact_zip_bytes: 0,
                zip_sha256: String::new(),
            },
            claim: json!({"schemaVersion":1,"repository":"owner/repo","revision":REVISION,
                "runId":789,"runAttempt":1,"workflow":WORKFLOW,"ref":REFERENCE,
                "event":"push","platform":"linux/amd64","imageId":image_id,
                "securityPassed":true,"fileDigests":{}}),
            response: Vec::new(),
        };
        fixture.rebuild().unwrap();
        fixture
    }
    pub(super) fn rewrite(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        if !BOUND
            .into_iter()
            .chain([
                "release-predicate.json",
                "release-attestation.jsonl",
                "artifact.zip",
            ])
            .any(|allowed| allowed == name)
        {
            return Err(io::Error::other("closed fixture name"));
        }
        write(&self.candidate.join(name), bytes, false, 0o600)
    }
    pub(super) fn rebuild(&mut self) -> io::Result<()> {
        let mut security: Value =
            serde_json::from_slice(&read(&self.candidate.join("image-security.json"))?)
                .map_err(io::Error::other)?;
        for (field, name) in [
            ("sbomSha256", "sbom.spdx.json"),
            ("runtimeProofSha256", "runtime-proof.json"),
            ("vexSha256", "vex.openvex.json"),
        ] {
            security[field] = checksum(&read(&self.candidate.join(name))?).into();
        }
        self.rewrite("image-security.json", &encoded(&security))?;
        let mut digests = BTreeMap::new();
        for name in BOUND {
            digests.insert(name, checksum(&read(&self.candidate.join(name))?));
        }
        self.claim["runId"] = self.selection.ci_run_id.into();
        self.claim["runAttempt"] = self.selection.run_attempt.into();
        self.claim["fileDigests"] = json!(digests);
        self.rewrite("release-predicate.json", &encoded(&self.claim))?;
        let mut zip = rawzip::ZipArchiveWriter::new(Vec::new());
        for name in BOUND
            .into_iter()
            .chain(["release-predicate.json", "release-attestation.jsonl"])
        {
            let data = read(&self.candidate.join(name))?;
            let (mut entry, config) = zip
                .new_file(name)
                .unix_permissions(0o600)
                .start()
                .map_err(io::Error::other)?;
            let mut writer = config.wrap(&mut entry);
            writer.write_all(&data)?;
            let (_, descriptor) = writer.finish().map_err(io::Error::other)?;
            entry.finish(descriptor).map_err(io::Error::other)?;
        }
        let zip = zip.finish().map_err(io::Error::other)?;
        self.rewrite("artifact.zip", &zip)?;
        self.selection.artifact_zip_bytes = zip.len() as u64;
        self.selection.zip_sha256 = checksum(&zip);
        let identity = format!("https://github.com/owner/repo/{WORKFLOW}@{REFERENCE}");
        let certificate = json!({
            "subjectAlternativeName":identity,
            "issuer":ISSUER,"buildSignerDigest":REVISION,
            "sourceRepositoryURI":"https://github.com/owner/repo",
            "sourceRepositoryDigest":REVISION,"sourceRepositoryRef":REFERENCE,
            "runInvocationURI":format!("https://github.com/owner/repo/actions/runs/{}/attempts/{}",
                self.selection.ci_run_id,self.selection.run_attempt),
            "runnerEnvironment":"github-hosted","buildTrigger":"push",
        });
        self.response = encoded(&json!([{"verificationResult":{
            "signature":{"certificate":certificate},
            "verifiedTimestamps":[{"type":"Tlog","timestamp":"2050-01-01T00:00:00Z"}],
            "statement":{"_type":"https://in-toto.io/Statement/v1",
                "predicateType":PREDICATE_TYPE,"predicate":self.claim,
                "subject":SUBJECTS.into_iter().map(|name| json!({"name":name,
                    "digest":{"sha256":digests[name]}})).collect::<Vec<_>>()}
        }}]));
        write(
            &self.home.join("response.json"),
            &self.response,
            false,
            0o600,
        )
    }
    pub(super) fn snapshot(&self) -> io::Result<BTreeMap<String, (u64, String, u32)>> {
        let mut pins = BTreeMap::new();
        for entry in fs::read_dir(&self.candidate)? {
            let path = entry?.path();
            let data = read(&path)?;
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| io::Error::other("fixture member name"))?
                .to_owned();
            pins.insert(
                name,
                (
                    data.len() as u64,
                    checksum(&data),
                    path.metadata()?.mode() & 0o7777,
                ),
            );
        }
        Ok(pins)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
