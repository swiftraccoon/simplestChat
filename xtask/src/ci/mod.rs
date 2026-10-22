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

mod automation;
mod browser_suite;
mod codeql_cache;
mod fingerprint;
mod inputs;
mod io;
mod native_security;
mod receipts;
mod required;
#[cfg(test)]
mod tests;

const USAGE: &str = "usage: cargo run --locked -q -p xtask -- ci key|save|verify \
--scope SCOPE [--key KEY --directory PATH] [--root CHECKOUT]\n\
       cargo run --locked -q -p xtask -- ci native-security|automation|codeql-cache|required\n\
       cargo run --locked -q -p xtask -- ci browser-suite SUITE";

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
}

fn options(root: &Path, arguments: &[String]) -> Option<Options> {
    let mut operation = None;
    let mut scope = None;
    let mut key = String::new();
    let mut directory = PathBuf::from(".");
    let mut checkout = root.to_path_buf();
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "key" | "save" | "verify" if operation.is_none() => operation = Some(argument.clone()),
            "--scope" => scope = Some(Scope::parse(arguments.next()?)?),
            "--key" => key = arguments.next()?.clone(),
            "--directory" => directory = PathBuf::from(arguments.next()?),
            "--root" => checkout = PathBuf::from(arguments.next()?),
            _ => return None,
        }
    }
    Some(Options {
        operation: operation?,
        scope: scope?,
        key,
        directory,
        root: checkout,
    })
}

pub(crate) fn run(root: &Path, arguments: &[String]) -> Result<ExitCode> {
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
            println!("{}", fingerprint::key(&context, options.scope)?);
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
