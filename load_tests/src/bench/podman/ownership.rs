//! Exact immutable container identity is checked before every cleanup operation.

use super::engine::{Engine, valid_id, valid_image_id};
use crate::bench::{artifacts, process::Sink};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(super) const LABEL: &str = "simplestchat.benchmark.run";
#[derive(Clone, Deserialize, Serialize)]
struct Entry {
    name: String,
    image: String,
    id: Option<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Journal {
    schema_version: u32,
    run_id: String,
    containers: Vec<Entry>,
}
pub(super) struct Ownership {
    directory: PathBuf,
    run_id: String,
}
impl Ownership {
    pub(super) fn new(directory: &Path, run_id: &str) -> Self {
        Self {
            directory: directory.into(),
            run_id: run_id.into(),
        }
    }
    fn read(&self) -> Result<Journal> {
        let path = self.directory.join("ownership.json");
        if !path.try_exists()? {
            return Ok(Journal {
                schema_version: 1,
                run_id: self.run_id.clone(),
                containers: Vec::new(),
            });
        }
        let journal: Journal = serde_json::from_value(artifacts::read_json(&path, 1024 * 1024)?)?;
        ensure!(
            journal.schema_version == 1 && journal.run_id == self.run_id,
            "Benchmark ownership journal belongs to another run"
        );
        Ok(journal)
    }
    pub(super) fn remember(&self, name: &str, image: &str, id: &str) -> Result<()> {
        ensure!(
            name.starts_with("bench-") && name.ends_with(&self.run_id),
            "Invalid benchmark container name"
        );
        ensure!(
            id.is_empty() || valid_id(id),
            "Engine returned an invalid container ID"
        );
        let mut journal = self.read()?;
        journal.containers.retain(|entry| entry.name != name);
        journal.containers.push(Entry {
            name: name.into(),
            image: image.into(),
            id: (!id.is_empty()).then(|| id.into()),
        });
        artifacts::replace_ownership(&self.directory, &journal)
    }
    pub(super) async fn resolve(&self, engine: &Engine, name: &str) -> Result<Option<String>> {
        let journal = self.read()?;
        let Some(entry) = journal.containers.iter().find(|entry| entry.name == name) else {
            return Ok(None);
        };
        let target = entry.id.as_deref().unwrap_or(name);
        let output = engine
            .call(
                &["inspect".into(), target.into()],
                Duration::from_secs(20),
                2 * 1024 * 1024,
                Sink::Capture,
                Sink::Capture,
                true,
            )
            .await?;
        if output.exit.code != Some(0) || output.exit.signal.is_some() {
            let errors = String::from_utf8_lossy(&output.stderr).to_lowercase();
            ensure!(
                errors.contains("no such container") || errors.contains("no such object"),
                "Owned benchmark container inspection failed"
            );
            return Ok(None);
        }
        let values: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout)?;
        ensure!(
            values.len() == 1,
            "Owned benchmark container inspection is ambiguous"
        );
        let value = &values[0];
        let id = value["Id"].as_str().unwrap_or("");
        ensure!(
            valid_id(id)
                && entry.id.as_ref().is_none_or(|expected| expected == id)
                && value["Name"]
                    .as_str()
                    .is_some_and(|actual| actual.strip_prefix('/').unwrap_or(actual) == entry.name)
                && image_matches(&entry.image, value["Image"].as_str().unwrap_or(""))
                && value["Config"]["Labels"][LABEL].as_str() == Some(self.run_id.as_str()),
            "Owned benchmark container identity mismatch"
        );
        Ok(Some(id.into()))
    }
    pub(super) async fn stop(
        &self,
        engine: &Engine,
        name: &str,
        grace: u64,
    ) -> Result<Option<String>> {
        let Some(id) = self.resolve(engine, name).await? else {
            return Ok(None);
        };
        engine
            .run(
                &[
                    "stop".into(),
                    "--time".into(),
                    grace.to_string(),
                    id.clone(),
                ],
                grace + 30,
                true,
            )
            .await?;
        Ok(Some(id))
    }
    pub(super) async fn remove(&self, engine: &Engine, name: &str) -> Result<()> {
        let Some(id) = self.resolve(engine, name).await? else {
            return Ok(());
        };
        engine
            .run(
                &["rm".into(), "--force".into(), "--volumes".into(), id],
                60,
                true,
            )
            .await?;
        Ok(())
    }
}

// Podman resolves Config.Image to a stored name even when run selects an ID.
// Inspect.Image is RootfsImageID: compare its full hash, never that display name.
// https://github.com/containers/podman/blob/v5.7.1/libpod/container_inspect.go#L140
fn image_matches(expected: &str, observed: &str) -> bool {
    valid_image_id(expected)
        && valid_image_id(observed)
        && expected
            .strip_prefix("sha256:")
            .unwrap_or(expected)
            .eq_ignore_ascii_case(observed.strip_prefix("sha256:").unwrap_or(observed))
}
