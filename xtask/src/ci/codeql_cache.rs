//! Exact analyzed-database cache primitives, independent of current finding policy.
//!
//! The Python module remains the reference and scanner-driver import API. This
//! CLI validates source/bundles without extraction; save/restore use only an
//! explicitly selected installed CodeQL executable and private bounded commands.

mod archive;
mod cache;
mod key;
mod process;

#[cfg(test)]
#[path = "codeql_cache/cache_tests.rs"]
mod tests;

use super::{Context, io, require};
use anyhow::Result;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::DirBuilder;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "usage: cargo run --locked -q -p xtask -- ci codeql-cache \
key|validate-bundle|validate-source|save|restore [OPTIONS]\n\
  key --source-root PATH --language rust|c-cpp [--suite security|all]\n\
      [--openssl-prefix PATH --include-vendor] [--root CHECKOUT]\n\
  validate-bundle --bundle PATH --language rust|c-cpp\n\
  validate-source --database PATH --source-root PATH --manifest JSON --language rust|c-cpp\n\
  save --directory PATH --key KEY --revision SHA --language rust|c-cpp\n\
      --codeql PATH --output PATH\n\
  restore --directory PATH --key KEY --database PATH --codeql PATH --output PATH";

struct Options {
    operation: String,
    values: BTreeMap<String, String>,
    include_vendor: bool,
}

impl Options {
    fn text(&self, name: &str) -> Result<&str> {
        self.values
            .get(name)
            .map(String::as_str)
            .ok_or_else(|| anyhow::anyhow!("codeql_cache_argument"))
    }
    fn path(&self, name: &str) -> Result<PathBuf> {
        Ok(PathBuf::from(self.text(name)?))
    }
}

fn options(arguments: &[String]) -> Option<Options> {
    let (operation, rest) = arguments.split_first()?;
    if ![
        "key",
        "validate-bundle",
        "validate-source",
        "save",
        "restore",
    ]
    .contains(&operation.as_str())
    {
        return None;
    }
    let mut values = BTreeMap::from([("suite".into(), "security".into())]);
    let mut include_vendor = false;
    let mut arguments = rest.iter();
    while let Some(argument) = arguments.next() {
        if argument == "--include-vendor" {
            include_vendor = true;
            continue;
        }
        let name = argument.strip_prefix("--")?;
        if ![
            "source-root",
            "openssl-prefix",
            "language",
            "suite",
            "root",
            "directory",
            "database",
            "manifest",
            "key",
            "revision",
            "output",
            "codeql",
            "bundle",
        ]
        .contains(&name)
        {
            return None;
        }
        values.insert(name.into(), arguments.next()?.clone());
    }
    if values
        .get("language")
        .is_some_and(|value| !matches!(value.as_str(), "rust" | "c-cpp"))
        || values
            .get("suite")
            .is_some_and(|value| !matches!(value.as_str(), "security" | "all"))
    {
        return None;
    }
    let required: &[&str] = match operation.as_str() {
        "key" => &["source-root", "language"],
        "validate-bundle" => &["bundle", "language"],
        "validate-source" => &["database", "source-root", "manifest", "language"],
        "save" => &[
            "directory",
            "key",
            "revision",
            "language",
            "codeql",
            "output",
        ],
        "restore" => &["directory", "key", "database", "codeql", "output"],
        _ => unreachable!("validated operation"),
    };
    if required
        .iter()
        .any(|name| values.get(*name).is_none_or(String::is_empty))
    {
        return None;
    }
    Some(Options {
        operation: operation.clone(),
        values,
        include_vendor,
    })
}

fn require_scope(context: &Context, language: &str, include_vendor: bool) -> Result<()> {
    require(
        !include_vendor
            || !["CI", "GITHUB_ACTIONS", "ACT"]
                .iter()
                .any(|name| context.environment.contains_key(*name)),
        "vendor_source_scan_local_only",
    )?;
    require(
        language != "c-cpp" || include_vendor,
        "codeql_vendor_opt_in_required",
    )
}

struct Driver {
    root: PathBuf,
    environment: BTreeMap<String, String>,
    codeql: String,
}

impl Driver {
    fn new(context: &Context, options: &Options) -> Result<Self> {
        let output = options.path("output")?;
        require(
            output.is_absolute() && output.is_dir() && !output.is_symlink(),
            "codeql_cache_output",
        )?;
        let home = output.join("home");
        if !home.exists() {
            DirBuilder::new().mode(0o700).create(&home)?;
        }
        require(home.is_dir() && !home.is_symlink(), "codeql_cache_output")?;
        let codeql = options.path("codeql")?;
        require(
            codeql.is_absolute() && codeql.is_file(),
            "codeql_binary_required",
        )?;
        let mut environment = BTreeMap::from([
            ("PATH".into(), context.env("PATH").to_owned()),
            ("HOME".into(), home.to_string_lossy().into_owned()),
            (
                "LANG".into(),
                if context.system == "Linux" {
                    "C.UTF-8"
                } else {
                    "en_US.UTF-8"
                }
                .into(),
            ),
            ("NO_COLOR".into(), "1".into()),
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
            ("PYTHONNOUSERSITE".into(), "1".into()),
            ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
        ]);
        for name in [
            "CARGO_HOME",
            "RUSTUP_HOME",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
            "TMPDIR",
        ] {
            if let Some(value) = context.environment.get(name) {
                environment.insert(name.into(), value.clone());
            }
        }
        Ok(Self {
            root: context.root.clone(),
            environment,
            codeql: codeql.to_string_lossy().into_owned(),
        })
    }
}

impl cache::Driver for Driver {
    fn run(&mut self, _label: &str, arguments: &[String]) -> Result<()> {
        process::capture(
            &self.root,
            &self.environment,
            &self.codeql,
            arguments,
            180,
            16 * 1024 * 1024,
            16 * 1024 * 1024,
        )
        .map(|_| ())
    }
}

fn execute(root: &Path, options: &Options) -> Result<Option<Value>> {
    let checkout = options
        .values
        .get("root")
        .map_or_else(|| root.to_path_buf(), PathBuf::from);
    match options.operation.as_str() {
        "key" => {
            let context = Context::current(&checkout)?;
            let language = options.text("language")?;
            require_scope(&context, language, options.include_vendor)?;
            let openssl = options.values.get("openssl-prefix").map(PathBuf::from);
            let value = key::key(
                &context,
                &options.path("source-root")?,
                openssl.as_deref(),
                language,
                options.text("suite")?,
                options.include_vendor,
            )?;
            println!("{value}");
            Ok(None)
        }
        "validate-bundle" => {
            archive::bundle_integrity(&options.path("bundle")?, options.text("language")?)?;
            Ok(None)
        }
        "validate-source" => {
            let manifest: Value = serde_json::from_slice(&io::bounded(
                &options.path("manifest")?,
                16 * 1024 * 1024,
            )?)?;
            archive::source_integrity(
                &options.path("database")?,
                &options.path("source-root")?,
                manifest
                    .as_object()
                    .ok_or_else(|| anyhow::anyhow!("codeql_cache_manifest"))?,
                options.text("language")?,
            )?;
            Ok(None)
        }
        "save" | "restore" => {
            let context = Context::current(&checkout)?;
            let mut driver = Driver::new(&context, options)?;
            let directory = options.path("directory")?;
            if options.operation == "save" {
                Ok(Some(cache::save(
                    &mut driver,
                    &options.path("output")?,
                    &directory,
                    options.text("key")?,
                    options.text("revision")?,
                    options.text("language")?,
                )?))
            } else {
                cache::restore(
                    &mut driver,
                    &directory,
                    options.text("key")?,
                    &options.path("database")?,
                )
            }
        }
        _ => unreachable!("validated operation"),
    }
}

pub(super) fn run(root: &Path, arguments: &[String]) -> Result<ExitCode> {
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "--help" | "-h"))
    {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    let Some(options) = options(arguments) else {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(2));
    };
    match execute(root, &options) {
        Ok(Some(value)) => {
            println!("{value}");
            Ok(ExitCode::SUCCESS)
        }
        Ok(None) => Ok(ExitCode::SUCCESS),
        Err(_) => {
            eprintln!("CodeQL analyzed database cache failed validation.");
            Ok(ExitCode::FAILURE)
        }
    }
}
