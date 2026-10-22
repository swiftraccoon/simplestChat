//! Build and verify one frozen source revision; cleanup precedes publication.

use super::{
    files::{self, EnvironmentFile},
    options::Context,
    plan::{self, Command, Program},
    process::Executor,
};
use crate::tooling::{Failure, Result};
use serde::Deserialize;
use std::collections::BTreeMap;

fn trimmed(bytes: &[u8]) -> Result<&str> {
    std::str::from_utf8(bytes)
        .map(|text| text.trim_end_matches('\n'))
        .map_err(|_| files::failure("Invalid production image command output."))
}

async fn source(context: &Context, runner: &mut impl Executor) -> Result<()> {
    let revision = runner
        .run(Command::capture(Program::Git, &["rev-parse", "HEAD"]))
        .await?;
    if trimmed(&revision)? != context.revision {
        return Err(files::failure(
            "Production image checkout does not match GITHUB_SHA.",
        ));
    }
    let status = runner
        .run(Command::capture(
            Program::Git,
            &["status", "--porcelain=v1", "--untracked-files=all"],
        ))
        .await?;
    if !status.is_empty() {
        return Err(files::failure("Production image checkout must be clean."));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Inspection {
    id: String,
    config: Configuration,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Configuration {
    user: String,
    labels: BTreeMap<String, String>,
    env: Vec<String>,
}

async fn inspect(context: &Context, runner: &mut impl Executor, id: &str) -> Result<Inspection> {
    let bytes = runner
        .run(Command::capture(
            Program::Docker,
            &["image", "inspect", "--format", "{{json .}}", id],
        ))
        .await?;
    let image: Inspection = serde_json::from_slice(&bytes)
        .map_err(|_| files::failure("Production image inspection invalid."))?;
    let expected = format!("SOURCE_REVISION={}", context.revision);
    if image.id != id
        || image.config.labels.get("org.opencontainers.image.revision") != Some(&context.revision)
        || image
            .config
            .env
            .iter()
            .filter(|entry| entry.starts_with("SOURCE_REVISION="))
            .collect::<Vec<_>>()
            != [&expected]
    {
        return Err(files::failure(
            "Production image revision or immutable identity mismatch.",
        ));
    }
    Ok(image)
}

pub(super) async fn build(context: &Context, runner: &mut impl Executor) -> Result<()> {
    let output = EnvironmentFile::open(context)?;
    source(context, runner).await?;
    files::absent(&context.temporary.join("production.iid"))?;
    if !context.cache_hit {
        files::absent(&context.temporary.join("buildx-cache-next"))?;
    }
    let cache_from = files::cache_from(context)?;
    let owned = files::intent(context)?;
    let mut create_attempted = false;
    let primary: Result<String> = async {
        let driver = if context.local_arm {
            files::new_file(
                &owned.directory.join("Dockerfile"),
                plan::ARM_DOCKERFILE.as_bytes(),
            )?;
            runner.run(plan::arm(&owned.directory)).await?;
            let image = files::image_id(&owned.directory.join("image.iid"))?;
            let platform = runner
                .run(Command::capture(
                    Program::Docker,
                    &[
                        "image",
                        "inspect",
                        "--format",
                        "{{.Os}}/{{.Architecture}}",
                        &image,
                    ],
                ))
                .await?;
            if trimmed(&platform)? != "linux/arm64" {
                return Err(files::failure("Native BuildKit image must be Linux ARM64."));
            }
            Some(image)
        } else {
            None
        };
        create_attempted = true;
        runner
            .run(plan::create(&owned.name, driver.as_deref()))
            .await?;
        runner
            .run(plan::build(context, &owned.name, cache_from))
            .await?;
        let image = files::image_id(&context.temporary.join("production.iid"))?;
        inspect(context, runner, &image).await?;
        Ok(image)
    }
    .await;
    // The unique cryptographic name belongs to the pre-create intent journal,
    // including an ambiguous failed create. No fixed/foreign builder is removed.
    let cleaned = if create_attempted {
        runner.run(plan::remove(&owned.name)).await.map(|_| ())
    } else {
        Ok(())
    };
    let image = primary?;
    cleaned?;
    source(context, runner).await?;
    runner.ready().await?;
    if !context.cache_hit {
        files::promote_cache(context)?;
    }
    runner.ready().await?;
    output.publish(&image)
}

fn migrations(context: &Context) -> Result<Vec<String>> {
    let entries = std::fs::read_dir(context.root.join("migrations"))
        .map_err(|_| files::failure("Production migration inventory unavailable."))?;
    let mut names = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|_| files::failure("Production migration inventory unavailable."))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| files::failure("Production migration name invalid."))?;
        if name.ends_with(".sql") {
            names.push(name);
        }
    }
    names.sort();
    if names.is_empty() {
        return Err(Failure::new(1, "Production migration inventory is empty."));
    }
    Ok(names)
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Container {
    id: String,
    name: String,
    image: String,
    config: ContainerConfiguration,
    state: ContainerState,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ContainerConfiguration {
    labels: BTreeMap<String, String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ContainerState {
    running: bool,
    status: String,
    exit_code: i32,
}

fn container_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn container_command(args: &[&str], cleanup: bool) -> Command {
    let mut command = Command::capture(Program::Docker, args);
    command.cleanup = cleanup;
    command
}

async fn owned_container(
    context: &Context,
    runner: &mut impl Executor,
    owned: &files::Owned,
    id: &str,
    cleanup: bool,
) -> Result<Container> {
    if !container_id(id) {
        return Err(files::failure("Verification container ID invalid."));
    }
    let bytes = runner
        .run(container_command(
            &["container", "inspect", "--format", "{{json .}}", id],
            cleanup,
        ))
        .await?;
    let container: Container = serde_json::from_slice(&bytes)
        .map_err(|_| files::failure("Verification container inspection invalid."))?;
    if container.id != id
        || container.name != format!("/{}", owned.name)
        || container.image != context.image
        || container.config.labels.get(plan::OWNER_LABEL) != Some(&owned.name)
    {
        return Err(files::failure("Verification container ownership mismatch."));
    }
    let record = owned.directory.join("container.id");
    if record.exists() {
        let existing = files::image_id(&record)?;
        if existing != format!("sha256:{id}") {
            return Err(files::failure("Verification container identity changed."));
        }
    } else {
        files::new_file(&record, format!("sha256:{id}\n").as_bytes())?;
    }
    Ok(container)
}

async fn retire_verification(
    context: &Context,
    runner: &mut impl Executor,
    owned: &files::Owned,
) -> Result<()> {
    // The exact fresh name locates a possibly committed create with a lost CLI
    // response. Inspect its immutable ID, image and owner label before removal.
    let record = owned.directory.join("container.id");
    let filter = if record.exists() {
        let recorded = files::image_id(&record)?;
        format!(
            "id={}",
            recorded.strip_prefix("sha256:").expect("validated digest")
        )
    } else {
        format!("name=^/{}$", owned.name)
    };
    let args = [
        "container",
        "ls",
        "--all",
        "--no-trunc",
        "--filter",
        &filter,
        "--format",
        "{{.ID}}",
    ];
    let bytes = runner.run(container_command(&args, true)).await?;
    let id = trimmed(&bytes)?;
    if id.is_empty() {
        return Ok(());
    }
    owned_container(context, runner, owned, id, true).await?;
    runner
        .run(container_command(&["container", "rm", "--force", id], true))
        .await?;
    let remaining = runner.run(container_command(&args, true)).await?;
    if !remaining.is_empty() {
        return Err(files::failure("Verification container cleanup incomplete."));
    }
    Ok(())
}

pub(super) async fn verify(context: &Context, runner: &mut impl Executor) -> Result<()> {
    source(context, runner).await?;
    let image = inspect(context, runner, &context.image).await?;
    if image.config.user != "10001:10001" {
        return Err(files::failure(
            "Production image must configure user10001:10001.",
        ));
    }
    let names = migrations(context)?;
    let owned = files::verification_intent(context)?;
    let primary: Result<()> = async {
        let bytes = runner
            .run(plan::verify(&context.image, &names, &owned.name))
            .await?;
        let id = trimmed(&bytes)?;
        owned_container(context, runner, &owned, id, false).await?;
        let mut start = container_command(&["container", "start", "--attach", id], false);
        start.timeout = std::time::Duration::from_secs(120);
        runner.run(start).await?;
        let container = owned_container(context, runner, &owned, id, false).await?;
        if container.state.running
            || container.state.status != "exited"
            || container.state.exit_code != 0
        {
            return Err(files::failure(
                "Production image guest verification did not succeed.",
            ));
        }
        Ok(())
    }
    .await;
    let cleaned = retire_verification(context, runner, &owned).await;
    primary?;
    cleaned?;
    source(context, runner).await?;
    runner.ready().await
}
