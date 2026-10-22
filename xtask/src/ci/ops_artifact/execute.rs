//! Observed fixed invocations and unsigned assembly. This is neither a trusted
//! workflow receipt nor complete build-script/toolchain/host-runtime containment.

use super::{
    Clock, Deadline, GRACE, LOG_LIMIT, Observation, failure,
    materials::Materials,
    namespace::{self, Budget, OwnedRun},
    recipe::{Recipe, Tools},
    request::Request,
    resolution, sha,
};
use crate::{
    ci::{
        bounded_log::Log,
        command::{Command, Program},
        process::Executor,
    },
    tooling::Result,
};
use rustix::fs::{Mode, OFlags};
use serde::Serialize;
use simplestchat_ops::distribution::{
    self,
    manifest::{
        ArtifactManifest,
        assembly::{EvidenceSources, prepare_artifact},
    },
};
use std::{collections::BTreeMap, fs::File, sync::Arc, time::Duration};

fn encoded(value: &impl Serialize) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|_| failure(1))?;
    bytes.push(b'\n');
    if bytes.len() > 1024 * 1024 {
        return Err(failure(1));
    }
    Ok(bytes)
}
async fn command<E: Executor, C: Clock>(
    executor: &mut E,
    run: &OwnedRun,
    recipe: &Recipe,
    tools: &Tools,
    deadline: &Deadline<'_, C>,
    mut command: Command,
    maximum: Duration,
) -> Result<Vec<u8>> {
    deadline.check(false)?;
    executor.ready().await?;
    recipe.check(run, tools)?;
    let _reservation = run.budget().reserve(16)?;
    command.timeout = deadline.body(maximum)?;
    command.grace = GRACE;
    let output = executor.run(command).await?;
    deadline.check(false)?;
    executor.ready().await?;
    recipe.check(run, tools)?;
    if !executor.cleanup_passed() {
        return Err(failure(1));
    }
    Ok(output)
}
fn selected(tools: &Tools, rustc: bool, args: &[&str]) -> Command {
    let mut value = Command::capture(
        if rustc {
            Program::OpsRustc
        } else {
            Program::OpsCargo
        },
        args,
    );
    value.ops = Some(if rustc {
        tools.rustc.clone()
    } else {
        tools.cargo.clone()
    });
    value
}
struct Measured {
    source: super::materials::Identity,
    kit: super::materials::Identity,
    recipe: serde_json::Value,
    toolchain: serde_json::Value,
    resolution: resolution::Resolution,
    log: serde_json::Value,
}
async fn build<E: Executor, C: Clock>(
    request: &Request,
    run: &OwnedRun,
    materials: &Materials,
    recipe: &Recipe,
    tools: &Tools,
    executor: &mut E,
    deadline: &Deadline<'_, C>,
) -> Result<Measured> {
    let mut probes = Vec::new();
    for (rustc, args) in [(false, vec!["--version", "--verbose"]), (true, vec!["-vV"])] {
        let mut value = selected(tools, rustc, &args);
        value.capture = Some(16 * 1024);
        let output = command(
            executor,
            run,
            recipe,
            tools,
            deadline,
            value,
            Duration::from_secs(30),
        )
        .await?;
        probes.push(resolution::version(&output, !rustc)?);
        run.private_record(
            if rustc {
                "rustc-version.txt"
            } else {
                "cargo-version.txt"
            },
            &output,
        )?;
    }
    let mut value = selected(tools, true, &["--print", "sysroot"]);
    value.capture = Some(4096);
    let output = command(
        executor,
        run,
        recipe,
        tools,
        deadline,
        value,
        Duration::from_secs(30),
    )
    .await?;
    let sysroot = resolution::sysroot(&output, run)?;
    run.private_record("rustc-sysroot.txt", &output)?;
    let mut value = selected(tools, false, &[]);
    value.arguments = Recipe::arguments(run, true);
    value.capture = Some(16 * 1024 * 1024);
    let bytes = command(
        executor,
        run,
        recipe,
        tools,
        deadline,
        value,
        Duration::from_secs(90),
    )
    .await?;
    let identity = serde_json::to_value(request.selection.identity()).map_err(|_| failure(1))?;
    let resolution = resolution::metadata(
        &bytes,
        run,
        materials,
        identity["packageVersion"]
            .as_str()
            .ok_or_else(|| failure(1))?,
    )?;
    run.private_record("cargo-metadata.json", &bytes)?;
    // Creation and the shared logger's pipes/stdio fall within the same reserved
    // sixteen-descriptor allowance, held until Native finishes the whole log.
    let reservation = run.budget().reserve(16)?;
    deadline.check(false)?;
    executor.ready().await?;
    recipe.check(run, tools)?;
    let body = deadline.body(Duration::from_secs(18 * 60))?;
    let file = rustix::fs::openat(
        run.file(),
        "build.log",
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )
    .map(File::from)
    .map_err(|_| failure(1))?;
    let stamp = match file.metadata() {
        Ok(value) => value,
        Err(_) => {
            let _ = nix::unistd::close(file);
            return Err(failure(1));
        }
    };
    let log = Log::start(file, LOG_LIMIT)?;
    let mut value = selected(tools, false, &[]);
    value.arguments = Recipe::arguments(run, false);
    value.capture = None;
    value.combined_log = Some(Arc::new(log));
    value.timeout = body;
    value.grace = GRACE;
    // Native owns the admitted sink even when setup/spawn fails. Group cleanup
    // precedes bounded EOF and explicit log closes; it never executes candidate.
    executor.run(value).await?;
    drop(reservation);
    deadline.check(false)?;
    executor.ready().await?;
    recipe.check(run, tools)?;
    if !executor.cleanup_passed() {
        return Err(failure(1));
    }
    let log = namespace::read_bound(
        &run.path.join("build.log"),
        LOG_LIMIT,
        run.budget(),
        Some(&stamp),
    )?;
    materials.verify(run, executor, deadline).await?;
    Ok(Measured {
        source: materials.source_identity.clone(),
        kit: materials.kit_identity.clone(),
        recipe: recipe.projection(),
        toolchain: serde_json::json!({"schemaVersion":1,
            "kind":"ops-producer-tool-observations","selectedTools":tools.identities,
            "versionProbes":probes,"sysroot":sysroot,
            "authentication":"callerPrerequisite","runtimeClosure":"unproven"}),
        resolution,
        log: serde_json::json!({"sha256":sha(&log),"bytes":log.len()}),
    })
}

pub(super) async fn produce<E: Executor, C: Clock>(
    request: &Request,
    run: &OwnedRun,
    executor: &mut E,
    deadline: &Deadline<'_, C>,
) -> Result<Observation> {
    let materials = Materials::stage(request, run, executor, deadline).await?;
    // Copy-time entries alone cannot establish complete destination membership:
    // independently enumerate/hash the actual staged trees before any tool.
    materials.verify(run, executor, deadline).await?;
    let tools = Tools::open(run, &materials)?;
    let recipe = Recipe::create(run, &tools)?;
    let measured = build(
        request, run, &materials, &recipe, &tools, executor, deadline,
    )
    .await;
    // Retire every source, config and selected-tool owner before reserving the
    // existing Ops library's bounded descriptor allowance. Attempt all closes.
    let config_closed = recipe.finish();
    let tools_closed = tools.finish();
    let inputs_closed = materials.finish();
    let measured = measured?;
    config_closed?;
    tools_closed?;
    inputs_closed?;
    deadline.check(true)?;
    executor.ready().await?;
    run.check()?;
    let binary = run
        .path
        .join(format!("target/{}/release/simplestchat-ops", super::TARGET));
    let executable = {
        let _library = run.budget().reserve(100)?;
        distribution::inspect(&binary).map_err(|_| failure(1))?
    };
    let mut documents = BTreeMap::new();
    documents.insert(
        "producer-materials.json",
        encoded(&serde_json::json!({"schemaVersion":1,
        "profile":"ops-producer-material-observations-v1","source":measured.source,
        "builderKit":measured.kit,"authentication":"callerPrerequisite",
        "compilerIoClosure":"unproven"}))?,
    );
    documents.insert("producer-recipe.json", encoded(&measured.recipe)?);
    documents.insert("producer-toolchain.json", encoded(&measured.toolchain)?);
    documents.insert("producer-resolution.json", encoded(&measured.resolution)?);
    documents.insert(
        "producer-outcome.json",
        encoded(&serde_json::json!({"schemaVersion":1,
        "kind":"ops-cargo-observation","requestSha256":request.sha256,
        "requestBytes":request.bytes,"commandStatus":0,"cargoAndProbeCliGroupsSettled":true,
        "log":measured.log,"sourceConfigAndToolOwnersClosed":true,
        "scope":"observedCliGroupsAndInputsOnlyNotSeparatedChildrenOrHostRuntime"}))?,
    );
    let mut paths = BTreeMap::new();
    let mut evidence = BTreeMap::new();
    for (name, bytes) in documents {
        run.record(name, &bytes)?;
        paths.insert(name.to_owned(), run.path.join(name));
        evidence.insert(name.to_owned(), sha(&bytes));
    }
    let mut value = serde_json::to_value(request.selection.identity()).map_err(|_| failure(1))?;
    let object = value.as_object_mut().ok_or_else(|| failure(1))?;
    object.insert("schemaVersion".into(), 1.into());
    object.insert("artifactType".into(), "simplestchat-ops".into());
    object.insert(
        "binary".into(),
        serde_json::json!({"name":"simplestchat-ops",
        "sha256":executable.sha256,"bytes":executable.bytes}),
    );
    object.insert(
        "inputDigests".into(),
        serde_json::json!({"source-tree/frame-v1":
        measured.source.sha256,"builder-kit/frame-v1":measured.kit.sha256}),
    );
    object.insert(
        "evidenceDigests".into(),
        serde_json::to_value(evidence).map_err(|_| failure(1))?,
    );
    let manifest = ArtifactManifest::decode(&encoded(&value)?).map_err(|_| failure(1))?;
    run.record("ops-release.json", manifest.as_json())?;
    deadline.check(true)?;
    executor.ready().await?;
    run.check()?;
    let artifact = {
        let _library = run.budget().reserve(100)?;
        prepare_artifact(
            &manifest,
            &binary,
            &request.selection,
            &EvidenceSources::new(paths).map_err(|_| failure(1))?,
            &run.path.join("unsigned"),
        )
        .map_err(|_| failure(1))?
    };
    deadline.check(true)?;
    executor.ready().await?;
    run.check()?;
    Ok(Observation {
        schema_version: 1,
        kind: "ops-unsigned-candidate-observation",
        request_sha256: request.sha256.clone(),
        request_bytes: request.bytes,
        source: measured.source,
        builder_kit: measured.kit,
        artifact,
        claims: "localObservationsNotAuthenticatedBuildMuslOrRuntimeProof",
    })
}

/// Native admission preserves a single signal registration through all owners
/// and closes. A known nonzero primary always wins over later close failures.
pub(super) async fn complete<E: Executor, C: Clock>(
    result: Result<Observation>,
    run: OwnedRun,
    executor: &mut E,
    deadline: &Deadline<'_, C>,
) -> Result<()> {
    let budget = run.budget().clone();
    let closed = run.finish();
    let value = result?;
    closed?;
    deadline.check(true)?;
    executor.ready().await?;
    if !executor.cleanup_passed() {
        return Err(failure(1));
    }
    let bytes = encoded(&value)?;
    let _replay = budget.reserve(16)?;
    executor.replay(&bytes, false, deadline.end()).await?;
    if !executor.cleanup_passed() {
        return Err(failure(1));
    }
    executor.ready().await?;
    deadline.check(true)
}
pub(super) async fn native(path: &std::path::Path, selected: &str) -> Result<()> {
    use crate::{ci::process::Native, owned_process::Signals};
    let mut signals = Signals::new()?;
    let clock = super::NativeClock;
    let deadline = Deadline::new(&clock);
    let budget = Budget::default();
    if let Some(code) = signals.pending().await {
        return Err(failure(code));
    }
    let request = Request::read(path, selected, &budget)?;
    deadline.check(false)?;
    if let Some(code) = signals.pending().await {
        return Err(failure(code));
    }
    let host = rustix::system::uname();
    if host.sysname() != c"Linux" || host.machine() != c"x86_64" {
        return Err(failure(1));
    }
    let run = OwnedRun::create(&request.output, &budget)?;
    let cwd = run.path.clone();
    let environment = Recipe::environment(&run)?;
    let mut executor = Native::new(&cwd, &environment, signals);
    let result = produce(&request, &run, &mut executor, &deadline).await;
    complete(result, run, &mut executor, &deadline).await
}
