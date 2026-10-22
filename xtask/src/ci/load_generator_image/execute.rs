//! Complete recipe, resource settlement, payload proof and publication ordering.

use super::{
    builder::Builder,
    capture::Capture,
    engine::{self, Core, Policy},
    files::{self, Area, Owned, Record},
    hash, image,
    plan::{self, Context},
    tools::Tools,
    transfer,
};
use crate::{
    ci::{
        command::{Command, Program},
        process::Executor,
        release_checks::files::Executable,
    },
    tooling::{Failure, Result},
};
use std::{
    fs::Metadata,
    sync::Arc,
    time::{Duration, Instant},
};

fn deadline(context: &Context) -> Result<()> {
    if context.time.elapsed() >= plan::ENTRY {
        Err(plan::expired())
    } else {
        Ok(())
    }
}
async fn source<E: Executor>(context: &Context, runner: &mut E) -> Result<()> {
    let mut head = Command::capture(Program::Git, &["rev-parse", "HEAD"]);
    head.timeout = Duration::from_secs(5);
    head.grace = Duration::from_secs(1);
    head.capture = Some(64 * 1024);
    let bytes = runner.run(head).await?;
    if engine::line(&bytes, engine::Scope::LoadImage)? != context.revision {
        return Err(plan::failure());
    }
    let mut clean = Command::capture(
        Program::Git,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    );
    clean.timeout = Duration::from_secs(5);
    clean.grace = Duration::from_secs(1);
    clean.capture = Some(64 * 1024);
    if !runner.run(clean).await?.is_empty() {
        return Err(plan::failure());
    }
    Ok(())
}
struct Ready {
    archive: Record,
    sealed: Metadata,
    id: Record,
    selected: String,
}
async fn export<E: Executor>(
    context: &Context,
    files: &Owned,
    core: &mut Core<'_, Capture<'_, E>, plan::Time>,
    zstd: &Arc<Executable>,
) -> Result<Ready> {
    let selected = image::selected(context, core).await?;
    let mut save = core.command(&["save", plan::TAG], false);
    save.timeout = Duration::from_secs(120);
    save.grace = Duration::from_secs(1);
    let (save, raw, diagnostic) =
        transfer::output(files, Area::Work, "image.tar", plan::RAW_LIMIT, save).await?;
    let saved = core.run(save).await;
    let diagnostics = files.read(&diagnostic);
    saved?;
    diagnostics?;
    if image::selected(context, core).await? != selected {
        return Err(plan::failure());
    }
    let identity = hash::archive(context, core.runner, files, &raw, &selected).await?;
    let (raw_hash, raw_bytes, raw_stamp) = hash::measure(context, core.runner, files, &raw).await?;
    if raw_hash != identity.archive_sha256() || raw_bytes != identity.archive_bytes() {
        return Err(plan::failure());
    }
    let (input, observed) = files.input(&raw)?;
    if !files::same(&raw_stamp, &observed) {
        let _ = files::close(input);
        return Err(plan::failure());
    }
    let mut compress = Command::capture(Program::Zstd, &["-T0", "-3", "--stdout"]);
    compress.timeout = Duration::from_secs(120);
    compress.grace = Duration::from_secs(1);
    compress.stdin = Some(Arc::new(input));
    compress.zstd = Some(zstd.clone());
    let (compress, archive, diagnostic) = transfer::output(
        files,
        Area::Output,
        &context.archive_name(),
        plan::COMPRESSED_LIMIT,
        compress,
    )
    .await?;
    let compressed = core.runner.run(compress).await;
    let unchanged = files.check_input(&raw, &raw_stamp);
    let diagnostics = files.read(&diagnostic);
    compressed?;
    unchanged?;
    diagnostics?;
    let (_, _, sealed) = hash::measure(context, core.runner, files, &archive).await?;
    files.remove(&raw)?;
    let (input, observed) = files.input(&archive)?;
    if !files::same(&sealed, &observed) {
        let _ = files::close(input);
        return Err(plan::failure());
    }
    let mut decompress = Command::capture(Program::Zstd, &["--decompress", "--stdout"]);
    decompress.timeout = Duration::from_secs(120);
    decompress.grace = Duration::from_secs(1);
    decompress.stdin = Some(Arc::new(input));
    decompress.zstd = Some(zstd.clone());
    let (decompress, roundtrip, diagnostic) = transfer::output(
        files,
        Area::Work,
        "roundtrip.tar",
        plan::RAW_LIMIT,
        decompress,
    )
    .await?;
    let expanded = core.runner.run(decompress).await;
    let unchanged = files.check_input(&archive, &sealed);
    let diagnostics = files.read(&diagnostic);
    expanded?;
    unchanged?;
    diagnostics?;
    let (sha256, bytes, _) = hash::measure(context, core.runner, files, &roundtrip).await?;
    if sha256 != identity.archive_sha256() || bytes != identity.archive_bytes() {
        return Err(plan::failure());
    }
    files.remove(&roundtrip)?;
    source(context, core.runner).await?;
    core.runner.ready().await?;
    deadline(context)?;
    let evidence = serde_json::to_vec(&identity).map_err(|_| plan::failure())?;
    files.write(Area::Work, "archive-identity.json", &evidence)?;
    let id = files.write(
        Area::Output,
        "image-id.txt",
        format!("{selected}\n").as_bytes(),
    )?;
    Ok(Ready {
        archive,
        sealed,
        id,
        selected,
    })
}
async fn body<E: Executor>(
    context: &Context,
    endpoint: &str,
    files: &Owned,
    runner: &mut E,
    tools: &Tools,
) -> Result<Ready> {
    let zstd = tools.zstd();
    let mut capture = Capture {
        inner: runner,
        files,
        context,
        tools,
    };
    source(context, &mut capture).await?;
    let cache = files.cache()?;
    let mut clock = context.time.clone();
    let mut core = Core::bind(endpoint, &mut capture, &mut clock, Policy::load_image()).await?;
    let mut builder = Builder::new(&files.token);
    let primary = async {
        builder.create(&mut core, files).await?;
        // Build dispatch itself may bootstrap the container. Retain intent even
        // when the command's reply is lost or its process is interrupted.
        builder.bootstrap_attempted = true;
        core.run(context.build(endpoint, &builder.name, cache))
            .await?;
        Ok(())
    }
    .await;
    let pending = core.runner.ready().await;
    let retired = builder.retire(&mut core).await;
    primary.and(pending).and(retired)?;
    core.runner.ready().await?;
    if !core.runner.cleanup_passed() {
        return Err(plan::failure());
    }
    export(context, files, &mut core, &zstd).await
}
pub(super) async fn run<E: Executor>(
    context: &Context,
    endpoint: &str,
    runner: &mut E,
    mut files: Owned,
    tools: Tools,
) -> Result<()> {
    let primary = body(context, endpoint, &files, runner, &tools).await;
    let pending = runner.ready().await;
    let closed = tools.finish();
    let groups = if runner.cleanup_passed() {
        Ok(())
    } else {
        Err(plan::failure())
    };
    let ready = primary.and_then(|value| pending.and(closed).and(groups).map(|()| value));
    let ready = match ready {
        Ok(value) => {
            let replay = runner
                .replay(
                    b"Load image payload and roundtrip verified.\n",
                    false,
                    Instant::now() + Duration::from_secs(25),
                )
                .await;
            replay
                .and(runner.ready().await)
                .and(deadline(context))
                .map(|()| value)
        }
        Err(error) => Err(error),
    };
    let result = match ready {
        Ok(value) => files.publish(&value.archive, &value.sealed, &value.id, &value.selected),
        Err(error) => Err(error),
    };
    let finished = files.finish();
    let pending = runner.ready().await;
    let groups = if runner.cleanup_passed() {
        Ok(())
    } else {
        Err(plan::failure())
    };
    result
        .and(finished)
        .and(pending)
        .and(groups)
        .and(deadline(context))
}
pub(super) fn early(files: Owned, tools: Option<Tools>, error: Failure) -> Result<()> {
    if let Some(tools) = tools {
        let _ = tools.finish();
    }
    let _ = files.finish();
    Err(error)
}
