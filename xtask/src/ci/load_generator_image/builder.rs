//! Exact private builder/node, engine container and labeled state-volume owner.

use super::{
    engine::{self, Clock, Core},
    files::{Area, Owned},
    plan::{self, failure},
};
use crate::{ci::process::Executor, tooling::Result};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct BuilderRow {
    name: String,
    driver: String,
    nodes: Vec<Node>,
    #[serde(default)]
    err: Option<serde_json::Value>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Node {
    name: String,
    endpoint: String,
    #[serde(default)]
    driver_opts: BTreeMap<String, String>,
    #[serde(default)]
    err: Option<serde_json::Value>,
}
#[derive(Clone, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase")]
struct Volume {
    name: String,
    driver: String,
    scope: String,
    mountpoint: String,
    created_at: String,
    labels: Option<BTreeMap<String, String>>,
    options: Option<BTreeMap<String, String>>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Container {
    id: String,
    name: String,
    image: String,
    config: Config,
    mounts: Vec<Mount>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Config {
    image: String,
    env: Vec<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Mount {
    #[serde(rename = "Type")]
    kind: String,
    name: String,
    destination: String,
    #[serde(rename = "RW")]
    rw: bool,
    source: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListedContainer {
    id: String,
    name: String,
}
fn no_error(error: &Option<serde_json::Value>) -> bool {
    matches!(error, None | Some(serde_json::Value::Null))
        || error.as_ref().and_then(|v| v.as_str()) == Some("")
}
fn one(bytes: &[u8]) -> Result<Option<&str>> {
    if bytes.is_empty() {
        return Ok(None);
    }
    engine::line(bytes, engine::Scope::LoadImage).map(Some)
}
pub(super) struct Builder {
    pub name: String,
    pub node: String,
    pub container: String,
    pub volume: String,
    token: String,
    volume_attempted: bool,
    volume_created: bool,
    builder_attempted: bool,
    builder_created: bool,
    pub bootstrap_attempted: bool,
    id: Option<String>,
    image: Option<String>,
    stamp: Option<Volume>,
}
impl Builder {
    pub fn new(token: &str) -> Self {
        let name = format!("sc-load-{token}");
        let node = format!("{name}-0");
        let container = format!("buildx_buildkit_{node}");
        let volume = format!("{container}_state");
        Self {
            name,
            node,
            container,
            volume,
            token: token.into(),
            volume_attempted: false,
            volume_created: false,
            builder_attempted: false,
            builder_created: false,
            bootstrap_attempted: false,
            id: None,
            image: None,
            stamp: None,
        }
    }
    async fn definition<E: Executor, C: Clock>(
        &self,
        core: &mut Core<'_, E, C>,
        cleanup: bool,
    ) -> Result<bool> {
        // Buildx v0.29.1 commands/ls.go: JSON emits one row per builder, unlike
        // a custom .Builder template which also repeats it on node rows.
        let bytes = core
            .run(core.command(&["buildx", "ls", "--no-trunc", "--format", "json"], cleanup))
            .await?;
        let text = std::str::from_utf8(&bytes).map_err(|_| failure())?;
        let mut found = false;
        for (index, line) in text.lines().enumerate() {
            if index >= 16 {
                return Err(failure());
            }
            let row: BuilderRow = serde_json::from_str(line).map_err(|_| failure())?;
            if row.name == "default" {
                continue;
            }
            if row.name != self.name
                || found
                || row.driver != "docker-container"
                || row.nodes.len() != 1
                || !no_error(&row.err)
            {
                return Err(failure());
            }
            let node = &row.nodes[0];
            if node.name != self.node
                || node.endpoint != core.endpoint
                || !no_error(&node.err)
                || node.driver_opts.len() != 1
                || node.driver_opts.get(&format!("env.{}", plan::MARKER)) != Some(&self.token)
            {
                return Err(failure());
            }
            found = true;
        }
        Ok(found)
    }
    async fn container_lookup<E: Executor, C: Clock>(
        &self,
        core: &mut Core<'_, E, C>,
        cleanup: bool,
    ) -> Result<Option<String>> {
        let bytes = core
            .run(core.command(
                &[
                    "container",
                    "ls",
                    "--all",
                    "--no-trunc",
                    "--format",
                    r#"{"id":{{json .ID}},"name":{{json .Names}}}"#,
                ],
                cleanup,
            ))
            .await?;
        let mut matched = None;
        let mut ids = BTreeSet::new();
        let mut names = BTreeSet::new();
        for (index, line) in std::str::from_utf8(&bytes)
            .map_err(|_| failure())?
            .lines()
            .enumerate()
        {
            if index >= 256 {
                return Err(failure());
            }
            let row: ListedContainer = serde_json::from_str(line).map_err(|_| failure())?;
            // Docker CLI v28.5.1 formatter/container.go Names joins the full
            // slash-prefix-stripped names with commas when --no-trunc is set.
            if !engine::container_id(&row.id)
                || row.name.is_empty()
                || row.name.len() > 4096
                || !ids.insert(row.id.clone())
            {
                return Err(failure());
            }
            let mut named = false;
            for name in row.name.split(',') {
                if name.is_empty()
                    || name.len() > 256
                    || !name.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-' | b'/')
                    })
                    || !names.insert(name.to_owned())
                {
                    return Err(failure());
                }
                named |= name == self.container;
            }
            if named || self.id.as_deref() == Some(row.id.as_str()) {
                if matched.is_some() {
                    return Err(failure());
                }
                matched = Some(row.id);
            }
        }
        Ok(matched)
    }
    async fn volume_lookup<E: Executor, C: Clock>(
        &self,
        core: &mut Core<'_, E, C>,
        cleanup: bool,
    ) -> Result<bool> {
        let filter = format!("name=^{}$", self.volume);
        let bytes = core
            .run(core.command(
                &["volume", "ls", "--filter", &filter, "--format", "{{.Name}}"],
                cleanup,
            ))
            .await?;
        match one(&bytes)? {
            None => Ok(false),
            Some(name) if name == self.volume => Ok(true),
            _ => Err(failure()),
        }
    }
    async fn inspect_volume<E: Executor, C: Clock>(
        &mut self,
        core: &mut Core<'_, E, C>,
        cleanup: bool,
    ) -> Result<()> {
        let bytes = core
            .run(core.command(
                &["volume", "inspect", "--format", "{{json .}}", &self.volume],
                cleanup,
            ))
            .await?;
        let value: Volume = serde_json::from_slice(&bytes).map_err(|_| failure())?;
        if value.name != self.volume
            || value.driver != "local"
            || value.scope != "local"
            || !engine::safe_path(std::path::Path::new(&value.mountpoint))
            || value.created_at.len() > 128
            || chrono::DateTime::parse_from_rfc3339(&value.created_at).is_err()
            || value.labels.as_ref().and_then(|v| v.get(plan::OWNER)) != Some(&self.token)
            || value.options.as_ref().is_some_and(|v| !v.is_empty())
            || self.stamp.as_ref().is_some_and(|old| old != &value)
        {
            return Err(failure());
        }
        self.stamp = Some(value);
        Ok(())
    }
    async fn inspect_container<E: Executor, C: Clock>(
        &mut self,
        core: &mut Core<'_, E, C>,
        id: &str,
        cleanup: bool,
    ) -> Result<()> {
        if !engine::container_id(id) || self.id.as_deref().is_some_and(|old| old != id) {
            return Err(failure());
        }
        let bytes = core
            .run(core.command(
                &["container", "inspect", "--format", "{{json .}}", id],
                cleanup,
            ))
            .await?;
        let value: Container = serde_json::from_slice(&bytes).map_err(|_| failure())?;
        let expected = format!("{}={}", plan::MARKER, self.token);
        if value.id != id
            || value.name != format!("/{}", self.container)
            || !engine::image_id(&value.image)
            || self
                .image
                .as_ref()
                .is_some_and(|image| image != &value.image)
            || value.config.image != "moby/buildkit:buildx-stable-1"
            || value
                .config
                .env
                .iter()
                .filter(|s| s.starts_with(&format!("{}=", plan::MARKER)))
                .collect::<Vec<_>>()
                != [&expected]
            || value.mounts.len() != 1
        {
            return Err(failure());
        }
        let mount = &value.mounts[0];
        if mount.kind != "volume"
            || mount.name != self.volume
            || mount.destination != "/var/lib/buildkit"
            || !mount.rw
            || self
                .stamp
                .as_ref()
                .is_none_or(|volume| mount.source != volume.mountpoint)
        {
            return Err(failure());
        }
        self.id = Some(id.into());
        self.image = Some(value.image);
        Ok(())
    }
    async fn references<E: Executor, C: Clock>(
        &self,
        core: &mut Core<'_, E, C>,
        expected: Option<&str>,
    ) -> Result<()> {
        let filter = format!("volume={}", self.volume);
        let bytes = core
            .run(core.command(
                &[
                    "container",
                    "ls",
                    "--all",
                    "--no-trunc",
                    "--filter",
                    &filter,
                    "--format",
                    "{{.ID}}",
                ],
                true,
            ))
            .await?;
        if one(&bytes)? != expected {
            return Err(failure());
        }
        Ok(())
    }
    pub async fn create<E: Executor, C: Clock>(
        &mut self,
        core: &mut Core<'_, E, C>,
        files: &Owned,
    ) -> Result<()> {
        if self.definition(core, false).await?
            || self.container_lookup(core, false).await?.is_some()
            || self.volume_lookup(core, false).await?
        {
            return Err(failure());
        }
        let intent = serde_json::to_vec(&serde_json::json!({"schemaVersion":1,
            "state":"creation-planned","builder":self.name,"node":self.node,
            "container":self.container,"volume":self.volume,"owner":self.token,
            "engineSha256":core.identity_sha256(),"transportSha256":core.endpoint_sha256()}))
        .map_err(|_| failure())?;
        files.write(Area::Work, "ownership.json", &intent)?;
        self.volume_attempted = true;
        let label = format!("{}={}", plan::OWNER, self.token);
        let bytes = core
            .run(core.command(
                &[
                    "volume",
                    "create",
                    "--driver",
                    "local",
                    "--label",
                    &label,
                    &self.volume,
                ],
                false,
            ))
            .await?;
        if one(&bytes)? != Some(self.volume.as_str()) {
            return Err(failure());
        }
        self.volume_created = true;
        self.inspect_volume(core, false).await?;
        self.builder_attempted = true;
        let marker = format!("env.{}={}", plan::MARKER, self.token);
        let bytes = core
            .run(core.command(
                &[
                    "buildx",
                    "create",
                    "--name",
                    &self.name,
                    "--node",
                    &self.node,
                    "--driver",
                    "docker-container",
                    "--driver-opt",
                    &marker,
                    "--use",
                    core.endpoint,
                ],
                false,
            ))
            .await?;
        if one(&bytes)? != Some(self.name.as_str()) {
            return Err(failure());
        }
        self.builder_created = true;
        if !self.definition(core, false).await? {
            return Err(failure());
        }
        Ok(())
    }
    pub async fn retire<E: Executor, C: Clock>(&mut self, core: &mut Core<'_, E, C>) -> Result<()> {
        if !self.volume_attempted && !self.builder_attempted {
            return core.check(true).await;
        }
        let definition = self.definition(core, true).await;
        let lookup = self.container_lookup(core, true).await;
        let volume = self.inspect_volume(core, true).await;
        let first = definition
            .as_ref()
            .map(|_| ())
            .map_err(|e| crate::tooling::Failure::new(e.code, "Load image retirement failed."));
        let lookup_status = lookup
            .as_ref()
            .map(|_| ())
            .map_err(|e| crate::tooling::Failure::new(e.code, "Load image retirement failed."));
        let container = match &lookup {
            Ok(Some(id)) => self.inspect_container(core, id, true).await,
            Ok(None) if !self.bootstrap_attempted || self.id.is_some() => Ok(()),
            _ => Err(failure()),
        };
        let expected = lookup.as_ref().ok().and_then(|v| v.as_deref());
        let references = self.references(core, expected).await;
        let removed = if container.is_ok() {
            match expected {
                Some(id) => core
                    .run(core.command(&["container", "rm", "--force", "--volumes", id], true))
                    .await
                    .map(|_| ()),
                None => Ok(()),
            }
        } else {
            Err(failure())
        };
        let absent = self
            .container_lookup(core, true)
            .await
            .and_then(|id| if id.is_none() { Ok(()) } else { Err(failure()) });
        let unchanged = self.inspect_volume(core, true).await;
        let unreferenced = self.references(core, None).await;
        let deleted = if volume.is_ok() && unchanged.is_ok() && unreferenced.is_ok() {
            core.run(core.command(&["volume", "rm", &self.volume], true))
                .await
                .map(|_| ())
        } else {
            Err(failure())
        };
        let volume_absent = self.volume_lookup(core, true).await.and_then(|found| {
            if !found && (self.volume_created || self.stamp.is_some()) {
                Ok(())
            } else {
                Err(failure())
            }
        });
        let forgotten = match definition {
            Ok(true) if container.is_ok() && absent.is_ok() && volume_absent.is_ok() => core
                .run(core.command(
                    &[
                        "buildx",
                        "rm",
                        "--force",
                        "--keep-daemon",
                        "--keep-state",
                        &self.name,
                    ],
                    true,
                ))
                .await
                .map(|_| ()),
            Ok(false) if self.builder_created || !self.builder_attempted => Ok(()),
            _ => Err(failure()),
        };
        let builder_absent = self
            .definition(core, true)
            .await
            .and_then(|found| if !found { Ok(()) } else { Err(failure()) });
        let checked = core.check(true).await;
        first
            .and(lookup_status)
            .and(volume)
            .and(container)
            .and(references)
            .and(removed)
            .and(absent)
            .and(unchanged)
            .and(unreferenced)
            .and(deleted)
            .and(volume_absent)
            .and(forgotten)
            .and(builder_absent)
            .and(checked)
    }
}
