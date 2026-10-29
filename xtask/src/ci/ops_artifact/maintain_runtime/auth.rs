//! Current native verifier children and opaque custody of their same retained inputs.
//!
//! CLI clocks remain independent. The outer setup ceiling clips every child and
//! settlement; it does not assert that a CLI's worst-case debt fits setup510.

use super::{
    Error, Result,
    control::Control,
    decode,
    files::{self, Directory, Original},
    options::{Options, REPOSITORY},
    process::Runner,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

const SERVER_NAMES: [&str; 11] = [
    "artifact.zip",
    "image.tar",
    "release.json",
    "outcome.json",
    "source.json",
    "sbom.spdx.json",
    "image-security.json",
    "runtime-proof.json",
    "vex.openvex.json",
    "release-predicate.json",
    "release-attestation.jsonl",
];
pub(super) fn encoded(value: &Value) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| Error::Input)?;
    bytes.push(b'\n');
    Ok(bytes)
}
pub(super) fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}
struct Tree {
    directory: Directory,
    files: BTreeMap<String, Original>,
}
impl Tree {
    fn open(path: &Path, server: bool, control: &mut Control) -> Result<Self> {
        let directory = Directory::open(path, control)?;
        let mut tree = Self {
            directory,
            files: BTreeMap::new(),
        };
        let admitted = (|| {
            let names = tree.directory.names(control)?;
            if server {
                let mut expected = SERVER_NAMES.map(str::to_string).to_vec();
                expected.sort();
                if names != expected {
                    return Err(Error::Refused);
                }
            } else {
                let fixed = [
                    "simplestchat-ops",
                    "ops-release.json",
                    "ops-evidence.json",
                    "ops-predicate.json",
                    "ops-preparation.json",
                    "ops-attestation.jsonl",
                ];
                if names.len() != 15
                    || fixed.iter().any(|name| !names.iter().any(|x| x == name))
                    || names.iter().any(|name| {
                        !fixed.contains(&name.as_str())
                            && !(0..16).any(|index| name == &format!("ops-evidence-{index:02}.bin"))
                    })
                {
                    return Err(Error::Refused);
                }
            }
            for name in names {
                let maximum = if server {
                    if ["image.tar", "artifact.zip"].contains(&name.as_str()) {
                        2 * 1024 * 1024 * 1024
                    } else {
                        64 * 1024 * 1024
                    }
                } else {
                    ops_bound(&name)?
                };
                let original = Original::open(&path.join(&name), maximum, control)?;
                tree.files.insert(name, original);
            }
            tree.check(control)
        })();
        if let Err(error) = admitted {
            control.record(error);
            let _ = tree.finish(control);
            return Err(error);
        }
        Ok(tree)
    }
    fn check(&self, control: &mut Control) -> Result<()> {
        let names = self.files.keys().cloned().collect::<Vec<_>>();
        if self.directory.names(control)? != names {
            return Err(Error::Changed);
        }
        for original in self.files.values() {
            original.check(control)?;
        }
        self.directory.check(control)
    }
    fn finish(&mut self, control: &mut Control) -> Result<()> {
        let mut first = self.check(control).err();
        for original in self.files.values_mut() {
            if let Err(error) = original.finish(control) {
                first.get_or_insert(error);
            }
        }
        if let Err(error) = self.directory.finish(control) {
            first.get_or_insert(error);
        }
        first.map_or(Ok(()), Err)
    }
}
/// Constructed only after this invocation's current acquire+retained verifier children.
pub(super) struct AuthenticatedOps {
    tree: Tree,
    pub selection: Value,
}
impl AuthenticatedOps {
    pub fn pin(&self, name: &str) -> Result<&Original> {
        self.tree.files.get(name).ok_or(Error::Input)
    }
    pub fn check(&self, control: &mut Control) -> Result<()> {
        self.tree.check(control)
    }
    pub fn finish(&mut self, control: &mut Control) -> Result<()> {
        self.tree.finish(control)
    }
}
/// Current cached4 must finish on the same eleven originals before this owner exists.
pub(super) struct AuthenticatedServer {
    tree: Tree,
}
impl AuthenticatedServer {
    pub fn path(&self) -> &Path {
        &self.tree.directory.path
    }
    pub fn pin(&self, name: &str) -> Result<&Original> {
        self.tree.files.get(name).ok_or(Error::Input)
    }
    pub fn check(&self, control: &mut Control) -> Result<()> {
        self.tree.check(control)
    }
    pub fn finish(&mut self, control: &mut Control) -> Result<()> {
        self.tree.finish(control)
    }
}
async fn request(
    runner: &mut Runner,
    control: &mut Control,
    role: &str,
    operation: &str,
    value: Value,
    report: &mut Value,
) -> Result<()> {
    let path = runner.output.join(format!("{role}.json"));
    files::write(&path, &encoded(&value)?, control)?;
    let mut original = Original::open(&path, 131072, control)?;
    let mut argv = args(&["ops-artifact", operation, "--request"]);
    argv.extend([
        path.into_os_string(),
        "--request-sha256".into(),
        original.sha256.clone().into(),
    ]);
    let delivered = runner
        .credential(control, role, "controller", argv, Duration::from_secs(300))
        .await;
    if operation == "acquire" && delivered.as_ref().is_ok_and(|value| value.status == 0) {
        report["opsVerifierCalls"] = 3.into();
    }
    let result = delivered.and_then(|observed| {
        if observed.status == 0 {
            original.check(control)
        } else {
            Err(Error::Child(observed.status))
        }
    });
    if let Err(error) = result {
        control.record(error);
    }
    let closed = original.finish(control);
    result.and(closed)
}
pub(super) async fn ops(
    runner: &mut Runner,
    control: &mut Control,
    options: &Options,
    report: &mut Value,
) -> Result<AuthenticatedOps> {
    let manifest = runner.source("Cargo.toml", control)?;
    let text = std::str::from_utf8(&manifest).map_err(|_| Error::Input)?;
    let package: toml::Value = toml::from_str(text).map_err(|_| Error::Input)?;
    let version = package
        .get("workspace")
        .and_then(|value| value.get("package"))
        .and_then(|value| value.get("version"))
        .and_then(toml::Value::as_str)
        .ok_or(Error::Input)?;
    let selection = json!({"repository":REPOSITORY,"revision":options.revision,
        "packageVersion":version,"target":"x86_64-unknown-linux-musl","protocolVersion":1,
        "buildRunId":options.run,"runAttempt":options.attempt});
    let output = runner.output.join("ops-acquired");
    request(
        runner,
        control,
        "authenticate-ops",
        "acquire",
        json!({"schemaVersion":1,
        "selection":selection,"artifactId":options.artifact,"output":output,
        "authentication":"gh-token"}),
        report,
    )
    .await?;
    let mut tree = Tree::open(&output.join("input"), false, control)?;
    let verified = async {
        tree.check(control)?;
        request(
            runner,
            control,
            "authenticate-retained-ops",
            "verify-retained",
            json!({
            "schemaVersion":1,"selection":selection,"directory":tree.directory.path,
            "output":runner.output.join("ops-reverified"),"authentication":"gh-token"}),
            report,
        )
        .await?;
        tree.check(control)?;
        runner.integrity(control)?;
        control.check()
    }
    .await;
    if let Err(error) = verified {
        control.record(error);
        let _ = tree.finish(control);
        return Err(error);
    }
    Ok(AuthenticatedOps { tree, selection })
}
async fn api(
    runner: &mut Runner,
    control: &mut Control,
    role: &str,
    endpoint: String,
) -> Result<Value> {
    let mut argv = args(&[
        "api",
        "--hostname",
        "github.com",
        "-H",
        "Accept: application/vnd.github+json",
        "-H",
        "X-GitHub-Api-Version: 2022-11-28",
    ]);
    argv.push(endpoint.into());
    let observed = runner
        .credential(control, role, "gh", argv, Duration::from_secs(20))
        .await?;
    decode(&observed.stdout)
}
fn current(
    artifact: &Value,
    run: &Value,
    options: &Options,
    id: u64,
    zip: &Original,
) -> Result<Value> {
    let name = format!("simplestchat-production-{}", options.revision);
    let association = &artifact["workflow_run"];
    let repo = association["repository_id"]
        .as_u64()
        .filter(|value| *value > 0 && *value < (1 << 53));
    if artifact["id"].as_u64() != Some(id)
        || artifact["expired"] != false
        || artifact["name"] != name
        || artifact["size_in_bytes"].as_u64() != Some(zip.bytes)
        || artifact["digest"] != format!("sha256:{}", zip.sha256)
        || association["id"].as_u64() != Some(options.run)
        || association["head_sha"] != options.revision
        || repo.is_none()
        || repo != association["head_repository_id"].as_u64()
    {
        return Err(Error::Refused);
    }
    for key in ["repository", "head_repository"] {
        if !run[key]["full_name"]
            .as_str()
            .is_some_and(|value| value.eq_ignore_ascii_case(REPOSITORY))
        {
            return Err(Error::Refused);
        }
    }
    if run["id"].as_u64() != Some(options.run)
        || run["run_attempt"].as_u64() != Some(options.attempt)
        || run["head_branch"] != "main"
        || run["head_sha"] != options.revision
        || run["name"] != "CI"
        || run["path"] != ".github/workflows/ci.yml"
        || run["event"] != "push"
        || run["status"] != "completed"
        || run["conclusion"] != "success"
    {
        return Err(Error::Refused);
    }
    Ok(
        json!({"schemaVersion":1,"repository":REPOSITORY,"artifactId":id,
        "buildRunId":options.run,"ciRunId":options.run,"runAttempt":options.attempt,
        "revision":options.revision,"artifactZipBytes":zip.bytes,"zipSha256":zip.sha256}),
    )
}
/// Locate a candidate across retained attempts. Creation time only selects the
/// download: its signatures, exact attempt and current metadata still authorize it.
pub(super) fn server_artifact(listing: &Value, revision: &str) -> Result<u64> {
    let rows = listing["artifacts"].as_array().ok_or(Error::Refused)?;
    if listing["total_count"].as_u64() != Some(rows.len() as u64) || rows.len() > 100 {
        return Err(Error::Refused);
    }
    let wanted = format!("simplestchat-production-{revision}");
    let mut candidates = rows
        .iter()
        .filter(|row| row["name"] == wanted)
        .map(|row| {
            let timestamp = row["created_at"]
                .as_str()
                .filter(|value| value.len() <= 64)
                .ok_or(Error::Refused)?;
            let created =
                chrono::DateTime::parse_from_rfc3339(timestamp).map_err(|_| Error::Refused)?;
            let id = row["id"]
                .as_u64()
                .filter(|value| *value > 0 && *value < (1 << 53))
                .ok_or(Error::Refused)?;
            Ok((created, id))
        })
        .collect::<Result<Vec<_>>>()?;
    candidates.sort_unstable_by_key(|candidate| std::cmp::Reverse(candidate.0));
    let (newest, older) = candidates.split_first().ok_or(Error::Refused)?;
    if older.first().is_some_and(|next| next.0 == newest.0) {
        return Err(Error::Refused);
    }
    Ok(newest.1)
}

pub(super) async fn server(
    runner: &mut Runner,
    control: &mut Control,
    options: &Options,
    report: &mut Value,
) -> Result<AuthenticatedServer> {
    let listing = api(
        runner,
        control,
        "select-server",
        format!(
            "repos/{REPOSITORY}/actions/runs/{}/artifacts?per_page=100",
            options.run
        ),
    )
    .await?;
    let id = server_artifact(&listing, &options.revision)?;
    server_explicit(runner, control, options, id, false, report).await
}

/// An explicit old selection has its own directory, roles and completed counts.
/// Current metadata and both signature passes still authorize the held bytes.
pub(super) async fn server_explicit(
    runner: &mut Runner,
    control: &mut Control,
    options: &Options,
    id: u64,
    previous: bool,
    report: &mut Value,
) -> Result<AuthenticatedServer> {
    if id == 0
        || id >= (1 << 53)
        || (previous && options.purpose != super::options::Purpose::Deploy)
    {
        return Err(Error::Input);
    }
    let parent = runner.output.join(if previous {
        "previous-acquired"
    } else {
        "server-acquired"
    });
    let role = if previous {
        "previous-server"
    } else {
        "server"
    };
    let deploy = options.purpose == super::options::Purpose::Deploy;
    let completed = if previous {
        "previousServerVerifierCalls"
    } else if deploy {
        "currentServerVerifierCalls"
    } else {
        "serverVerifierCalls"
    };
    let n = if deploy {
        let left = control
            .body_end()
            .saturating_duration_since(std::time::Instant::now())
            .as_secs();
        let clipped = left.saturating_sub(40).min(300);
        if clipped < 100 {
            return Err(Error::Deadline);
        }
        clipped
    } else {
        900
    };
    let mut argv = args(&[
        "release",
        "verify",
        "--repository",
        REPOSITORY,
        "--revision",
    ]);
    argv.extend([
        options.revision.clone().into(),
        "--artifact-id".into(),
        id.to_string().into(),
        "--ci-run".into(),
        options.run.to_string().into(),
        "--output-parent".into(),
        parent.clone().into_os_string(),
        "--timeout-seconds".into(),
        n.to_string().into(),
        "--finalization-seconds".into(),
        "60".into(),
    ]);
    let observed = runner
        .credential(
            control,
            &format!("authenticate-{role}"),
            "ops",
            argv,
            Duration::from_secs(if deploy { n + 10 } else { 300 }),
        )
        .await?;
    report[completed] = 4.into();
    let receipt = decode(&observed.stdout)?;
    super::projection::exact(
        &receipt,
        &["revision", "directory", "image.tar", "release.json"],
    )?;
    let directory = PathBuf::from(receipt["directory"].as_str().ok_or(Error::Refused)?);
    let attempt = directory.parent().ok_or(Error::Refused)?;
    let attempt_name = attempt
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or(Error::Refused)?;
    if receipt["revision"] != options.revision
        || directory.file_name() != Some("artifact".as_ref())
        || attempt.parent() != Some(parent.as_path())
        || !attempt_name
            .strip_prefix("release-verification.")
            .is_some_and(|value| super::options::digest(value, 32))
    {
        return Err(Error::Refused);
    }
    let mut tree = Tree::open(&directory, true, control)?;
    let result = async {
        for name in ["image.tar", "release.json"] {
            if receipt[name] != tree.files[name].sha256 {
                return Err(Error::Refused);
            }
        }
        let predicate = decode(&tree.files["release-predicate.json"].data(1024 * 1024, control)?)?;
        if predicate["runId"].as_u64() != Some(options.run)
            || predicate["runAttempt"].as_u64() != Some(options.attempt)
        {
            return Err(Error::Refused);
        }
        let artifact = api(
            runner,
            control,
            &format!("current-{role}-artifact"),
            format!("repos/{REPOSITORY}/actions/artifacts/{id}"),
        )
        .await?;
        let run = api(
            runner,
            control,
            &format!("current-{role}-run"),
            format!("repos/{REPOSITORY}/actions/runs/{}", options.run),
        )
        .await?;
        let metadata = current(&artifact, &run, options, id, &tree.files["artifact.zip"])?;
        tree.check(control)?;
        // Preserve the reference cached verifier's floor(remaining-40), max900 profile.
        let left = control
            .body_end()
            .saturating_duration_since(std::time::Instant::now())
            .as_secs();
        let n = left.saturating_sub(40).min(900);
        if n < 100 {
            return Err(Error::Deadline);
        }
        let mut argv = args(&["release", "attestation", "verify", "--directory"]);
        argv.extend([
            directory.as_os_str().into(),
            "--selection-json".into(),
            String::from_utf8(encoded(&metadata)?)
                .map_err(|_| Error::Input)?
                .into(),
            "--timeout-seconds".into(),
            n.to_string().into(),
            "--finalization-seconds".into(),
            "60".into(),
        ]);
        let verified = runner
            .credential(
                control,
                &format!("authenticate-retained-{role}"),
                "ops",
                argv,
                Duration::from_secs(n + 10),
            )
            .await?;
        let value = decode(&verified.stdout)?;
        let object = value.as_object().ok_or(Error::Refused)?;
        if object.len() != 10
            || metadata
                .as_object()
                .ok_or(Error::Refused)?
                .iter()
                .any(|(key, expected)| object.get(key) != Some(expected))
        {
            return Err(Error::Refused);
        }
        let proof = &value["attestation"];
        super::projection::exact(proof, &["predicate", "bundleSha256", "verificationSha256"])?;
        if proof["predicate"] != predicate
            || proof["bundleSha256"] != tree.files["release-attestation.jsonl"].sha256
        {
            return Err(Error::Refused);
        }
        verification_hashes(&proof["verificationSha256"])?;
        tree.check(control)?;
        runner.integrity(control)?;
        control.check()?;
        Ok(())
    }
    .await;
    match result {
        Ok(()) => Ok(AuthenticatedServer { tree }),
        Err(error) => {
            control.record(error);
            let _ = tree.finish(control);
            Err(error)
        }
    }
}
fn ops_bound(name: &str) -> Result<u64> {
    if name == "simplestchat-ops" {
        Ok(256 * 1024 * 1024)
    } else if name == "ops-attestation.jsonl" {
        Ok(8 * 1024 * 1024)
    } else if [
        "ops-release.json",
        "ops-evidence.json",
        "ops-predicate.json",
        "ops-preparation.json",
    ]
    .contains(&name)
    {
        Ok(65536)
    } else if (0..16).any(|index| name == format!("ops-evidence-{index:02}.bin")) {
        Ok(1024 * 1024)
    } else {
        Err(Error::Refused)
    }
}
fn verification_hashes(value: &Value) -> Result<()> {
    super::projection::exact(
        value,
        &[
            "image.tar",
            "sbom.spdx.json",
            "runtime-proof.json",
            "vex.openvex.json",
        ],
    )?;
    if value
        .as_object()
        .ok_or(Error::Refused)?
        .values()
        .any(|value| {
            !value
                .as_str()
                .is_some_and(|text| super::options::digest(text, 64))
        })
    {
        return Err(Error::Refused);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_four_subject_verification_shape_is_required_without_extra_fields() {
        let good = json!({"image.tar":"a".repeat(64),"sbom.spdx.json":"b".repeat(64),
            "runtime-proof.json":"c".repeat(64),"vex.openvex.json":"d".repeat(64)});
        assert!(verification_hashes(&good).is_ok());
        assert!(verification_hashes(&json!("a".repeat(64))).is_err());
        let mut extra = good.clone();
        extra["private-secret"] = "SECRET".into();
        let mut missing = good.clone();
        missing.as_object_mut().unwrap().remove("image.tar");
        let mut bad = good;
        bad["sbom.spdx.json"] = "A".repeat(64).into();
        for value in [extra, missing, bad] {
            assert_eq!(verification_hashes(&value), Err(Error::Refused));
        }
    }
    #[test]
    fn every_ops_member_retains_its_exact_frozen_reference_bound() {
        for (name, bytes) in [
            ("simplestchat-ops", 256 * 1024 * 1024),
            ("ops-attestation.jsonl", 8 * 1024 * 1024),
            ("ops-evidence.json", 65536),
            ("ops-release.json", 65536),
            ("ops-predicate.json", 65536),
            ("ops-preparation.json", 65536),
        ] {
            assert_eq!(ops_bound(name), Ok(bytes));
        }
        for index in 0..16 {
            assert_eq!(
                ops_bound(&format!("ops-evidence-{index:02}.bin")),
                Ok(1024 * 1024)
            );
        }
        for name in ["ops-evidence-16.bin", "ops-evidence-1.bin", "foreign.bin"] {
            assert_eq!(ops_bound(name), Err(Error::Refused));
        }
    }
}
