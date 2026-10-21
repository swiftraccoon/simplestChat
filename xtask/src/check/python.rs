//! NUL-delimited Git discovery and the maintained Python source policy.

use crate::tooling::{self, Environment, Failure};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub(super) fn run(root: &Path, environment: &Environment) -> tooling::Result<()> {
    let selected = environment
        .get(OsStr::new("PYTHON_CHECK_ENV"))
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.join("ops/ansible/.venv"));
    for name in ["python", "ruff", "basedpyright"] {
        let executable = selected.join("bin").join(name);
        let installed = root
            .join(&executable)
            .metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0);
        if !installed {
            return Err(Failure::new(
                1,
                format!(
                    "Missing {name}; install build/python-requirements.txt in {}.",
                    selected.display()
                ),
            ));
        }
    }
    let discovered = tooling::output(
        root,
        environment,
        "git",
        [
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
            "*.py",
            "*.pyi",
        ],
    )?;
    let mut sources = Vec::new();
    for source in discovered
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        if source.starts_with(b"vendor/") || source.starts_with(b"reference/") {
            continue;
        }
        let allowed = (source.ends_with(b".py")
            && [
                b"build/".as_slice(),
                b"ops/ansible/files/",
                b"ops/ansible/callback_plugins/",
                b"ops/ansible/filter_plugins/",
                b"ops/ansible/tests/",
            ]
            .iter()
            .any(|prefix| source.starts_with(prefix)))
            || (source.starts_with(b"typings/") && source.ends_with(b".pyi"));
        let path = OsString::from_vec(source.to_vec());
        if !allowed {
            return Err(Failure::new(
                1,
                format!(
                    "Python source outside the checked roots: {}",
                    path.to_string_lossy()
                ),
            ));
        }
        sources.push(path);
    }
    if sources.is_empty() {
        return Err(Failure::new(1, "No maintained Python sources found."));
    }
    println!(
        "Checking {} maintained Python sources and stubs.",
        sources.len()
    );
    for arguments in [vec!["check", "--"], vec!["format", "--check", "--"]] {
        tooling::run(
            root,
            environment,
            selected.join("bin/ruff"),
            arguments
                .into_iter()
                .map(OsString::from)
                .chain(sources.iter().cloned()),
        )?;
    }
    // basedpyright has no terminator. Every source begins with an allowlisted,
    // non-option root, so it cannot become a checker option.
    tooling::run(
        root,
        environment,
        selected.join("bin/basedpyright"),
        [
            OsString::from("--pythonpath"),
            selected.join("bin/python").into_os_string(),
        ]
        .into_iter()
        .chain(sources),
    )
}
