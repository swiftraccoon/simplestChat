//! Persist creation intent, then inspect exact identities before owned removal.

use super::{
    artifacts,
    engine::Engine,
    files, json,
    process::{Limits, Request},
};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Deserialize, Serialize)]
struct Container {
    name: String,
    image: String,
    id: Option<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Journal {
    schema_version: u32,
    run_id: String,
    containers: Vec<Container>,
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
    fn journal(&self) -> Result<Journal> {
        let path = self.directory.join("ownership.json");
        if !path.try_exists()? {
            return Ok(Journal {
                schema_version: 1,
                run_id: self.run_id.clone(),
                containers: Vec::new(),
            });
        }
        let journal: Journal =
            serde_json::from_slice(&artifacts::read_regular(&path, 1024 * 1024)?)?;
        ensure!(
            journal.schema_version == 1 && journal.run_id == self.run_id,
            "Capacity ownership journal belongs to another run"
        );
        Ok(journal)
    }
    pub(super) fn remember(&self, name: &str, image: &str, id: &str) -> Result<()> {
        ensure!(
            name.starts_with("capacity-") && name.ends_with(&self.run_id)
                || name.starts_with("capacity-") && name.contains(&format!("-{}-", self.run_id)),
            "invalid owned container name"
        );
        ensure!(
            id.is_empty() || valid_id(id),
            "engine returned an invalid container ID"
        );
        let mut journal = self.journal()?;
        journal
            .containers
            .retain(|container| container.name != name);
        journal.containers.push(Container {
            name: name.into(),
            image: image.into(),
            id: (!id.is_empty()).then(|| id.into()),
        });
        files::replace(&self.directory, "ownership.json", &json::bytes(&journal)?)
    }
    pub(super) async fn resolve(&self, engine: &dyn Engine, name: &str) -> Result<Option<String>> {
        let journal = self.journal()?;
        let Some(owned) = journal.containers.iter().find(|entry| entry.name == name) else {
            return Ok(None);
        };
        let identity = owned.id.as_deref().unwrap_or(name);
        let output = engine
            .call(
                &["inspect".into(), identity.into()],
                Request {
                    cleanup: true,
                    limits: Limits {
                        timeout: Duration::from_secs(20),
                        ..Limits::default()
                    },
                    ..Request::default()
                },
            )
            .await?;
        if output.status != 0 {
            // A failed lookup is not proof of absence; retain the journal and
            // report cleanup uncertainty rather than remove an unchecked name.
            let text = String::from_utf8_lossy(&output.stderr).to_lowercase();
            ensure!(
                text.contains("no such container") || text.contains("no such object"),
                "owned container inspection failed"
            );
            return Ok(None);
        }
        let values: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout)?;
        ensure!(values.len() == 1, "owned container inspection is ambiguous");
        let value = &values[0];
        let id = value["Id"].as_str().unwrap_or("");
        ensure!(
            valid_id(id)
                && owned.id.as_ref().is_none_or(|expected| expected == id)
                && value["Name"]
                    .as_str()
                    .is_some_and(|actual| actual.trim_start_matches('/') == owned.name)
                && value["Config"]["Image"].as_str() == Some(owned.image.as_str())
                && value["Config"]["Labels"]["simplestchat.capacity.run"].as_str()
                    == Some(self.run_id.as_str()),
            "owned container identity mismatch"
        );
        Ok(Some(id.into()))
    }
    pub(super) async fn remove(&self, engine: &dyn Engine, name: &str) -> Result<()> {
        let Some(id) = self.resolve(engine, name).await? else {
            return Ok(());
        };
        let output = engine
            .call(
                &["rm".into(), "--force".into(), "--volumes".into(), id],
                Request {
                    cleanup: true,
                    limits: Limits {
                        timeout: Duration::from_secs(60),
                        ..Limits::default()
                    },
                    ..Request::default()
                },
            )
            .await?;
        ensure!(output.status == 0, "owned container removal failed");
        Ok(())
    }
}
fn valid_id(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
