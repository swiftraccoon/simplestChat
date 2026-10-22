//! New-contract fixtures only; no claimed source/provenance is observed.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
pub fn selection() -> Value {
    json!({"repository":"owner/repo","revision":"a".repeat(40),"packageVersion":"0.1.0",
        "protocolVersion":1,"target":"x86_64-unknown-linux-musl","buildRunId":123,
        "runAttempt":1})
}
pub fn manifest(binary: &[u8]) -> Value {
    let mut value = selection();
    let fields = value.as_object_mut().unwrap();
    fields.insert("schemaVersion".into(), json!(1));
    fields.insert("artifactType".into(), json!("simplestchat-ops"));
    fields.insert(
        "binary".into(),
        json!({"name":"simplestchat-ops",
        "sha256":digest(binary),"bytes":binary.len()}),
    );
    fields.insert(
        "inputDigests".into(),
        json!({"PRIVATE-SENTINEL/source.json":"b".repeat(64)}),
    );
    fields.insert("evidenceDigests".into(), json!({}));
    value
}
