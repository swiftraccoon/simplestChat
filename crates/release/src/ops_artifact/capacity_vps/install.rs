//! A new run's delivery of the selected signed Ops executable to the benchmark
//! host: the production delivery tasks, imported unchanged by
//! `capacity-ops-install.yml`, given what a routine release gives them.
//!
//! Those tasks join the Ops selection to the signed application artifact of the
//! same CI run, so the run first verifies that artifact with the production
//! verifier `release.yml` runs. Its only use here is that join.

use super::super::deploy_frontend::policy::{self, ArtifactKind, Revision, Run};
use super::super::namespace::{self, Budget};
use super::{
    Result,
    execute::{Shared, State, lock, originals, stage},
    failure,
    files::Original,
    plan::Plan,
    transport,
    worker::Checkpoint,
};
use serde_json::{Value, json};
use simplestchat_ci::{command::Program, process::Executor};
use simplestchat_ops::capacity_vps::protocol::{Pin, Source};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// The benchmark host's delivery playbook, below the checkout.
pub(super) const PLAYBOOK: &str = "ops/ansible/capacity-ops-install.yml";
/// The production delivery tasks it imports unchanged.
pub(super) const TASKS: &str = "ops/ansible/tasks/signed-ops.yml";
const REPOSITORY: &str = "swiftraccoon/simplestChat";
/// The verifier's own bound, as `release.yml` gives it, then its settlement.
const VERIFY: [&str; 2] = ["600", "60"];
/// The largest predicate or manifest `release.yml` admits.
const MAX_ORIGINAL: u64 = 64 * 1024 * 1024;

/// The controller-verified application predicate and manifest that the delivery
/// joins to the Ops selection, held unchanged from verification to the end.
pub(super) struct Application {
    directory: PathBuf,
    predicate: Original,
    manifest: Original,
}
impl Application {
    pub fn check(&self, budget: &Budget, checkpoint: &Checkpoint) -> Result<()> {
        self.predicate.check(budget, checkpoint)?;
        self.manifest.check(budget, checkpoint)
    }
}

/// The `gh api` request for the run's one application artifact.
pub(super) fn listing(run: u64, revision: &Revision) -> Vec<OsString> {
    let endpoint = format!(
        "repos/{REPOSITORY}/actions/runs/{run}/artifacts?name={}&per_page=100",
        ArtifactKind::Application.name(revision)
    );
    [
        "api",
        "--hostname",
        "github.com",
        "--method",
        "GET",
        "-H",
        "Accept: application/vnd.github+json",
        "-H",
        "X-GitHub-Api-Version: 2022-11-28",
        &endpoint,
    ]
    .map(OsString::from)
    .to_vec()
}

/// The production verifier's command, as `release.yml` runs it, writing below
/// `parent` and downloading the artifact itself.
pub(super) fn verification(
    revision: &str,
    artifact: u64,
    run: u64,
    parent: &Path,
) -> Vec<OsString> {
    let mut argv = ["verify", "--repository", REPOSITORY, "--artifact-id"]
        .map(OsString::from)
        .to_vec();
    argv.extend([
        artifact.to_string().into(),
        "--revision".into(),
        revision.into(),
        "--ci-run".into(),
        run.to_string().into(),
        "--output-parent".into(),
        parent.as_os_str().to_owned(),
        "--timeout-seconds".into(),
        VERIFY[0].into(),
        "--finalization-seconds".into(),
        VERIFY[1].into(),
    ]);
    argv
}

/// The delivery playbook's command: the private inventory snapshot, the one
/// selected host, the written variables and the run's SSH trust override.
pub(super) fn command(
    projection: &Path,
    inventory: &Path,
    limit: &str,
    variables: &Path,
    common: &str,
) -> Result<Vec<OsString>> {
    let mut variables_file = OsString::from("@");
    variables_file.push(variables);
    Ok(vec![
        "-i".into(),
        inventory.as_os_str().to_owned(),
        projection.join(PLAYBOOK).into_os_string(),
        "--limit".into(),
        limit.into(),
        "--extra-vars".into(),
        variables_file,
        "--extra-vars".into(),
        serde_json::to_string(&json!({"ansible_ssh_common_args": common}))
            .map_err(|_| failure(1))?
            .into(),
    ])
}

/// The inputs of one delivery, borrowed from the run that installs.
pub(super) struct Delivery<'a> {
    pub revision: &'a str,
    pub ci_run: u64,
    pub artifact: u64,
    pub selection: &'a Source,
    pub controller: &'a Path,
    pub gh: &'a Path,
    pub host: &'a str,
    pub run: &'a str,
    pub binary: &'a Pin,
    pub application: &'a Path,
}
impl Delivery<'_> {
    /// What the delivery tasks are given: the variables `release deploy`
    /// passes, with the revision `release.yml` derives, and the inputs that
    /// bind the playbook to this host, run, executable and verified application.
    pub fn variables(&self) -> Value {
        json!({
            "scpub_release_repository": REPOSITORY,
            "scpub_release_ci_run": self.ci_run,
            "scpub_release_expected_revision": self.revision,
            "scpub_release_revision": self.revision,
            "scpub_ops_release_controller": self.controller,
            "scpub_ops_gh_executable": self.gh,
            "scpub_ops_artifact_id": self.artifact,
            "scpub_ops_selection": self.selection,
            "capacity_host": self.host,
            "capacity_run": self.run,
            "capacity_application_directory": self.application,
            "capacity_ops_binary": {"sha256": self.binary.sha256, "bytes": self.binary.bytes},
        })
    }
}

/// What the verified application must name: the run's revision and the CI
/// run and attempt that built it.
pub(super) struct Expected<'a> {
    pub revision: &'a str,
    pub ci_run: u64,
    pub attempt: u64,
}

/// Admit the verifier's receipt as `release.yml` does and hold its predicate
/// and manifest. The predicate must name this run and attempt, and the
/// directory the verifier's own fresh attempt below `parent`.
pub(super) fn admit(
    receipt: &[u8],
    parent: &Path,
    expected: &Expected<'_>,
    budget: &Budget,
    checkpoint: &Checkpoint,
) -> Result<Application> {
    let receipt: Value = serde_json::from_slice(receipt).map_err(|_| failure(1))?;
    let fields = receipt.as_object().ok_or_else(|| failure(1))?;
    let mut keys = fields.keys().map(String::as_str).collect::<Vec<_>>();
    keys.sort_unstable();
    let digest = |name: &str| receipt[name].as_str().is_some_and(super::super::digest);
    let directory = PathBuf::from(receipt["directory"].as_str().ok_or_else(|| failure(1))?);
    let attempt = directory.parent().ok_or_else(|| failure(1))?;
    let attempt_name = attempt
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| failure(1))?;
    if keys != ["directory", "image.tar", "release.json", "revision"]
        || receipt["revision"] != expected.revision
        || !digest("image.tar")
        || !digest("release.json")
        || !namespace::path(&directory)
        || directory.file_name() != Some("artifact".as_ref())
        || attempt.parent() != Some(parent)
        || !attempt_name
            .strip_prefix("release-verification.")
            .is_some_and(|value| {
                value.len() == 32
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
    {
        return Err(failure(1));
    }
    let (predicate, predicate_bytes) = Original::open(
        &directory.join("release-predicate.json"),
        MAX_ORIGINAL,
        budget,
        checkpoint,
    )?;
    let (manifest, manifest_bytes) = Original::open(
        &directory.join("release.json"),
        MAX_ORIGINAL,
        budget,
        checkpoint,
    )?;
    let named: Value = serde_json::from_slice(&predicate_bytes).map_err(|_| failure(1))?;
    if receipt["release.json"] != super::super::sha(&manifest_bytes)
        || named["runId"].as_u64() != Some(expected.ci_run)
        || named["runAttempt"].as_u64() != Some(expected.attempt)
        || named["revision"] != expected.revision
    {
        return Err(failure(1));
    }
    Ok(Application {
        directory,
        predicate,
        manifest,
    })
}

/// Verify the run's application artifact, then run the delivery playbook.
///
/// # Errors
/// A missing or ambiguous application artifact, a refused verification or
/// receipt, a failed playbook or a changed original fails the run before it
/// contacts the host for anything else.
pub(super) async fn run<E: Executor>(
    state: &Shared,
    plan: &Plan,
    executor: &mut E,
    end: Instant,
) -> Result<()> {
    let (revision, selected) = stage(
        state,
        plan,
        executor,
        end,
        |state, checkpoint| {
            let State {
                tools,
                caller,
                environment,
                controller,
                ..
            } = state;
            tools
                .as_mut()
                .ok_or_else(|| failure(1))?
                .install(caller, environment, controller)?;
            originals(state, checkpoint)?;
            Ok((
                Revision::named(&state.options.revision)?,
                Run::named(state.options.ci_run, state.options.attempt)?,
            ))
        },
        |_, _| Ok(()),
    )
    .await?;
    let found = transport::child(
        state,
        plan,
        executor,
        end,
        Program::OpsVpsGh,
        listing(selected.id.get(), &revision),
        Duration::from_secs(30),
    )
    .await?;
    let artifact = policy::artifact(&found, &revision, &selected, ArtifactKind::Application)?;
    let (argv, parent) = {
        let state = lock(state)?;
        let parent = state
            .output
            .as_ref()
            .ok_or_else(|| failure(1))?
            .path("application");
        (
            verification(
                revision.as_str(),
                artifact.id.get(),
                selected.id.get(),
                &parent,
            ),
            parent,
        )
    };
    let receipt = transport::child(
        state,
        plan,
        executor,
        end,
        Program::OpsVpsController,
        argv,
        Duration::from_secs(690),
    )
    .await?;
    let id = artifact.id.get();
    let argv = stage(
        state,
        plan,
        executor,
        end,
        move |state, checkpoint| {
            let expected = Expected {
                revision: &state.options.revision,
                ci_run: state.options.ci_run,
                attempt: state.options.attempt,
            };
            let application = admit(&receipt, &parent, &expected, &state.budget, checkpoint)?;
            let tools = state.tools.as_ref().ok_or_else(|| failure(1))?;
            let values = Delivery {
                revision: &state.options.revision,
                ci_run: state.options.ci_run,
                artifact: state.options.artifact,
                selection: state.selection.as_ref().ok_or_else(|| failure(1))?,
                controller: tools.controller_path()?,
                gh: tools.gh_path()?,
                host: &state.options.limit,
                run: &state.run,
                binary: state.binary.as_ref().ok_or_else(|| failure(1))?,
                application: &application.directory,
            }
            .variables();
            let bytes = serde_json::to_vec(&values).map_err(|_| failure(1))?;
            let application = std::sync::Arc::new(application);
            state
                .tools
                .as_mut()
                .ok_or_else(|| failure(1))?
                .bind_application(application.clone())?;
            state.application = Some(application);
            state.report.set("applicationArtifact", id.into());
            let output = state.output.as_mut().ok_or_else(|| failure(1))?;
            output.write("ops-install-vars.json", &bytes, checkpoint)?;
            let argv = command(
                state
                    .sources
                    .as_ref()
                    .ok_or_else(|| failure(1))?
                    .directory(),
                &output.path("inventory.snapshot.json"),
                &state.options.limit,
                &output.path("ops-install-vars.json"),
                &state
                    .inventory
                    .as_ref()
                    .ok_or_else(|| failure(1))?
                    .target
                    .common(),
            )?;
            originals(state, checkpoint)?;
            Ok(argv)
        },
        |_, _| Ok(()),
    )
    .await?;
    // The production export's own bound is 1,920 s; the installer's is 150 s.
    transport::child(
        state,
        plan,
        executor,
        end,
        Program::OpsVpsInstall,
        argv,
        Duration::from_secs(2010),
    )
    .await?;
    stage(
        state,
        plan,
        executor,
        end,
        |state, checkpoint| originals(state, checkpoint),
        |_, _| Ok(()),
    )
    .await
}
