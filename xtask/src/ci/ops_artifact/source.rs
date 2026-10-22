//! Export one explicitly selected local Git commit; no remote/main authentication.
//! Selected Git and its runtime/object database are caller-owned trust inputs.

mod catalog;
mod git;
#[cfg(test)]
mod native_tests;
mod objects;
mod request;
#[cfg(test)]
mod tests;
mod tree;
mod view;
use super::{
    Clock, Deadline, NativeClock, digest, failure, hex, io, materials,
    namespace::{self, Budget, Chain, Held, OwnedRun},
    sha,
};
use crate::{
    ci::{
        command::{Command, Program},
        process::Executor,
    },
    tooling::{Environment, Result},
};
pub(crate) use git::Git;
use git::Repository;
use request::Request;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path, sync::Arc, time::Duration};

fn environment(run: &OwnedRun, repository: &Repository) -> Result<Environment> {
    for name in ["home", "tmp"] {
        run.directory(name)?.finish()?;
    }
    Ok([
        ("HOME", run.path.join("home").into_os_string()),
        ("TMPDIR", run.path.join("tmp").into_os_string()),
        ("GIT_CONFIG_SYSTEM", "/dev/null".into()),
        ("GIT_CONFIG_GLOBAL", "/dev/null".into()),
        ("GIT_CONFIG_NOSYSTEM", "1".into()),
        ("GIT_CONFIG_COUNT", "0".into()),
        ("GIT_TERMINAL_PROMPT", "0".into()),
        ("GIT_ALTERNATE_OBJECT_DIRECTORIES", "".into()),
        (
            "GIT_OBJECT_DIRECTORY",
            repository.objects().as_os_str().to_owned(),
        ),
        ("LC_ALL", "C".into()),
        ("TZ", "UTC".into()),
    ]
    .into_iter()
    .map(|(key, value)| (key.into(), value))
    .collect())
}

struct Context<'a, C> {
    git: &'a Arc<Git>,
    repository: &'a Repository,
    run: &'a OwnedRun,
    view: &'a view::View,
    deadline: &'a Deadline<'a, C>,
}
impl<C: Clock> Context<'_, C> {
    async fn call<E: Executor>(
        &self,
        executor: &mut E,
        args: &[&str],
        maximum: u64,
    ) -> Result<Vec<u8>> {
        let Self {
            git,
            repository,
            run,
            view,
            deadline,
        } = self;
        deadline.check(false)?;
        executor.ready().await?;
        repository.check()?;
        run.check()?;
        git.validate()?;
        view.check(run)?;
        let mut command = git::command(git, args, maximum);
        command.timeout = deadline.body(Duration::from_secs(30))?;
        let result = executor.run(command).await;
        let checked = git
            .validate()
            .map(|_| ())
            .and(repository.check())
            .and(run.check())
            .and(view.check(run));
        let bytes = result?;
        checked?;
        executor.ready().await?;
        deadline.check(false)?;
        if !executor.cleanup_passed() || bytes.len() as u64 > maximum {
            return Err(failure(1));
        }
        Ok(bytes)
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Observation<'a> {
    schema_version: u8,
    kind: &'static str,
    request_sha256: &'a str,
    request_bytes: usize,
    selection: &'a simplestchat_ops::distribution::manifest::SourceIdentity,
    commit: &'a str,
    tree: String,
    git_sha256: &'a str,
    git_bytes: u64,
    source: materials::Identity,
    claims: &'static str,
}
async fn export<E: Executor, C: Clock>(
    request: &Request,
    context: &Context<'_, C>,
    executor: &mut E,
) -> Result<Vec<u8>> {
    let run = context.run;
    let deadline = context.deadline;
    let kind = context
        .call(executor, &["cat-file", "-t", &request.revision], 64)
        .await?;
    if kind != b"commit\n" {
        return Err(failure(1));
    }
    let selected_tree = format!("{}^{{tree}}", request.revision);
    let tree = catalog::oid(
        &context
            .call(
                executor,
                &["rev-parse", "--verify", "--end-of-options", &selected_tree],
                64,
            )
            .await?,
    )?;
    let args = [
        "ls-tree",
        "-r",
        "-t",
        "-l",
        "-z",
        "--full-tree",
        &tree,
        "--",
    ];
    let raw = context.call(executor, &args, catalog::CATALOG).await?;
    let catalog = catalog::parse(&raw)?;
    let mut objects = objects::Objects::create(run)?;
    let checked = check_objects(request, context, executor, &tree, &catalog, &mut objects).await;
    if let Err(error) = checked {
        let _ = objects.finish();
        return Err(error);
    }
    let mut output = tree::Tree::create(run)?;
    let mut digests = BTreeMap::new();
    let written = async {
        for (name, entry) in &catalog {
            executor.ready().await?;
            deadline.check(false)?;
            if entry.directory {
                output.directory(name)?;
                continue;
            }
            let bytes = context
                .call(executor, &["cat-file", "blob", &entry.oid], catalog::BLOB)
                .await?;
            if bytes.len() as u64 != entry.bytes {
                return Err(failure(1));
            }
            output.write(name, &bytes, entry.executable)?;
            digests.insert(name.clone(), Sha256::digest(&bytes));
            let path = run.path.join("source").join(name);
            let observed = context
                .call(
                    executor,
                    &[
                        "hash-object",
                        "--no-filters",
                        "-t",
                        "blob",
                        "--",
                        path.to_str().ok_or_else(|| failure(1))?,
                    ],
                    64,
                )
                .await?;
            if catalog::oid(&observed)? != entry.oid {
                return Err(failure(1));
            }
            output.check_one(name)?;
        }
        let repeated = context.call(executor, &args, catalog::CATALOG).await?;
        if repeated != raw {
            return Err(failure(1));
        }
        check_objects(request, context, executor, &tree, &catalog, &mut objects).await?;
        output.check(run)?;
        let identity =
            materials::measure(&run.path.join("source"), run.budget(), executor, deadline).await?;
        output.check(run)?;
        let mut expected = Sha256::new();
        expected.update(b"ops-producer-tree-v1\0");
        for (name, directory, executable, count, digest) in std::iter::once((
            "", true, false, 0u64, [0u8; 32],
        ))
        .chain(catalog.iter().map(|(name, entry)| {
            (
                name.as_str(),
                entry.directory,
                entry.executable,
                entry.bytes,
                digests.get(name).map_or([0; 32], |value| (*value).into()),
            )
        })) {
            expected.update((name.len() as u32).to_be_bytes());
            expected.update(name.as_bytes());
            expected.update([u8::from(directory), u8::from(executable)]);
            expected.update(count.to_be_bytes());
            expected.update(digest);
        }
        if identity.sha256 != hex(&expected.finalize()) {
            return Err(failure(1));
        }
        let bytes = serde_json::to_vec(&Observation {
            schema_version: 1,
            kind: "ops-local-git-source-observation",
            request_sha256: &request.sha256,
            request_bytes: request.bytes,
            selection: request.selection.identity(),
            commit: &request.revision,
            tree: tree.clone(),
            git_sha256: &request.git.sha256,
            git_bytes: request.git.bytes,
            source: identity,
            claims: "observedLocalGitRelationshipNotAuthenticatedOriginOrSigningReadiness",
        })
        .map_err(|_| failure(1))?;
        namespace::write(run.file(), "source-observation.json", &bytes, run.budget())?;
        output.check(run)?;
        Ok(bytes)
    }
    .await;
    let closed = output.finish(run);
    let objects_closed = objects.finish();
    written.and_then(|value| closed.and(objects_closed).map(|()| value))
}

async fn check_objects<E: Executor, C: Clock>(
    request: &Request,
    context: &Context<'_, C>,
    executor: &mut E,
    root: &str,
    catalog: &catalog::Catalog,
    objects: &mut objects::Objects,
) -> Result<()> {
    let commit = objects
        .check(context, executor, "commit", &request.revision)
        .await?;
    if !commit.starts_with(format!("tree {root}\n").as_bytes()) {
        return Err(failure(1));
    }
    let mut trees = std::collections::BTreeSet::from([root.to_owned()]);
    for entry in catalog.values().filter(|entry| entry.directory) {
        trees.insert(entry.oid.clone());
    }
    for oid in trees {
        objects.check(context, executor, "tree", &oid).await?;
    }
    Ok(())
}

pub(super) async fn native(path: &Path, selected: &str) -> Result<()> {
    use crate::{ci::process::Native, owned_process::Signals};
    let mut signals = Signals::new()?;
    let clock = NativeClock;
    let deadline = Deadline::new(&clock);
    let budget = Budget::default();
    if let Some(code) = signals.pending().await {
        return Err(failure(code));
    }
    let request = Request::read(path, selected, &budget)?;
    let repository = Repository::open(&request.repository, &budget)?;
    // No Cargo configuration is relevant: every Git configuration source is
    // replaced by the literal fresh bare view and clear-then-set environment.
    let run = OwnedRun::local(&request.output, &budget)?;
    let view = view::View::create(&run)?;
    let environment = environment(&run, &repository)?;
    let cwd = run.path.clone();
    let mut executor = Native::new(&cwd, &environment, signals);
    let result = async {
        executor.ready().await?;
        deadline.check(false)?;
        let git = Arc::new(Git::open(&request.git, &budget, &mut executor, &deadline).await?);
        let _native = budget.reserve(16)?;
        let context = Context {
            git: &git,
            repository: &repository,
            run: &run,
            view: &view,
            deadline: &deadline,
        };
        let result = export(&request, &context, &mut executor).await;
        let closed = Arc::try_unwrap(git)
            .map_err(|_| failure(1))
            .and_then(Git::finish);
        result.and_then(|value| closed.map(|()| value))
    }
    .await;
    let view_closed = view.finish(&run);
    let repository_closed = repository.finish();
    let run_closed = run.finish();
    complete(
        result,
        view_closed.and(repository_closed).and(run_closed),
        &budget,
        &mut executor,
        &deadline,
    )
    .await
}
async fn complete<E: Executor, C: Clock>(
    result: Result<Vec<u8>>,
    closed: Result<()>,
    budget: &Budget,
    executor: &mut E,
    deadline: &Deadline<'_, C>,
) -> Result<()> {
    let mut bytes = result?;
    closed?;
    executor.ready().await?;
    deadline.check(true)?;
    if !executor.cleanup_passed() {
        return Err(failure(1));
    }
    bytes.push(b'\n');
    let _replay = budget.reserve(16)?;
    executor.replay(&bytes, false, deadline.end()).await?;
    executor.ready().await?;
    if !executor.cleanup_passed() {
        return Err(failure(1));
    }
    deadline.check(true)
}
