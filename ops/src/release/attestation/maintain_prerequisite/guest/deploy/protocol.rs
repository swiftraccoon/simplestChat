//! Closed deploy-only request; admission grants no authentication authority.

use super::*;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Payload {
    #[serde(rename = "release.json")]
    pub manifest: super::super::protocol::Pin,
    #[serde(rename = "image.tar")]
    pub archive: super::super::protocol::Pin,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Request {
    pub schema_version: u64,
    pub scope: String,
    pub run_id: String,
    pub revision: String,
    pub previous_revision: String,
    pub setup_seconds: u64,
    pub body_seconds: u64,
    pub source_pins: BTreeMap<String, String>,
    pub helper_pins: BTreeMap<String, String>,
    pub binary: super::super::protocol::Pin,
    pub current: Payload,
    pub previous: Payload,
    pub source_bundle: super::super::protocol::Pin,
    pub source_tree: String,
    pub engine_version: String,
    pub postgres_image: String,
    pub caddy_image: String,
}
fn pin(value: &super::super::protocol::Pin, maximum: u64) -> Result<()> {
    require(hex(&value.sha256, 64) && (1..=maximum).contains(&value.bytes))
}
impl Request {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        require(bytes.len() <= 16384)?;
        let value = super::super::protocol::decode(bytes)?;
        let request: Self = serde_json::from_value(value).map_err(|_| Error::Input)?;
        require(request.schema_version == 1 && request.scope == "NativeHostDeployRequest")?;
        require(hex(&request.run_id, 32) && hex(&request.revision, 40))?;
        require(
            hex(&request.previous_revision, 40) && request.revision != request.previous_revision,
        )?;
        require((1..=600).contains(&request.setup_seconds))?;
        require((3720..=4320).contains(&request.body_seconds) && hex(&request.source_tree, 40))?;
        require(request.engine_version == "29.8.2")?;
        require(request.caddy_image == super::super::protocol::CADDY_IMAGE)?;
        require(
            request
                .postgres_image
                .strip_prefix("postgres:17-alpine@sha256:")
                .is_some_and(|hash| hex(hash, 64)),
        )?;
        require(
            request.source_pins.keys().cloned().collect::<BTreeSet<_>>()
                == sources::guest_names().into_iter().collect(),
        )?;
        require(
            request.helper_pins.keys().cloned().collect::<BTreeSet<_>>()
                == sources::helper_names().into_iter().collect(),
        )?;
        require(
            request
                .source_pins
                .values()
                .chain(request.helper_pins.values())
                .all(|hash| hex(hash, 64)),
        )?;
        pin(&request.binary, 256 * 1024 * 1024)?;
        pin(&request.source_bundle, 320 * 1024 * 1024)?;
        for payload in [&request.current, &request.previous] {
            pin(&payload.manifest, 1024 * 1024)?;
            pin(&payload.archive, 2 * 1024 * 1024 * 1024)?;
        }
        Ok(request)
    }
    /// Shared owners receive explicit current source identity and old payload identity.
    /// No maintenance wire decoder or maintenance success predicate is involved.
    pub fn adapter(&self) -> super::super::protocol::Request {
        super::super::protocol::Request {
            schema_version: 1,
            scope: self.scope.clone(),
            run_id: self.run_id.clone(),
            revision: self.revision.clone(),
            scenario: "deploy-rollback".into(),
            setup_seconds: self.setup_seconds,
            body_seconds: self.body_seconds,
            end_seconds: self.body_seconds + 30,
            binary: self.binary.clone(),
            manifest: self.previous.manifest.clone(),
            archive: self.previous.archive.clone(),
            source_bundle: self.source_bundle.clone(),
            source_tree: self.source_tree.clone(),
            source_pins: self.source_pins.clone(),
            helper_pins: self.helper_pins.clone(),
            postgres_image: self.postgres_image.clone(),
            caddy_image: self.caddy_image.clone(),
            engine_version: self.engine_version.clone(),
        }
    }
}
