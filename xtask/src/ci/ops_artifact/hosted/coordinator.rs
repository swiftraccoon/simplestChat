//! One registration and executor own the entire hosted preparation. Every
//! borrowed legacy stage keeps its original context and a clipped deadline.

use super::super::{
    Deadline, materials,
    namespace::{self, Budget, Chain, Held, OwnedRun},
};
use super::{
    archive::Graph,
    catalog::{self, Catalog, Format},
    context::{ArtifactKind, Context},
    failure, fetch,
    files::Tree,
    kit, native_build,
    plan::{self, Plan},
    progress::{Checkpoint, Edge, MaterialKind, Phase, Progress},
    provenance, signing,
    tools::{InvocationSpec, Role, Tools},
    worker,
};
use crate::{
    ci::{
        command::Command,
        process::{Executor, Native},
    },
    owned_process::Signals,
    tooling::{Environment, Result},
};
use serde_json::{Value, json};
use simplestchat_ops::distribution::manifest::{
    ArtifactManifest,
    assembly::{EvidenceSources, prepare_artifact},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

struct StageClock(Arc<dyn plan::Clock>);
impl super::super::Clock for StageClock {
    fn now(&self) -> Instant {
        self.0.now()
    }
}

#[derive(Default)]
struct Owners {
    downloads: Option<Tree>,
    kit: Option<Tree>,
    native: Option<Tree>,
    tools: Option<Tools>,
}
impl Owners {
    fn finish(&mut self) -> Result<()> {
        let mut result = Ok(());
        if let Some(owner) = self.downloads.take() {
            result = result.and(owner.finish());
        }
        if let Some(owner) = self.kit.take() {
            result = result.and(owner.finish());
        }
        if let Some(owner) = self.native.take() {
            result = result.and(owner.finish_build());
        }
        if let Some(owner) = self.tools.take() {
            result = result.and(owner.finish());
        }
        result
    }
}
fn decode(bytes: &[u8]) -> Result<Value> {
    if bytes.len() > 65536 {
        return Err(failure(1));
    }
    serde_json::from_slice(bytes).map_err(|_| failure(1))
}
fn selected_digest(value: &Value, name: &str) -> Result<String> {
    value[name]["sha256"]
        .as_str()
        .filter(|s| super::super::digest(s))
        .map(str::to_owned)
        .ok_or_else(|| failure(1))
}
fn record(run: &OwnedRun, name: &str, value: &impl serde::Serialize) -> Result<(PathBuf, String)> {
    run.check()?;
    let bytes = provenance::encoded(value)?;
    namespace::write(run.file(), name, &bytes, run.budget())?;
    run.check()?;
    Ok((run.path.join(name), super::super::sha(&bytes)))
}
fn disjoint(a: &Path, b: &Path) -> bool {
    !a.starts_with(b) && !b.starts_with(a)
}
async fn create<E: Executor>(
    context: &Context,
    output: &Path,
    budget: &Budget,
    plan: &Plan,
    executor: &mut E,
) -> Result<OwnedRun> {
    if !namespace::path(output)
        || !disjoint(&context.workspace, output)
        || output.components().count() > 15
        || output.as_os_str().as_encoded_bytes().contains(&b':')
    {
        return Err(failure(1));
    }
    let source = Chain::open(&context.workspace, budget)?;
    let parent = match Chain::open(output.parent().ok_or_else(|| failure(1))?, budget) {
        Ok(owner) => owner,
        Err(error) => {
            let _ = source.finish();
            return Err(error);
        }
    };
    let result = (|| {
        let original = source.witness()?;
        let parent = parent.witness()?;
        if original.leaf_in(&parent) {
            return Err(failure(1));
        }
        Ok(parent)
    })();
    let closed = source.finish().and(parent.finish());
    let witness = result?;
    closed?;
    executor.ready().await?;
    plan.check()?;
    OwnedRun::local_witness(output, budget, &witness)
}

async fn import<E: Executor>(
    mut tree: Tree,
    mut graph: Graph,
    file: Held,
    artifact: super::catalog::Artifact,
    plan: &Plan,
    executor: &mut E,
    end: Instant,
) -> Result<(Tree, Graph)> {
    worker::work(
        plan,
        executor,
        end,
        move |checkpoint| {
            let result = (|| {
                checkpoint.check()?;
                let original = file.file().metadata().map_err(|_| failure(1))?;
                graph.import(file.file(), &artifact, &mut tree, &mut || {
                    checkpoint.check()
                })?;
                if !namespace::same(&original, &file.file().metadata().map_err(|_| failure(1))?) {
                    return Err(failure(1));
                }
                tree.check()
            })();
            let closed = file.finish();
            if let Err(error) = result.and(closed) {
                let _ = tree.finish();
                return Err(error);
            }
            Ok((tree, graph))
        },
        |(tree, _)| tree.finish(),
    )
    .await
}
async fn complete_kit<E: Executor>(
    mut tree: Tree,
    mut graph: Graph,
    plan: &Plan,
    executor: &mut E,
    end: Instant,
) -> Result<Tree> {
    worker::work(
        plan,
        executor,
        end,
        move |checkpoint| {
            let result = kit::complete(&mut tree, &mut graph, &mut || checkpoint.check());
            if let Err(error) = result {
                let _ = tree.finish();
                return Err(error);
            }
            Ok(tree)
        },
        Tree::finish,
    )
    .await
}
async fn crate_alias<E: Executor>(
    mut tree: Tree,
    source: String,
    name: String,
    plan: &Plan,
    executor: &mut E,
    end: Instant,
) -> Result<Tree> {
    worker::work(
        plan,
        executor,
        end,
        move |checkpoint| {
            let result =
                Graph::default().copy(&mut tree, &source, &name, &mut || checkpoint.check());
            if let Err(error) = result {
                let _ = tree.finish();
                return Err(error);
            }
            Ok(tree)
        },
        Tree::finish,
    )
    .await
}

async fn probes<E: Executor>(
    owners: &mut Owners,
    progress: &mut Progress<'_>,
    executor: &mut E,
    end: Instant,
    next: &mut usize,
) -> Result<()> {
    let plan = progress.owner();
    let tools = owners.tools.as_ref().ok_or_else(|| failure(1))?;
    let tree = owners.kit.as_ref().ok_or_else(|| failure(1))?;
    let arguments = kit::arguments();
    let total = arguments.len();
    for (index, args) in arguments.into_iter().enumerate() {
        progress.kit(Checkpoint::Probe, Edge::Begin, index + 1, total);
        executor.ready().await?;
        tree.check()?;
        let (body, latest) = plan.body(end, Duration::from_secs(5))?;
        let proof = tools.invocation(InvocationSpec {
            role: Role::CcProbe,
            args: args.clone(),
            directory: tree.run.path.clone(),
            environment: kit::environment(&tree.run.path)?,
            body,
            stdout_maximum: 65536,
            input: None,
            clock: plan.clock.clone(),
            latest,
        })?;
        let downloads = owners.downloads.as_ref().ok_or_else(|| failure(1))?;
        let (logs, names, permits) = downloads.capture(*next, 65536).await?;
        *next += 1;
        let mut command = Command::capture(Role::CcProbe.program(), &[]);
        command.arguments = args;
        command.capture = None;
        command.separate_logs = Some(logs);
        command.hosted_ops = Some(proof.clone());
        command.timeout = body;
        command.grace = Duration::from_secs(5);
        let ran = executor.run(command).await;
        drop(permits);
        ran?;
        let seals = proof.captures()?;
        // Both final writer seals are checked before parsing either stream.
        let downloads = owners.downloads.as_mut().ok_or_else(|| failure(1))?;
        downloads.captured(&names, &seals, &mut || {
            plan.check()?;
            if plan.now()? >= end {
                Err(failure(124))
            } else {
                Ok(())
            }
        })?;
        if !downloads
            .read(&names[1], 65536, &mut || plan.check())?
            .is_empty()
        {
            return Err(failure(1));
        }
        let bytes = downloads.read(&names[0], 65536, &mut || plan.check())?;
        kit::validate(index, &bytes, tree)?;
        executor.ready().await?;
        if plan.now()? >= end {
            return Err(failure(124));
        }
        progress.kit(Checkpoint::Probe, Edge::Complete, index + 1, total);
    }
    Ok(())
}

fn final_inventory(run: &OwnedRun, expected: &BTreeSet<String>) -> Result<()> {
    run.check()?;
    let _permit = run.budget().permit()?;
    let cursor = rustix::fs::Dir::read_from(run.file()).map_err(|_| failure(1))?;
    let mut actual = BTreeSet::new();
    for entry in cursor {
        let entry = entry.map_err(|_| failure(1))?;
        let name = entry.file_name().to_str().map_err(|_| failure(1))?;
        if !matches!(name, "." | "..") && (actual.len() >= 32 || !actual.insert(name.to_owned())) {
            return Err(failure(1));
        }
    }
    if &actual != expected {
        return Err(failure(1));
    }
    run.check()
}

trait Stages {
    async fn source<E: Executor, C: super::super::Clock>(
        &mut self,
        path: &Path,
        selected: &str,
        budget: &Budget,
        executor: &mut E,
        deadline: &Deadline<'_, C>,
    ) -> Result<Vec<u8>>;
    async fn registry<E: Executor, C: super::super::Clock>(
        &mut self,
        path: &Path,
        selected: &str,
        budget: &Budget,
        executor: &mut E,
        deadline: &Deadline<'_, C>,
    ) -> Result<Vec<u8>>;
    async fn producer<E: Executor, C: super::super::Clock>(
        &mut self,
        path: &Path,
        selected: &str,
        budget: &Budget,
        executor: &mut E,
        deadline: &Deadline<'_, C>,
    ) -> Result<Vec<u8>>;
}
struct RealStages;
impl Stages for RealStages {
    async fn source<E: Executor, C: super::super::Clock>(
        &mut self,
        path: &Path,
        selected: &str,
        budget: &Budget,
        executor: &mut E,
        deadline: &Deadline<'_, C>,
    ) -> Result<Vec<u8>> {
        Box::pin(super::super::source::observe(
            path, selected, budget, executor, deadline,
        ))
        .await
    }
    async fn registry<E: Executor, C: super::super::Clock>(
        &mut self,
        path: &Path,
        selected: &str,
        budget: &Budget,
        executor: &mut E,
        deadline: &Deadline<'_, C>,
    ) -> Result<Vec<u8>> {
        Box::pin(super::super::registry::observe(
            path, selected, budget, executor, deadline,
        ))
        .await
    }
    async fn producer<E: Executor, C: super::super::Clock>(
        &mut self,
        path: &Path,
        selected: &str,
        budget: &Budget,
        executor: &mut E,
        deadline: &Deadline<'_, C>,
    ) -> Result<Vec<u8>> {
        Box::pin(super::super::execute::observe(
            path, selected, budget, executor, deadline,
        ))
        .await
    }
}

async fn produce<E: Executor>(
    context: &Context,
    run: &OwnedRun,
    captured: &Environment,
    progress: &mut Progress<'_>,
    executor: &mut E,
) -> Result<Vec<u8>> {
    Box::pin(produce_with(
        context,
        run,
        captured,
        progress,
        executor,
        Catalog::load()?,
        &mut RealStages,
    ))
    .await
}

async fn produce_with<E: Executor, S: Stages>(
    context: &Context,
    run: &OwnedRun,
    captured: &Environment,
    progress: &mut Progress<'_>,
    executor: &mut E,
    catalog: Catalog,
    stages: &mut S,
) -> Result<Vec<u8>> {
    let plan = progress.owner();
    let budget = run.budget();
    let clock = StageClock(plan.clock.clone());
    let mut owners = Owners::default();
    let result = async {
        progress.enter(Phase::Source);
        let end = plan.begin(0)?;
        executor.ready().await?;
        plan.check()?;
        owners.tools = Some(Tools::select(&context.workspace, captured, budget)?);
        executor.ready().await?;
        if plan.now()? >= end {
            return Err(failure(124));
        }
        let git = owners.tools.as_ref().unwrap().git()?.to_path_buf();
        let bytes = namespace::read(&git, 64 * 1024 * 1024, budget)?;
        owners.tools.as_ref().unwrap().git()?;
        let (request, selected) = record(
            run,
            "source-request.json",
            &json!({
                "schemaVersion":1,"selection":context,"repository":context.workspace,
                "git":{"path":git,"sha256":super::super::sha(&bytes),"bytes":bytes.len()},
                "output":run.path.join("source")
            }),
        )?;
        let original_source = Box::pin(stages.source(
            &request,
            &selected,
            budget,
            executor,
            &Deadline::bounded(&clock, end),
        ))
        .await?;
        let source = decode(&original_source)?;
        let source_sha = selected_digest(&source, "source")?;
        let source_path = run.path.join("source/source");
        let lock_path = source_path.join("Cargo.lock");
        let original_lock = namespace::read(&lock_path, 4 * 1024 * 1024, budget)?;
        let lock_sha = super::super::sha(&original_lock);
        if materials::measure_bounded(
            &source_path,
            budget,
            executor,
            &Deadline::bounded(&clock, end),
            1024 * 1024 * 1024,
        )
        .await?
        .sha256
            != source_sha
        {
            return Err(failure(1));
        }
        plan.complete(0, end)?;
        progress.complete();

        progress.enter(Phase::Download);
        let end = plan.begin(1)?;
        owners.downloads = Some(Tree::create(&run.path.join("downloads"), budget)?);
        let mut material_names = Vec::new();
        let mut next = 0;
        for (index, artifact) in catalog.artifacts.iter().enumerate() {
            let tree = owners.downloads.take().ok_or_else(|| failure(1))?;
            let fetched = fetch::run(
                tree,
                next,
                fetch::Download {
                    url: artifact.url.clone(),
                    sha256: artifact.sha256.clone(),
                    bytes: Some(artifact.bytes),
                    maximum: catalog::MAX_ARCHIVE,
                },
                owners.tools.as_ref().unwrap(),
                executor,
                plan,
                end,
            )
            .await;
            progress.material_refused(
                MaterialKind::Catalog,
                index + 1,
                catalog.artifacts.len(),
                &fetched.result,
            );
            progress.transport_timeout(
                MaterialKind::Catalog,
                index + 1,
                catalog.artifacts.len(),
                fetched.timeout,
            );
            let (tree, name) = fetched.result?;
            next += 1;
            owners.downloads = Some(tree);
            material_names.push(name);
        }
        let archives = super::super::registry::selected_archives(&original_lock)?;
        if archives.is_empty() || archives.len() > 1024 {
            return Err(failure(1));
        }
        let mut archive_bytes = 0u64;
        let total = archives.len();
        for (index, archive) in archives.into_iter().enumerate() {
            let tree = owners.downloads.take().ok_or_else(|| failure(1))?;
            let fetched = fetch::run(
                tree,
                next,
                fetch::Download {
                    url: format!(
                        "https://static.crates.io/crates/{}/{}-{}.crate",
                        archive.name, archive.name, archive.version
                    ),
                    sha256: archive.checksum,
                    bytes: None,
                    maximum: 16 * 1024 * 1024,
                },
                owners.tools.as_ref().unwrap(),
                executor,
                plan,
                end,
            )
            .await;
            progress.material_refused(MaterialKind::Registry, index + 1, total, &fetched.result);
            progress.transport_timeout(MaterialKind::Registry, index + 1, total, fetched.timeout);
            let (tree, name) = fetched.result?;
            next += 1;
            archive_bytes = archive_bytes
                .checked_add(tree.records[&name].bytes)
                .ok_or_else(|| failure(1))?;
            if archive_bytes > 128 * 1024 * 1024 {
                let _ = tree.finish();
                return Err(failure(1));
            }
            owners.downloads = Some(
                crate_alias(
                    tree,
                    name,
                    format!("{}-{}.crate", archive.name, archive.version),
                    plan,
                    executor,
                    end,
                )
                .await?,
            );
        }
        plan.complete(1, end)?;
        progress.complete();

        progress.enter(Phase::Registry);
        let end = plan.begin(2)?;
        let (request, selected) = record(
            run,
            "registry-request.json",
            &json!({
                "schemaVersion":1,"lock":{"path":lock_path,"sha256":lock_sha},
                "archivesRoot":run.path.join("downloads"),"output":run.path.join("registry")
            }),
        )?;
        let original_registry = Box::pin(stages.registry(
            &request,
            &selected,
            budget,
            executor,
            &Deadline::bounded(&clock, end),
        ))
        .await?;
        let registry = decode(&original_registry)?;
        let registry_sha = selected_digest(&registry, "registry")?;
        plan.complete(2, end)?;
        progress.complete();

        progress.enter(Phase::Kit);
        let end = plan.begin(3)?;
        owners.kit = Some(Tree::create(&run.path.join("kit"), budget)?);
        owners.native = Some(Tree::create(&run.path.join("native-work"), budget)?);
        let mut graph = Graph::default();
        let mut native_graph = Graph::default();
        let total = catalog.artifacts.len();
        for (index, (artifact, original_name)) in
            catalog.artifacts.iter().zip(material_names).enumerate()
        {
            progress.kit(Checkpoint::Import, Edge::Begin, index + 1, total);
            let name = if artifact.format == Format::UbuntuDeb {
                let downloads = owners.downloads.take().ok_or_else(|| failure(1))?;
                let (downloads, name) = fetch::decoded_deb(
                    downloads,
                    next,
                    &original_name,
                    owners.tools.as_ref().unwrap(),
                    executor,
                    plan,
                    end,
                )
                .await?;
                next += 1;
                owners.downloads = Some(downloads);
                name
            } else {
                original_name
            };
            let file = owners.downloads.as_ref().unwrap().open(&name)?;
            if artifact.format == Format::NativeGzip {
                let tree = owners.native.take().ok_or_else(|| failure(1))?;
                let (tree, imported) = import(
                    tree,
                    native_graph,
                    file,
                    artifact.clone(),
                    plan,
                    executor,
                    end,
                )
                .await?;
                native_graph = imported;
                owners.native = Some(tree);
            } else {
                let tree = owners.kit.take().ok_or_else(|| failure(1))?;
                let (tree, imported) =
                    import(tree, graph, file, artifact.clone(), plan, executor, end).await?;
                graph = imported;
                owners.kit = Some(tree);
            }
            progress.kit(Checkpoint::Import, Edge::Complete, index + 1, total);
        }
        progress.kit(Checkpoint::Resolve, Edge::Begin, 1, 1);
        let tree = owners.kit.take().ok_or_else(|| failure(1))?;
        owners.kit = Some(complete_kit(tree, graph, plan, executor, end).await?);
        let mut tree = owners.native.take().ok_or_else(|| failure(1))?;
        owners.native = Some(
            worker::work(
                plan,
                executor,
                end,
                move |checkpoint| {
                    let result = native_graph.resolve(&mut tree, &mut || checkpoint.check());
                    if let Err(error) = result {
                        let _ = tree.finish();
                        return Err(error);
                    }
                    Ok(tree)
                },
                Tree::finish,
            )
            .await?,
        );
        progress.kit(Checkpoint::Resolve, Edge::Complete, 1, 1);
        owners
            .tools
            .as_mut()
            .unwrap()
            .admit_cc(&run.path.join("kit"))?;
        owners
            .tools
            .as_mut()
            .unwrap()
            .admit_native(&run.path.join("kit"))?;
        probes(&mut owners, progress, executor, end, &mut next).await?;
        progress.kit(Checkpoint::NativeBuild, Edge::Begin, 1, 1);
        let native_observation = native_build::build(
            native_build::NativeBuild {
                catalog: &catalog,
                work: owners.native.as_ref().unwrap(),
                tools: owners.tools.as_ref().unwrap(),
                plan,
                end,
            },
            owners.kit.as_mut().unwrap(),
            owners.downloads.as_mut().unwrap(),
            executor,
            &mut next,
            progress,
        )
        .await?;
        progress.kit(Checkpoint::NativeBuild, Edge::Complete, 1, 1);
        progress.kit(Checkpoint::RegistryCopy, Edge::Begin, 1, 1);
        materials::copy_bounded(
            &run.path.join("registry/registry"),
            &run.path.join("kit/registry"),
            &registry_sha,
            1024 * 1024 * 1024,
            budget,
            executor,
            &Deadline::bounded(&clock, end),
        )
        .await?;
        owners.kit.as_mut().unwrap().registry_created()?;
        progress.kit(Checkpoint::RegistryCopy, Edge::Complete, 1, 1);
        progress.kit(Checkpoint::Measure, Edge::Begin, 1, 1);
        let kit_identity = materials::measure(
            &run.path.join("kit"),
            budget,
            executor,
            &Deadline::bounded(&clock, end),
        )
        .await?;
        if materials::measure_bounded(
            &run.path.join("kit/registry"),
            budget,
            executor,
            &Deadline::bounded(&clock, end),
            1024 * 1024 * 1024,
        )
        .await?
        .sha256
            != registry_sha
        {
            return Err(failure(1));
        }
        progress.kit(Checkpoint::Measure, Edge::Complete, 1, 1);
        // All private readers and bootstrap proofs close before the producer's
        // separately reserved descriptor profile admits compiler work.
        owners.finish()?;
        plan.complete(3, end)?;
        progress.complete();

        progress.enter(Phase::Producer);
        let end = plan.begin(4)?;
        let (request, selected) = record(
            run,
            "producer-request.json",
            &json!({
                "schemaVersion":1,"selection":context,
                "source":{"path":source_path,"sha256":source_sha},
                "builderKit":{"path":run.path.join("kit"),"sha256":kit_identity.sha256},
                "output":run.path.join("build")
            }),
        )?;
        let producer = Box::pin(stages.producer(
            &request,
            &selected,
            budget,
            executor,
            &Deadline::bounded(&clock, end),
        ))
        .await?;
        decode(&producer)?;
        plan.complete(4, end)?;
        progress.complete();
        progress.enter(Phase::Assembly);
        plan.finalizing()?;

        let hosted_kit = provenance::encoded(&json!({
            "schemaVersion":1,"kind":"ops-hosted-kit-observation",
            "catalogSha256":catalog.identity.sha256,"catalogBytes":catalog.identity.bytes,
            "archives":catalog.identity.archives,"archiveBytes":catalog.identity.archive_bytes,
            "registrySha256":registry_sha,"lockSha256":lock_sha,"kit":kit_identity,
            "resolverProbes":kit::arguments().len(),
            "compilerRuntime":"hostedOsAndDynamicLoaderTrusted",
            "native":native_observation,
        }))?;
        let context_bytes = provenance::encoded(context)?;
        let documents = BTreeMap::from([
            ("hosted-context.json".to_owned(), context_bytes),
            ("hosted-kit.json".to_owned(), hosted_kit),
            ("hosted-registry.json".to_owned(), original_registry),
            ("hosted-source.json".to_owned(), original_source),
        ]);
        let root = run.path.clone();
        let selection = context.selection()?;
        let selected_context = context.clone();
        let final_budget = budget.clone();
        let final_end = (plan.now()? + plan::FINAL).min(plan.end()) - Duration::from_secs(30);
        let observation = worker::work(
            plan,
            executor,
            final_end,
            move |checkpoint| {
                checkpoint.check()?;
                let original =
                    namespace::read(&root.join("build/ops-release.json"), 65536, &final_budget)?;
                let mut value = decode(&original)?;
                let mut sources = BTreeMap::new();
                let mut all_documents = documents;
                for (name, bytes) in &all_documents {
                    let chain = Chain::open(&root, &final_budget)?;
                    let written = namespace::write(chain.file(), name, bytes, &final_budget);
                    let closed = chain.finish();
                    written?;
                    closed?;
                    sources.insert(name.clone(), root.join(name));
                }
                for name in provenance::NAMES
                    .iter()
                    .filter(|n| n.starts_with("producer-"))
                {
                    let path = root.join("build").join(name);
                    let bytes = namespace::read(&path, 65536, &final_budget)?;
                    all_documents.insert((*name).to_owned(), bytes);
                    sources.insert((*name).to_owned(), path);
                }
                let evidence = value["evidenceDigests"]
                    .as_object_mut()
                    .ok_or_else(|| failure(1))?;
                for (name, bytes) in &all_documents {
                    evidence.insert(name.clone(), super::super::sha(bytes).into());
                }
                let inputs = value["inputDigests"]
                    .as_object_mut()
                    .ok_or_else(|| failure(1))?;
                inputs.insert(
                    "hosted/catalog-v1".into(),
                    catalog.identity.sha256.clone().into(),
                );
                inputs.insert("source-lock/original-sha256".into(), lock_sha.into());
                provenance::check(&selected_context, &value, &all_documents, &catalog)?;
                let manifest = ArtifactManifest::decode(&provenance::encoded(&value)?)
                    .map_err(|_| failure(1))?;
                checkpoint.check()?;
                let prepared = {
                    let _library = final_budget.reserve(100)?;
                    prepare_artifact(
                        &manifest,
                        &root.join(format!(
                            "build/target/{}/release/simplestchat-ops",
                            super::context::TARGET
                        )),
                        &selection,
                        &EvidenceSources::new(sources).map_err(|_| failure(1))?,
                        &root.join("unsigned"),
                    )
                    .map_err(|_| failure(1))?
                };
                checkpoint.check()?;
                provenance::encoded(&json!({
                "schemaVersion":1,"kind":"ops-hosted-production-observation",
                "selection":selection.identity(),"artifact":prepared,"catalog":catalog.identity,
                "unsignedArtifactName":selected_context.artifact_name(ArtifactKind::Unsigned),
                "signedArtifactName":selected_context.artifact_name(ArtifactKind::Signed),
                "scope":"preparedUnsignedBytesNotSignatureRuntimeOrInstallationProof"}))
            },
            |_| Ok(()),
        )
        .await?;
        let expected = [
            "source-request.json",
            "source",
            "downloads",
            "registry-request.json",
            "registry",
            "kit",
            "native-work",
            "producer-request.json",
            "build",
            "hosted-context.json",
            "hosted-kit.json",
            "hosted-registry.json",
            "hosted-source.json",
            "unsigned",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        final_inventory(run, &expected)?;
        progress.complete();
        Ok(observation)
    }
    .await;
    progress.refused(&result);
    progress.enter(Phase::Finalization);
    let closed = owners.finish();
    result.and_then(|bytes| closed.map(|()| bytes))
}

fn replay_environment() -> Environment {
    [
        ("LC_ALL", "C"),
        ("LANG", "C"),
        ("TZ", "UTC"),
        ("PATH", "/usr/bin:/bin"),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect()
}
async fn completion<E: Executor>(
    result: Result<Vec<u8>>,
    closed: Result<()>,
    plan: &Plan,
    executor: &mut E,
    budget: &Budget,
) -> Result<()> {
    let mut bytes = result?;
    closed?;
    executor.ready().await?;
    plan.check()?;
    if !executor.cleanup_passed() {
        return Err(failure(1));
    }
    bytes.push(b'\n');
    let _replay = budget.reserve(16)?;
    executor
        .replay(
            &bytes,
            false,
            plan.end().min(plan.now()? + Duration::from_secs(25)),
        )
        .await?;
    executor.ready().await?;
    plan.check()?;
    if !executor.cleanup_passed() {
        return Err(failure(1));
    }
    Ok(())
}
pub(super) async fn native(output: &Path) -> Result<()> {
    let signals = Signals::new()?;
    let captured = crate::tooling::inherited_env();
    let context = Context::capture(&captured)?;
    let budget = Budget::default();
    let plan = Plan::new(Arc::new(plan::NativeClock));
    let replay = replay_environment();
    let mut executor = Native::new(&context.workspace, &replay, signals);
    let mut stderr = std::io::stderr();
    let mut progress = Progress::new(&plan, &mut stderr);
    progress.enter(Phase::Source);
    let result = async {
        executor.ready().await?;
        plan.check()?;
        let run = create(&context, output, &budget, &plan, &mut executor).await?;
        let result = produce(&context, &run, &captured, &mut progress, &mut executor).await;
        progress.refused(&result);
        progress.enter(Phase::Finalization);
        let closed = run.finish();
        completion(result, closed, &plan, &mut executor, &budget).await
    }
    .await;
    progress.finish(&result);
    result
}
pub(super) async fn signing_native(directory: &Path, output: &Path) -> Result<()> {
    let signals = Signals::new()?;
    let captured = crate::tooling::inherited_env();
    let context = Context::capture(&captured)?;
    if !disjoint(directory, output) {
        return Err(failure(1));
    }
    let budget = Budget::default();
    let plan = Plan::signer(Arc::new(plan::NativeClock));
    let replay = replay_environment();
    let mut executor = Native::new(&context.workspace, &replay, signals);
    let end = plan.end() - Duration::from_secs(30);
    let directory = directory.to_path_buf();
    let output = output.to_path_buf();
    let selected_context = context.clone();
    let worker_budget = budget.clone();
    let result = worker::work(
        &plan,
        &mut executor,
        end,
        move |checkpoint| {
            checkpoint.check()?;
            let bytes = signing::prepare(
                &selected_context,
                &directory,
                &output,
                &worker_budget,
                &mut || checkpoint.check(),
            )?;
            checkpoint.check()?;
            Ok(bytes)
        },
        |_| Ok(()),
    )
    .await;
    completion(result, Ok(()), &plan, &mut executor, &budget).await
}

#[cfg(test)]
#[path = "coordinator_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "whole_tests.rs"]
mod whole_tests;
