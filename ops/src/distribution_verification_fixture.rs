//! Synthetic caller-supplied verifier output, not signed or verified material.
use super::*;
use crate::{
    distribution::{
        fixture as elf,
        manifest::{TARGET, digest},
    },
    test_support::Fixture,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs};

pub(super) struct Graph {
    pub root: Fixture,
    pub selection: Selection,
    pub records: BTreeMap<String, Vec<u8>>,
}
pub(super) fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}
impl Graph {
    pub fn new(evidence: usize) -> Self {
        Self::with_run(evidence, 123, 2)
    }
    pub fn with_run(evidence: usize, run: i64, attempt: i64) -> Self {
        let root = Fixture::new();
        let binary = elf::fixture(true, false, false);
        fs::write(root.0.join("binary"), &binary).unwrap();
        let source = json!({"repository":REPOSITORY,"revision":"a".repeat(40),
            "packageVersion":"0.1.0","protocolVersion":1,"target":TARGET,
            "buildRunId":run,"runAttempt":attempt});
        let selection = Selection::decode(&bytes(&source)).unwrap();
        let mut manifest = source;
        let fields = manifest.as_object_mut().unwrap();
        fields.insert("schemaVersion".into(), json!(1));
        fields.insert("artifactType".into(), json!("simplestchat-ops"));
        fields.insert(
            "binary".into(),
            json!({"name":"simplestchat-ops",
            "sha256":digest(&binary),"bytes":binary.len()}),
        );
        fields.insert(
            "inputDigests".into(),
            json!({"PRIVATE-INPUT/source.json":"b".repeat(64)}),
        );
        let mut claims = serde_json::Map::new();
        let mut sources = BTreeMap::new();
        for n in 0..evidence {
            let data = format!("PRIVATE-EVIDENCE-{n}");
            let logical = format!("PRIVATE-LOGICAL/{n:02}.json");
            let path = root.0.join(format!("private-input-{n}"));
            fs::write(&path, &data).unwrap();
            sources.insert(logical.clone(), path);
            claims.insert(logical, json!(digest(data.as_bytes())));
        }
        fields.insert("evidenceDigests".into(), Value::Object(claims));
        // Preserve original noncanonical bytes through the borrowed manifest.
        let mut original = b"\xef\xbb\xbf".to_vec();
        original.extend(serde_json::to_vec_pretty(&manifest).unwrap());
        original.extend(b"\n\n");
        let manifest = ArtifactManifest::decode(&original).unwrap();
        let output = root.0.join("assembly");
        assembly::prepare_artifact(
            &manifest,
            &root.0.join("binary"),
            &selection,
            &assembly::EvidenceSources::new(sources).unwrap(),
            &output,
        )
        .unwrap();
        let records = fs::read_dir(output)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.file_name().into_string().unwrap(),
                    fs::read(entry.path()).unwrap(),
                )
            })
            .collect();
        Self {
            root,
            selection,
            records,
        }
    }
    pub fn value(&self, name: &str) -> Value {
        serde_json::from_slice(&self.records[name]).unwrap()
    }
    pub fn replace(&mut self, name: &str, value: &Value) {
        self.records.insert(name.into(), bytes(value));
    }
    pub fn metadata(&self) -> Result<RetainedMetadata, Failure> {
        RetainedMetadata::decode(
            &self.selection,
            &self.records["ops-release.json"],
            &self.records["ops-evidence.json"],
            &self.records["ops-predicate.json"],
            &self.records["ops-preparation.json"],
        )
    }
    pub fn repository() -> Value {
        json!({"id":REPOSITORY_ID,"full_name":REPOSITORY,"fork":false,
            "owner":{"id":OWNER_ID,"login":OWNER},"private":false,
            "unused":"PRIVATE-API-IGNORED"})
    }
    pub fn attempt(&self) -> Value {
        let source = &self.selection.0;
        json!({"id":source.build_run_id,"run_attempt":source.run_attempt,
            "head_sha":source.revision,"head_branch":"main","name":"CI",
            "path":WORKFLOW,"event":"push","status":"completed","conclusion":"success",
            "repository":Self::repository(),"head_repository":Self::repository()})
    }
    pub fn verifier(&self) -> Value {
        let source = &self.selection.0;
        let prep = digest(&self.records["ops-preparation.json"]);
        let manifest = digest(&self.records["ops-release.json"]);
        let binary = digest(&self.records["simplestchat-ops"]);
        json!([{"attestation":{"UNTRUSTED":"PRIVATE-NOT-AUTHORITY"},
            "verificationResult":{
                "mediaType":RESULT_MEDIA,
                "signature":{"certificate":{
                    "certificateIssuer":"PRIVATE-DN-NOT-OIDC-AUTHORITY",
                    "subjectAlternativeName":CERTIFICATE_IDENTITY,"issuer":OIDC_ISSUER,
                    "sourceRepositoryIdentifier":REPOSITORY_ID.to_string(),
                    "sourceRepositoryOwnerIdentifier":OWNER_ID.to_string(),
                    "sourceRepositoryURI":REPOSITORY_URI,"sourceRepositoryOwnerURI":OWNER_URI,
                    "sourceRepositoryDigest":source.revision,"sourceRepositoryRef":SOURCE_REF,
                    "buildSignerDigest":source.revision,"buildTrigger":"push",
                    "runnerEnvironment":"github-hosted","runInvocationURI":format!(
                        "{REPOSITORY_URI}/actions/runs/{}/attempts/{}",
                        source.build_run_id,source.run_attempt)}},
                "verifiedTimestamps":[{"type":"TimestampAuthority",
                    "uri":"https://timestamp.example.invalid",
                    "timestamp":"2026-10-22T00:00:00Z"}],
                "statement":{"_type":STATEMENT_TYPE,"predicateType":assembly::PREDICATE_TYPE,
                    "subject":[
                        {"name":"ops-preparation.json","digest":{"sha256":prep}},
                        {"name":"ops-release.json","digest":{"sha256":manifest}},
                        {"name":"simplestchat-ops","digest":{"sha256":binary}}],
                    "predicate":self.value("ops-predicate.json")}}}])
    }
    pub fn matched(
        &self,
        repository: &Value,
        attempt: &Value,
        output: &Value,
    ) -> Result<MatchedVerifierPolicy, Failure> {
        self.metadata()?.interpret_successful_verifier_output(
            &bytes(repository),
            &bytes(attempt),
            &bytes(output),
        )
    }
}
