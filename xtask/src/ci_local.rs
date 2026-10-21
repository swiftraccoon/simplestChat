//! Execute the maintained workflow on an explicitly owned local engine.
//!
//! Full-gate evidence requires clean, unchanged source and the exact aggregate
//! receipts; focused jobs always remain partial. Cleanup stops only the owned
//! VM started by this invocation, never an ambient or pre-existing engine.

mod engine;
mod evidence;
mod options;
mod process;
#[cfg(test)]
#[path = "ci_local/ci_local_tests.rs"]
mod tests;

use crate::tooling::{Environment, Failure};
use anyhow::Result;
use engine::text;
use options::Options;
use process::{Runner, Signals};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

struct Context {
    root: PathBuf,
    revision: String,
    base: String,
    job: String,
    started: String,
    image: String,
    platform: String,
    run_id: String,
    started_machine: bool,
}

struct OwnedDirectory {
    path: PathBuf,
    handle: File,
}

impl OwnedDirectory {
    fn open(path: PathBuf) -> crate::tooling::Result<Self> {
        let handle = File::open(&path).map_err(io_failure)?;
        Ok(Self { path, handle })
    }

    fn current(&self) -> crate::tooling::Result<()> {
        let actual = fs::symlink_metadata(&self.path).map_err(io_failure)?;
        let owned = self.handle.metadata().map_err(io_failure)?;
        if actual.is_dir() && actual.dev() == owned.dev() && actual.ino() == owned.ino() {
            Ok(())
        } else {
            Err(Failure::new(
                1,
                "Owned local CI directory was replaced; it was left untouched.",
            ))
        }
    }
}

fn timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn io_failure(error: std::io::Error) -> Failure {
    Failure::new(1, error.to_string())
}

fn require_tool(root: &Path, environment: &Environment, name: &str) -> crate::tooling::Result<()> {
    let installed = environment.get(OsStr::new("PATH")).is_some_and(|path| {
        std::env::split_paths(path).any(|directory| {
            root.join(directory)
                .join(name)
                .metadata()
                .is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                })
        })
    });
    if installed {
        Ok(())
    } else {
        Err(Failure::new(2, format!("Missing prerequisite: {name}")))
    }
}

pub(crate) fn run(root: &Path, arguments: &[String]) -> Result<ExitCode> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(async {
        run_with_env(
            root,
            arguments,
            crate::tooling::inherited_env(),
            Signals::new()?,
        )
        .await
    });
    let status = match result {
        Ok(status) => status,
        Err(error) => {
            eprintln!("{}", error.message);
            error.code
        }
    };
    Ok(ExitCode::from(status))
}

async fn run_with_env(
    root: &Path,
    arguments: &[String],
    environment: Environment,
    signals: Signals,
) -> crate::tooling::Result<u8> {
    let Some(options) = options::parse(arguments)? else {
        print!("{}", options::USAGE);
        return Ok(0);
    };
    for tool in ["curl", "git", "python3"] {
        require_tool(root, &environment, tool)?;
    }
    let mut runner = Runner::new(root, environment, signals);
    let revision = text(
        runner
            .capture("git", ["rev-parse", "--verify", "HEAD"])
            .await?,
    )?;
    if options.job == "all"
        && !runner
            .capture("git", ["status", "--porcelain=v1", "--untracked-files=all"])
            .await?
            .is_empty()
    {
        return Err(Failure::new(
            2,
            "Full CI requires a clean committed checkout. Commit local changes first; no push is needed.",
        ));
    }
    let base_ref = if let Some(base) = options.base.as_ref().filter(|base| !base.is_empty()) {
        base.clone()
    } else {
        let base = runner
            .capture("git", ["merge-base", "HEAD", "refs/remotes/origin/main"])
            .await
            .map_err(|failure| refusal(failure, "Pass --base with an existing local commit."))?;
        let base = text(base)?;
        if base == revision {
            "HEAD^".into()
        } else {
            base
        }
    };
    let base = text(
        runner
            .capture(
                "git",
                ["rev-parse", "--verify", &format!("{base_ref}^{{commit}}")],
            )
            .await
            .map_err(|failure| refusal(failure, "The --base revision must exist locally."))?,
    )?;
    if base == revision {
        return Err(Failure::new(
            2,
            "The changed-source base must precede the candidate.",
        ));
    }
    runner
        .run("git", ["merge-base", "--is-ancestor", &base, &revision])
        .await
        .map_err(|failure| {
            refusal(
                failure,
                "The changed-source base must be an ancestor of HEAD.",
            )
        })?;
    let state = root.join("target/act");
    for directory in [
        state.join("actions"),
        state.join("cache"),
        root.join("results"),
    ] {
        evidence::private_directory(&directory, true)?;
    }
    let lock_path = state.join("run.lock");
    evidence::private_directory(&lock_path, false).map_err(|_| {
        Failure::new(
            2,
            format!("Another local CI invocation owns {}.", lock_path.display()),
        )
    })?;
    let lock = match OwnedDirectory::open(lock_path.clone()) {
        Ok(lock) => lock,
        Err(error) => {
            let _ = fs::remove_dir(lock_path);
            return Err(error);
        }
    };
    let mut context = Context {
        root: root.into(),
        revision,
        base,
        job: options.job.clone(),
        started: timestamp(),
        image: engine::AMD64_IMAGE.into(),
        platform: "linux/amd64".into(),
        run_id: String::new(),
        started_machine: false,
    };
    let mut output = None;
    let operation = owned(&mut runner, &mut context, &options, &state, &mut output).await;
    let mut status = match operation {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{}", error.message);
            error.code
        }
    };
    if let Some(signal) = runner.pending_signal().await {
        status = signal;
    }
    if context.started_machine
        && let Err(error) = engine::stop(&mut runner).await
    {
        eprintln!("{}", error.message);
        status = if engine::interrupted(&error) {
            error.code
        } else {
            1
        };
    }
    if let Err(error) = lock
        .current()
        .and_then(|()| fs::remove_dir(&lock.path).map_err(io_failure))
    {
        eprintln!("{}", error.message);
        status = 1;
    }
    if let Some(output) = output {
        if let Some(signal) = runner.pending_signal().await {
            status = signal;
        }
        let written = output.current().and_then(|()| {
            evidence::json_file(
                &output.path.join("summary.json"),
                &evidence::summary(&context, status),
            )
        });
        if let Err(error) = written {
            eprintln!("{}", error.message);
            status = 1;
        }
    }
    Ok(status)
}

fn refusal(failure: Failure, message: &str) -> Failure {
    if engine::interrupted(&failure) {
        failure
    } else {
        Failure::new(2, message)
    }
}

async fn owned(
    runner: &mut Runner,
    context: &mut Context,
    options: &Options,
    state: &Path,
    output: &mut Option<OwnedDirectory>,
) -> crate::tooling::Result<()> {
    *output = Some(OwnedDirectory::open(evidence::create_output(
        &context.root,
        options.output.as_deref(),
    )?)?);
    let output = output.as_ref().expect("created output");
    println!("Local CI evidence: {}", output.path.display());
    let act = text(runner.capture("python3", ["build/ci_local_act.py"]).await?)?;
    let socket = engine::select(runner, context, options).await?;
    engine::ready(runner, &socket, &output.path).await?;
    if !options.disposable && context.platform == "linux/arm64" {
        engine::arm64(runner).await?;
    }
    let networks = engine::snapshot(
        runner,
        &socket,
        "http://localhost/networks",
        &output.path.join("engine-networks.json"),
    )
    .await?;
    let gateway = engine::gateway(&networks)?;
    context.run_id = evidence::random_id()?;
    evidence::json_file(&output.path.join("event.json"), &evidence::event(context))?;
    evidence::private_directory(&output.path.join("checks"), false)?;
    let arguments = act_arguments(context, options, state, &output.path, &gateway.to_string());
    let (child, logging) = runner
        .act(&act, arguments, &output.path.join("act.log"))
        .await?;
    output.current()?;
    if context.job == "all" && child == 0 {
        let unchanged = text(
            runner
                .capture("git", ["rev-parse", "--verify", "HEAD"])
                .await?,
        )? == context.revision;
        let clean = unchanged
            && runner
                .capture("git", ["status", "--porcelain=v1", "--untracked-files=all"])
                .await?
                .is_empty();
        if !clean {
            return Err(Failure::new(
                2,
                "The candidate changed during CI; this result does not validate the current source.",
            ));
        }
        evidence::validate(context, &output.path)?;
    }
    let containers = engine::snapshot(
        runner,
        &socket,
        "http://localhost/containers/json?all=1",
        &output.path.join("engine-after.json"),
    )
    .await?;
    engine::empty(
        &containers,
        "Local CI left containers behind; inspect the owned engine before another run.",
    )?;
    if child != 0 {
        return Err(Failure::new(
            child,
            format!("Workflow failed (status {child})"),
        ));
    }
    if logging != 0 {
        return Err(Failure::new(
            logging,
            "Cannot retain the complete workflow log.",
        ));
    }
    Ok(())
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn act_arguments(
    context: &Context,
    options: &Options,
    state: &Path,
    output: &Path,
    gateway: &str,
) -> Vec<OsString> {
    let mount = format!(
        "type=bind,source={},target=/local-ci-evidence",
        output.join("checks").display()
    );
    let mut container = format!(
        "--privileged --cgroupns=private --cpus=3 --memory=16g --pids-limit=2048 \
         --add-host=host.docker.internal:{gateway} --mount type=volume,target=/var/lib/local-ci-docker \
         --mount {}",
        quote(&mount)
    );
    if evidence::codeql_cache(&context.root, &context.platform) {
        let path = context.root.join(".cache/codeql-tools");
        container.push_str(&format!(
            " --mount {}",
            quote(&format!(
                "type=bind,source={},target={},readonly",
                path.display(),
                path.display()
            ))
        ));
    }
    let selected = if context.job == "all" {
        "required"
    } else {
        &context.job
    };
    let mut arguments: Vec<OsString> = [
        "push",
        "--workflows",
        ".github/workflows/ci.yml",
        "--job",
        selected,
        "--eventpath",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    arguments.push(output.join("event.json").into_os_string());
    arguments.extend(["--defaultbranch", "main", "--platform"].map(OsString::from));
    arguments.push(format!("ubuntu-24.04={}", context.image).into());
    arguments.extend(
        [
            "--container-architecture",
            "",
            "--pull=false",
            "--rm",
            "--network",
            "bridge",
            "--concurrent-jobs",
            "4",
            "--dryrun=false",
            "--list=false",
            "--graph=false",
            "--validate=false",
            "--watch=false",
            "--reuse=false",
            "--bind=false",
            "--no-skip-checkout=false",
            "--list-options=false",
            "--bug-report=false",
            "--man-page=false",
            "--container-daemon-socket",
            "-",
            "--container-options",
        ]
        .map(OsString::from),
    );
    arguments.push(container.into());
    arguments.extend(["--use-new-action-cache=true", "--action-cache-path"].map(OsString::from));
    arguments.push(state.join("actions").into_os_string());
    arguments.push("--cache-server-path".into());
    arguments.push(state.join("cache").into_os_string());
    arguments.extend(
        [
            "--env-file",
            "/dev/null",
            "--secret-file",
            "/dev/null",
            "--var-file",
            "/dev/null",
            "--input-file",
            "/dev/null",
            "--secret",
            "GITHUB_TOKEN=",
            "--env",
            "LOCAL_CI_DISPOSABLE=1",
            "--env",
            "LOCAL_CI_EVIDENCE=/local-ci-evidence",
            "--env",
        ]
        .map(OsString::from),
    );
    arguments.push(format!("LOCAL_CI_RUNNER_IMAGE={}", context.image).into());
    arguments.push("--env".into());
    arguments.push(format!("LOCAL_CI_RUN_ID={}", context.run_id).into());
    arguments.push("--env".into());
    arguments.push(format!("RUNNER_TRACKING_ID=local-ci-{}", context.run_id).into());
    for matrix in &options.matrix {
        arguments.extend(["--matrix".into(), matrix.into()]);
    }
    arguments
}
