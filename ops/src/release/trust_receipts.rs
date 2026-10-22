//! Pure preflight producer conformance and the controller's redacted receiver
//! projection. A decoded `settled`/`finalized` bit does not prove any write/fsync;
//! the operation owner must establish persistence and current process success.

use super::*;
pub(super) const BASE: [&str; 10] = [
    "bounded_process.py",
    "release-public.py",
    "release_public.py",
    "release_artifact.py",
    "release_assets.py",
    "browser_assets.py",
    "release_json.py",
    "runtime_profile.py",
    "reboot-public.py",
    "reboot_public.py",
];
pub(super) const FETCH: [&str; 3] = [
    "fetch-release.py",
    "release_fetch_receiver.py",
    "release_trust.py",
];
const RECEIPT_KEYS: [&str; 16] = [
    "schemaVersion",
    "status",
    "passed",
    "settled",
    "phase",
    "failureClass",
    "evidence",
    "revision",
    "artifactId",
    "zipSha256",
    "artifactZipBytes",
    "downloadedBytes",
    "archiveSha256",
    "manifestSha256",
    "elapsedSeconds",
    "finalized",
];

#[derive(Clone, Debug, Serialize)]
#[serde(transparent)]
pub struct HelperSet(BTreeMap<String, String>);
impl HelperSet {
    /// The exact current Python helper-set policy. This validates source hash
    /// declarations only; it never reads or executes an installed helper.
    pub fn decode(bytes: &[u8]) -> Result<Document<Self>, Error> {
        require(
            bytes.len() <= 4 * 4096 + 4 && json::character_count(bytes, 4 * 4096 + 4)? <= 4096,
            Failure::Request,
        )?;
        let value = json::decode_limited(bytes, 4 * 4096 + 4)?;
        let values = value.object().ok_or(Error::Policy(Failure::HelperSet))?;
        let mut fetch = BASE.to_vec();
        fetch.extend(FETCH);
        let mut force = BASE.to_vec();
        force.push("force_release.py");
        require(
            !value.truthiness()
                || exact(values, &BASE)
                || exact(values, &fetch)
                || exact(values, &force),
            Failure::HelperSet,
        )?;
        require(values.values().all(digest), Failure::HelperDigest)?;
        Ok(Document::new(
            Self(
                values
                    .iter()
                    .map(|(k, v)| (k.clone(), v.string().unwrap().into()))
                    .collect(),
            ),
            bytes,
        ))
    }
    pub fn prepared(&self) -> bool {
        !self.0.is_empty()
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreflightReceipt {
    schema_version: u8,
    run_id: String,
    prepared: bool,
}
impl PreflightReceipt {
    /// Validate the producer's exact three-field output against the selected
    /// helper request. This is stronger than Ansible's current two-field assert;
    /// it does not prove root/OS/path checks, a fresh invocation, or a held lock.
    pub fn decode(bytes: &[u8], helpers: &HelperSet) -> Result<Document<Self>, Error> {
        let value = json::decode_limited(bytes, MAX_LINE)?;
        let value = value
            .object()
            .ok_or(Error::Policy(Failure::PreflightReceipt))?;
        let run = string(value, "runId").unwrap_or("");
        require(
            exact(value, &["schemaVersion", "runId", "prepared"])
                && integer(value, "schemaVersion") == Some(1)
                && hex(run, 32)
                && boolean(value, "prepared") == Some(helpers.prepared()),
            Failure::PreflightReceipt,
        )?;
        Ok(Document::new(
            Self {
                schema_version: 1,
                run_id: run.into(),
                prepared: helpers.prepared(),
            },
            bytes,
        ))
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct FinalizedJournal {
    #[serde(rename = "schemaVersion")]
    schema_version: u8,
    finalized: bool,
}
impl FinalizedJournal {
    /// The JSON condition in `finished_journal`. Unknown fields are accepted and
    /// omitted from this projection. This does not prove file ownership or fsync.
    pub fn decode(bytes: &[u8]) -> Result<Document<Self>, Error> {
        let value = json::decode_limited(bytes, MAX_LINE)?;
        let condition = value.object().is_some()
            && value.get("schemaVersion").and_then(Json::integer) == Some(1)
            && value.get("finalized").and_then(Json::boolean) == Some(true);
        require(condition, Failure::Journal)?;
        Ok(Document::new(
            Self {
                schema_version: 1,
                finalized: true,
            },
            bytes,
        ))
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
enum Seconds {
    Integer(i64),
    Float(f64),
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiverReceipt {
    schema_version: u8,
    status: String,
    passed: bool,
    settled: bool,
    phase: String,
    failure_class: Option<String>,
    evidence: Option<String>,
    revision: String,
    artifact_id: i64,
    zip_sha256: String,
    artifact_zip_bytes: i64,
    downloaded_bytes: i64,
    archive_sha256: Option<String>,
    manifest_sha256: Option<String>,
    elapsed_seconds: Seconds,
    finalized: bool,
}
fn optional_string(value: &BTreeMap<String, Json>, key: &str) -> Option<String> {
    string(value, key).map(str::to_owned)
}
fn null(value: Option<&Json>) -> bool {
    value.is_none_or(|v| matches!(v, Json::Null))
}
fn lower_word(value: Option<&str>, maximum: usize) -> bool {
    value.is_some_and(|v| {
        !v.is_empty()
            && v.len() <= maximum
            && v.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
    })
}
impl ReceiverReceipt {
    /// Read a local receipt snapshot and validate it against the supplied
    /// envelope snapshot. No process result or durable-write action is observed.
    pub fn read(path: &Path, envelope: &Envelope) -> Result<Document<Self>, Error> {
        let bytes = files::read(path, 65536).map_err(|_| Error::Policy(Failure::Receipt))?;
        Self::decode(&bytes, envelope)
    }
    /// Match `release_fetch_controller.verified_receipt`, including field types,
    /// exact selected identities, partial failures and success/digest consistency.
    /// Unknown source fields stay private in Document; Serialize emits only the
    /// established sixteen allowed fields. A success-shaped file alone has no authority.
    pub fn decode(bytes: &[u8], envelope: &Envelope) -> Result<Document<Self>, Error> {
        let value = json::decode_limited(bytes, 65536)?;
        let value = value.object().ok_or(Error::Policy(Failure::Receipt))?;
        require(
            integer(value, "schemaVersion") == Some(1)
                && matches!(string(value, "status"), Some("complete" | "failed"))
                && ["passed", "settled", "finalized"]
                    .iter()
                    .all(|key| boolean(value, key).is_some()),
            Failure::Receipt,
        )?;
        require(
            string(value, "revision") == Some(&envelope.selection.revision)
                && integer(value, "artifactId") == Some(envelope.artifact_id)
                && string(value, "zipSha256") == Some(&envelope.zip_sha256)
                && integer(value, "artifactZipBytes") == Some(envelope.artifact_zip_bytes),
            Failure::Identity,
        )?;
        let passed = boolean(value, "passed").unwrap();
        let settled = boolean(value, "settled").unwrap();
        let finalized = boolean(value, "finalized").unwrap();
        let evidence = optional_string(value, "evidence");
        if null(value.get("evidence")) {
            require(
                !passed
                    && !settled
                    && !finalized
                    && string(value, "phase") == Some("preflight")
                    && value
                        .get("downloadedBytes")
                        .is_some_and(|v| v.semantic_eq(&Json::Integer("0".into())))
                    && null(value.get("archiveSha256"))
                    && null(value.get("manifestSha256")),
                Failure::Evidence,
            )?;
        } else {
            let prefix = format!(
                "/srv/simplestchat-public/releases/{}/download.",
                envelope.selection.revision
            );
            require(
                evidence
                    .as_deref()
                    .and_then(|v| v.strip_prefix(&prefix))
                    .is_some_and(|suffix| {
                        !suffix.is_empty()
                            && suffix
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                    }),
                Failure::Evidence,
            )?;
        }
        let elapsed = value.get("elapsedSeconds").and_then(Json::number);
        require(
            elapsed.is_some_and(|v| (0.0..=330.0).contains(&v)),
            Failure::Timing,
        )?;
        let downloaded = integer(value, "downloadedBytes");
        require(
            downloaded.is_some_and(|v| (0..=envelope.artifact_zip_bytes).contains(&v)),
            Failure::ByteCount,
        )?;
        for key in ["archiveSha256", "manifestSha256"] {
            require(
                null(value.get(key)) || value.get(key).is_some_and(digest),
                Failure::Digest,
            )?;
        }
        require(lower_word(string(value, "phase"), 40), Failure::Phase)?;
        require(
            null(value.get("failureClass")) || lower_word(string(value, "failureClass"), 80),
            Failure::FailureClass,
        )?;
        if passed {
            let prefix = string(value, "status") == Some("complete")
                && settled
                && finalized
                && null(value.get("failureClass"))
                && string(value, "phase") == Some("complete")
                && downloaded == Some(envelope.artifact_zip_bytes);
            let consistent = prefix
                && value
                    .get("archiveSha256")
                    .ok_or(Error::Policy(Failure::Receipt))?
                    .string()
                    == envelope.attestation.predicate.file_digest("image.tar")
                && value
                    .get("manifestSha256")
                    .ok_or(Error::Policy(Failure::Receipt))?
                    .string()
                    == envelope.attestation.predicate.file_digest("release.json");
            require(consistent, Failure::Success)?;
        } else {
            require(
                string(value, "status") == Some("failed") && !null(value.get("failureClass")),
                Failure::Failed,
            )?;
        }
        // Missing optional fields can pass Python's .get checks but are still
        // refused by its final explicit projection. Never invent their values.
        require(
            RECEIPT_KEYS.iter().all(|key| value.contains_key(*key)),
            Failure::Receipt,
        )?;
        let elapsed_seconds = if let Some(seconds) = integer(value, "elapsedSeconds") {
            Seconds::Integer(seconds)
        } else {
            Seconds::Float(elapsed.unwrap())
        };
        Ok(Document::new(
            Self {
                schema_version: 1,
                status: string(value, "status").unwrap().into(),
                passed,
                settled,
                phase: string(value, "phase").unwrap().into(),
                failure_class: optional_string(value, "failureClass"),
                evidence,
                revision: envelope.selection.revision.clone(),
                artifact_id: envelope.artifact_id,
                zip_sha256: envelope.zip_sha256.clone(),
                artifact_zip_bytes: envelope.artifact_zip_bytes,
                downloaded_bytes: downloaded.unwrap(),
                archive_sha256: optional_string(value, "archiveSha256"),
                manifest_sha256: optional_string(value, "manifestSha256"),
                elapsed_seconds,
                finalized,
            },
            bytes,
        ))
    }
    /// A pure consistency result, still requiring the owning transport's zero
    /// exit status, observed identity and durable publication before reuse.
    pub fn completed(&self) -> bool {
        self.passed && self.settled && self.finalized
    }
}
