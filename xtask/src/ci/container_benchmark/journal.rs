//! Protected intent precedes create. A lost daemon response is never absence proof.
use super::{
    config, failure,
    files::{Guard, Output},
    io::Role,
    plan,
};
use crate::{ci::container_engine, tooling::Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Journal {
    pub schema_version: u8,
    pub run_id: String,
    pub output: PathBuf,
    pub endpoint: String,
    pub server_image: String,
    pub generator_image: String,
    pub finalized: bool,
    #[serde(default)]
    pub daemon_sha256: Option<String>,
}
impl Journal {
    pub fn new(
        settings: &config::Settings,
        run: String,
        endpoint: String,
        output: PathBuf,
        daemon_sha256: String,
    ) -> Self {
        Self {
            schema_version: 1,
            run_id: run,
            output,
            endpoint,
            server_image: settings.server_image.clone(),
            generator_image: settings.generator_image.clone(),
            finalized: false,
            daemon_sha256: Some(daemon_sha256),
        }
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > 16384 {
            return Err(failure(1));
        }
        let j: Self = serde_json::from_slice(bytes).map_err(|_| failure(1))?;
        if j.daemon_sha256
            .as_deref()
            .is_some_and(|s| !container_engine::container_id(s))
            || j.schema_version != 1
            || !plan::nonce(&j.run_id)
            || !config::path(&j.output)
            || !container_engine::image_id(&j.server_image)
            || !container_engine::image_id(&j.generator_image)
            || container_engine::endpoint(&j.endpoint, container_engine::Scope::Benchmark).is_err()
        {
            return Err(failure(1));
        }
        Ok(j)
    }
    pub fn image(&self, role: Role) -> &str {
        match role {
            Role::Server => &self.server_image,
            Role::Generator => &self.generator_image,
        }
    }
    pub fn save(&self, guard: &Guard) -> Result<()> {
        guard.save(&serde_json::to_vec(self).map_err(|_| failure(1))?)
    }
    pub fn begin(&self, guard: &Guard, output: &Output) -> Result<()> {
        if let Some(old) = guard.current()?
            && !Self::decode(&old)?.finalized
        {
            return Err(failure(1));
        }
        self.save(guard)?;
        output.json("run.json", self)?;
        output.json(
            "result.json",
            &serde_json::json!({"schemaVersion":1,"completed":false,
            "passed":false,"runId":self.run_id}),
        )
    }
    pub fn preserve_emergency(&self, output: &Output, budget: &super::io::Budget) -> Result<()> {
        let retained = match output.read("result.json", 16384)? {
            Some(bytes) => {
                let name = format!(
                    "result-before-emergency-{}.json",
                    super::files::hex(&super::files::random::<8>(budget)?)
                );
                // Retain exact original bytes before changing the old result.
                output.bytes(&name, &bytes)?;
                Some(name)
            }
            None => None,
        };
        output.json(
            "result.json",
            &serde_json::json!({"schemaVersion":1,"runId":self.run_id,
            "completed":false,"passed":false,"error":"supervisor_or_launcher_interrupted",
            "retainedResult":retained}),
        )
    }
}
#[derive(Clone)]
pub(super) struct Owned {
    pub role: Role,
    pub id: Option<String>,
    pub attempted: bool,
    pub removed: bool,
}
impl Owned {
    pub fn new(role: Role) -> Self {
        Self {
            role,
            id: None,
            attempted: false,
            removed: false,
        }
    }
    pub fn recover(role: Role, output: &Output) -> Result<Self> {
        let attempted = output.attempted(role)?;
        let id = output
            .read(&format!("{}.cid", role.name()), 128)?
            .map(|bytes| {
                let v = container_engine::line(&bytes, container_engine::Scope::Benchmark)?;
                if !container_engine::container_id(v) {
                    return Err(failure(1));
                }
                Ok(v.to_owned())
            })
            .transpose()?;
        if id.is_some() && !attempted {
            return Err(failure(1));
        }
        let removed = output
            .read(&format!("{}.removed", role.name()), 128)?
            .map(|bytes| {
                id.as_ref()
                    .is_some_and(|id| bytes == format!("{id}\n").as_bytes())
            })
            .unwrap_or(false);
        Ok(Self {
            role,
            id,
            attempted,
            removed,
        })
    }
    pub fn id(&self) -> Result<&str> {
        self.id.as_deref().ok_or_else(|| failure(1))
    }
}
