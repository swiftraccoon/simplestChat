//! Exact-input CI fingerprints and authenticated success receipts.
//!
//! Keys bind the index's inventory/modes and the worktree's current bytes, not
//! HEAD. Receipt revision is attribution only. CodeQL scopes name extraction
//! inputs and cannot substitute a successful stamp for an analyzed database.

use anyhow::{Result, bail};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::time::Duration;
use tokio::io::AsyncReadExt;

mod audit_rust;
mod automation;
mod bounded_log;
mod browser_suite;
mod codeql_cache;
mod command;
mod commit_subject;
mod config_validation;
mod container_engine;
mod container_smoke;
mod database_check;
mod evidence_copy;
mod fingerprint;
mod github_output;
mod image_cache;
mod inputs;
mod install_compose;
mod io;
mod load_generator_image;
mod mdns;
mod native_dtls;
mod native_security;
mod npm_path;
mod ops_artifact;
mod performance_compare;
mod process;
mod production_image;
#[cfg(feature = "public-smoke")]
mod public_smoke;
mod receipts;
mod release_checks;
mod release_controller;
mod required;
mod runner;
mod selected_executable;
mod session_soak;
#[cfg(test)]
mod tests;

const USAGE: &str = "usage: cargo run --locked -q -p xtask -- ci key|save|verify \
--scope SCOPE [--github-output] [--key KEY --directory PATH] [--root CHECKOUT]\n\
       cargo run --locked -q -p xtask -- ci native-security|automation|codeql-cache|required\n\
       cargo run --locked -q -p xtask -- ci browser-suite SUITE\n\
       cargo run --locked -q -p xtask -- ci copy-evidence KIND\n\
       cargo run --locked -q -p xtask -- ci production-image build|verify\n\
       cargo run --locked -q -p xtask -- ci install-compose\n\
       cargo run --locked -q -p xtask -- ci audit-rust\n\
       cargo run --locked -q -p xtask -- ci release-checks\n\
       cargo run --locked -q -p xtask -- ci release-controller start|wait\n\
       cargo run --locked -q -p xtask -- ci commit-subject\n\
       cargo run --locked -q -p xtask -- ci container-smoke\n\
       cargo run --locked -q -p xtask -- ci validate-config promtool|caddy\n\
       cargo run --locked -q -p xtask -- ci load-generator-image\n\
       cargo run --locked -q -p xtask -- ci mdns preflight|snapshot [--artifact-only]\n\
       cargo run --locked -q -p xtask -- ci database-check monitoring|restore|migration\n\
       cargo run --locked -q -p xtask -- ci session-soak\n\
       cargo run --locked -q -p xtask -- ci performance-compare\n\
       cargo run --locked -q -p xtask -- ci native-dtls [--record-success]\n\
       cargo run --locked -q -p xtask -- ci npm-path\n\
       cargo run --locked -q -p xtask -- ci controller-path\n\
       cargo run --locked -q -p xtask -- ci image-cache hint|status preparation|status production";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Scope {
    Automation,
    RustLint,
    RustTest,
    Backend,
    NativeAsan,
    NativeUbsan,
    NativeReplay,
    CodeqlNative,
    CodeqlRust,
}

impl Scope {
    fn parse(value: &str) -> Option<Self> {
        if !inputs::SCOPES.contains(&value) {
            return None;
        }
        Some(match value {
            "automation" => Self::Automation,
            "rust-lint" => Self::RustLint,
            "rust-test" => Self::RustTest,
            "backend" => Self::Backend,
            "native-asan" => Self::NativeAsan,
            "native-ubsan" => Self::NativeUbsan,
            "native-replay" => Self::NativeReplay,
            "codeql-native" => Self::CodeqlNative,
            "codeql-rust" => Self::CodeqlRust,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Self::Automation => "automation",
            Self::RustLint => "rust-lint",
            Self::RustTest => "rust-test",
            Self::Backend => "backend",
            Self::NativeAsan => "native-asan",
            Self::NativeUbsan => "native-ubsan",
            Self::NativeReplay => "native-replay",
            Self::CodeqlNative => "codeql-native",
            Self::CodeqlRust => "codeql-rust",
        }
    }

    fn native(self) -> bool {
        matches!(
            self,
            Self::NativeAsan | Self::NativeUbsan | Self::NativeReplay
        )
    }

    fn codeql(self) -> bool {
        matches!(self, Self::CodeqlNative | Self::CodeqlRust)
    }
}

pub(super) fn require(condition: bool, code: &str) -> Result<()> {
    if !condition {
        bail!("{code}");
    }
    Ok(())
}

/// Capture process inputs once; tests vary explicit contexts rather than global env.
pub(super) struct Context {
    root: PathBuf,
    environment: BTreeMap<String, String>,
    system: String,
    architecture: String,
}

impl Context {
    fn current(root: &Path) -> Result<Self> {
        let host = rustix::system::uname();
        let environment = std::env::vars_os()
            .map(|(name, value)| {
                Ok((
                    name.into_string()
                        .map_err(|_| anyhow::anyhow!("ci_cache_environment"))?,
                    value
                        .into_string()
                        .map_err(|_| anyhow::anyhow!("ci_cache_environment"))?,
                ))
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            root: root.canonicalize()?,
            environment,
            system: host.sysname().to_str()?.to_owned(),
            architecture: host.machine().to_str()?.to_owned(),
        })
    }

    fn env(&self, name: &str) -> &str {
        self.environment.get(name).map_or("", String::as_str)
    }

    fn trusted(&self) -> bool {
        self.env("GITHUB_REF") == "refs/heads/main"
            && self.env("GITHUB_EVENT_NAME") != "pull_request"
    }

    /// The command boundary accepts only the two original read-only Git inventories.
    fn tracked(&self, staged: bool) -> Result<Vec<u8>> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            let mut command = tokio::process::Command::new("git");
            command.arg("ls-files");
            if staged {
                command.arg("--stage");
            }
            let mut child = command
                .arg("-z")
                .current_dir(&self.root)
                .env_clear()
                .envs(&self.environment)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()?;
            let mut stdout = child
                .stdout
                .take()
                .ok_or_else(|| anyhow::anyhow!("ci_cache_git"))?
                .take(64 * 1024 * 1024 + 1);
            let mut stderr = child
                .stderr
                .take()
                .ok_or_else(|| anyhow::anyhow!("ci_cache_git"))?
                .take(65537);
            let work = async {
                let mut output = Vec::new();
                let mut errors = Vec::new();
                let read_output = async {
                    stdout.read_to_end(&mut output).await?;
                    require(output.len() <= 64 * 1024 * 1024, "ci_cache_git")
                };
                let read_errors = async {
                    stderr.read_to_end(&mut errors).await?;
                    require(errors.len() <= 65536, "ci_cache_git")
                };
                let (status, _, _) = tokio::try_join!(
                    async { Ok::<_, anyhow::Error>(child.wait().await?) },
                    read_output,
                    read_errors,
                )?;
                require(
                    status.success() && output.len() <= 64 * 1024 * 1024 && errors.len() <= 65536,
                    "ci_cache_git",
                )?;
                Ok(output)
            };
            tokio::time::timeout(Duration::from_secs(30), work)
                .await
                .map_err(|_| anyhow::anyhow!("ci_cache_git_timeout"))?
        })
    }
}

struct Options {
    operation: String,
    scope: Scope,
    key: String,
    directory: PathBuf,
    root: PathBuf,
    github_output: bool,
}

fn options(root: &Path, arguments: &[String]) -> Option<Options> {
    let mut operation = None;
    let mut scope = None;
    let mut key = String::new();
    let mut directory = PathBuf::from(".");
    let mut checkout = root.to_path_buf();
    let mut github_output = false;
    let mut receipt_arguments = false;
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "key" | "save" | "verify" if operation.is_none() => operation = Some(argument.clone()),
            "--scope" => scope = Some(Scope::parse(arguments.next()?)?),
            "--github-output" if !github_output => github_output = true,
            "--key" => {
                key = arguments.next()?.clone();
                receipt_arguments = true;
            }
            "--directory" => {
                directory = PathBuf::from(arguments.next()?);
                receipt_arguments = true;
            }
            "--root" => checkout = PathBuf::from(arguments.next()?),
            _ => return None,
        }
    }
    let operation = operation?;
    if github_output && (operation != "key" || receipt_arguments) {
        return None;
    }
    Some(Options {
        operation,
        scope: scope?,
        key,
        directory,
        root: checkout,
        github_output,
    })
}

fn print_key(context: &Context, scope: Scope, github_output: bool) -> Result<()> {
    if github_output {
        let target = github_output::Target::open(context)?;
        let key = fingerprint::key(context, scope)?;
        target.append(scope, context.trusted(), &key)
    } else {
        println!("{}", fingerprint::key(context, scope)?);
        Ok(())
    }
}

/// Emit only one selected media address through the existing reserved replay
/// child. The detector's same signal registration stays with its caller.
pub(crate) async fn replay_media_ip(
    root: &Path,
    environment: &crate::tooling::Environment,
    signals: &mut crate::owned_process::Signals,
    address: std::net::Ipv4Addr,
    deadline: std::time::Instant,
) -> crate::tooling::Result<()> {
    process::replay_output(
        root,
        environment,
        signals,
        format!("{address}\n").as_bytes(),
        false,
        deadline,
    )
    .await
    .result
}

pub(crate) fn run_container_smoke(root: &Path, arguments: &[String]) -> Result<ExitCode> {
    container_smoke::run_local(root, arguments)
}

#[cfg(feature = "public-smoke")]
pub(crate) fn run_public_smoke(arguments: &[String]) -> Result<ExitCode> {
    public_smoke::run(arguments)
}

#[cfg(not(feature = "public-smoke"))]
pub(crate) fn run_public_smoke(_arguments: &[String]) -> Result<ExitCode> {
    bail!(
        "public-smoke requires: cargo run --locked -q -p xtask --features public-smoke -- public-smoke --help"
    )
}

pub(crate) fn run_ops_artifact(arguments: &[String]) -> Result<ExitCode> {
    ops_artifact::run(arguments)
}

pub(crate) fn run(root: &Path, arguments: &[String]) -> Result<ExitCode> {
    if arguments.first().is_some_and(|arg| arg == "mdns") {
        return mdns::run(root, &arguments[1..]);
    }
    if arguments
        .first()
        .is_some_and(|arg| arg == "load-generator-image")
    {
        return load_generator_image::run(root, &arguments[1..]);
    }
    if arguments
        .first()
        .is_some_and(|arg| arg == "validate-config")
    {
        return config_validation::run(root, &arguments[1..]);
    }
    if arguments
        .first()
        .is_some_and(|argument| argument == "performance-compare")
    {
        return performance_compare::run(root, &arguments[1..]);
    }
    if arguments
        .first()
        .is_some_and(|argument| argument == "native-dtls")
    {
        return native_dtls::run(root, &arguments[1..]);
    }
    if arguments
        .first()
        .is_some_and(|argument| argument == "container-smoke")
    {
        return container_smoke::run_ci(root, &arguments[1..]);
    }
    if arguments
        .first()
        .is_some_and(|argument| argument == "session-soak")
    {
        return session_soak::run(root, &arguments[1..]);
    }
    if arguments
        .first()
        .is_some_and(|argument| argument == "commit-subject")
    {
        return commit_subject::run(root, &arguments[1..]);
    }
    if arguments
        .first()
        .is_some_and(|argument| argument == "release-controller")
    {
        return release_controller::run(root, &arguments[1..]);
    }
    if arguments
        .first()
        .is_some_and(|argument| argument == "database-check")
    {
        return database_check::run(root, &arguments[1..]);
    }

    if arguments
        .first()
        .is_some_and(|argument| argument == "release-checks")
    {
        return release_checks::run(root, &arguments[1..]);
    }
    if arguments
        .first()
        .is_some_and(|argument| argument == "image-cache")
    {
        return image_cache::run(root, &arguments[1..]);
    }
    if arguments
        .first()
        .is_some_and(|argument| argument == "controller-path")
    {
        return npm_path::run_controller(&arguments[1..]);
    }
    if arguments
        .first()
        .is_some_and(|argument| argument == "npm-path")
    {
        return npm_path::run(&arguments[1..]);
    }
    if arguments
        .first()
        .is_some_and(|argument| argument == "audit-rust")
    {
        return audit_rust::run(root, &arguments[1..]);
    }
    if arguments
        .first()
        .is_some_and(|argument| argument == "install-compose")
    {
        return install_compose::run(root, &arguments[1..]);
    }
    if arguments
        .first()
        .is_some_and(|argument| argument == "production-image")
    {
        return production_image::run(root, &arguments[1..]);
    }
    if arguments
        .first()
        .is_some_and(|argument| argument == "copy-evidence")
    {
        return evidence_copy::run(&arguments[1..]);
    }
    if arguments
        .first()
        .is_some_and(|argument| argument == "browser-suite")
    {
        return browser_suite::run(root, &arguments[1..]);
    }
    if arguments
        .first()
        .is_some_and(|argument| argument == "required")
    {
        return required::run(&arguments[1..]);
    }
    if arguments
        .first()
        .is_some_and(|argument| argument == "codeql-cache")
    {
        return codeql_cache::run(root, &arguments[1..]);
    }
    if arguments
        .first()
        .is_some_and(|argument| argument == "automation")
    {
        return automation::run(root, &arguments[1..]);
    }
    if arguments
        .first()
        .is_some_and(|argument| argument == "native-security")
    {
        return native_security::run(root, &arguments[1..]);
    }
    if arguments
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    let Some(options) = options(root, arguments) else {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(2));
    };
    let work = || -> Result<()> {
        let context = Context::current(&options.root)?;
        if options.operation == "key" {
            print_key(&context, options.scope, options.github_output)?;
        } else {
            require(
                options.directory.is_absolute() && !options.directory.is_symlink(),
                "ci_cache_directory",
            )?;
            if options.operation == "save" {
                receipts::save(&context, options.scope, &options.key, &options.directory)?;
            } else {
                let revision =
                    receipts::verify(&context, options.scope, &options.key, &options.directory)?;
                println!(
                    "Reused {} success for identical inputs; original revision: {revision}",
                    options.scope.name()
                );
            }
        }
        Ok(())
    };
    match work() {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(error) => {
            eprintln!("CI cache rejected: {error}");
            Ok(ExitCode::FAILURE)
        }
    }
}
