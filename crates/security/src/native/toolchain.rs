//! The host that built: its compilers by what they say of themselves, the
//! packages it is made of with the source package of each, and the package
//! that owns the static C++ runtime that was linked.

use super::{
    field,
    licenses::{inherit_workspace_package, space},
    object, problem, reviewed_license, toml,
};
use crate::{
    context,
    document::Document,
    vendor::{Problem, Verdict, diff, manifest},
};
use std::{path::Path, process::Command, time::Duration};

/// The most bytes a query of the packages may print.
const MAX_REPORT: u64 = 8 * 1024 * 1024;
/// The most bytes a compiler or a query of one package may print.
const SHORT: u64 = 65536;
/// The most bytes a command may complain in: one that complains at all
/// has failed.
const COMPLAINT: u64 = 65536;
/// How long a command may take.
const PATIENCE: Duration = Duration::from_secs(30);
/// How a package is asked for: its name, its version and its source
/// package, with tabs between them.
const FORMAT: &str = "%{NAME}\t%{EPOCHNUM}:%{VERSION}-%{RELEASE}.%{ARCH}\t%{SOURCERPM}\n";

/// What runs the commands that inspect the host.
pub trait Host {
    /// What the command `argv` prints, of at most `limit` bytes, when it
    /// ends well and complains of nothing.
    fn run(&self, argv: &[&str], limit: u64) -> Verdict<String>;
}

/// This machine.
pub struct Machine;

impl Host for Machine {
    fn run(&self, argv: &[&str], limit: u64) -> Verdict<String> {
        let failed = || problem("Build-host provenance command failed");
        let (program, arguments) = argv.split_first().ok_or_else(failed)?;
        let mut command = Command::new(program);
        command.args(arguments);
        let heard = context::heard(&mut command, PATIENCE, (limit, COMPLAINT));
        let (status, printed, complained) =
            heard.map_err(|failure| Problem(failure.to_string()))?;
        if status != 0 || !complained.is_empty() {
            return Err(failed());
        }
        String::from_utf8(printed).map_err(|_| problem("Build-host output is no text"))
    }
}

/// The packages of the host and the keys that sign packages, both in
/// their order: a key is no installed software and has neither an
/// architecture nor a source package, and every package must name its
/// source package.
pub fn builder_rpm_records(lines: &[&str]) -> Verdict<(Vec<String>, Vec<String>)> {
    let mut sorted = lines.to_vec();
    sorted.sort_unstable();
    if sorted.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(problem("Duplicate builder RPM provenance"));
    }
    let incomplete = || problem("Incomplete builder RPM provenance");
    let (mut packages, mut keys) = (Vec::new(), Vec::new());
    for line in sorted {
        let parts: Vec<&str> = line.split('\t').collect();
        let [name, version, source] = parts[..] else {
            return Err(incomplete());
        };
        if name.is_empty() || version.is_empty() || source.is_empty() {
            return Err(incomplete());
        }
        if name == "gpg-pubkey" {
            let digits = |text: &str, count: usize| {
                let digit = |byte: u8| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte);
                text.len() == count && text.bytes().all(digit)
            };
            let signed = version
                .strip_prefix("0:")
                .and_then(|said| said.strip_suffix(".(none)"));
            let signed = signed.and_then(|said| said.split_once('-'));
            let named = signed.is_some_and(|(print, made)| digits(print, 40) && digits(made, 8));
            if !named || source != "(none)" {
                return Err(problem("Invalid builder RPM signing-key provenance"));
            }
            keys.push(line.to_owned());
        } else if source.ends_with(".src.rpm") {
            packages.push(line.to_owned());
        } else {
            return Err(incomplete());
        }
    }
    Ok((packages, keys))
}

/// The lines of what a command printed, without their ends.
fn lines(text: &str) -> Vec<&str> {
    let lines = diff::lines(text).into_iter();
    lines
        .map(|line| line.trim_end_matches(diff::ends))
        .collect()
}

/// The evidence of the host that built the checkout `root` and linked the
/// static C++ `runtime`.
pub fn toolchain_evidence(root: &Path, runtime: &Path, host: &dyn Host) -> Verdict<Document> {
    let rust = host.run(&["rustc", "--version", "--verbose"], SHORT)?;
    let manifest = toml(&root.join("Cargo.toml"))?;
    let package = object(field(&manifest, "package")?)?;
    let package = inherit_workspace_package(package, &manifest)?;
    let reviewed = manifest::string(field(&package, "rust-version")?)?;
    if !rust.contains(&format!("release: {reviewed}\n")) {
        return Err(problem(
            "Rust compiler differs from the reviewed production version",
        ));
    }
    let compiler = host.run(&["c++", "--version"], SHORT)?;
    let all = host.run(
        &["rpm", "--query", "--all", "--queryformat", FORMAT],
        MAX_REPORT,
    )?;
    let (packages, keys) = builder_rpm_records(&lines(&all))?;
    let runtime = runtime.display().to_string();
    let owner = host.run(
        &[
            "rpm",
            "--query",
            "--file",
            &runtime,
            "--queryformat",
            FORMAT,
        ],
        MAX_REPORT,
    )?;
    let owned = match lines(&owner)[..] {
        [one] if one.starts_with("libstdc++-static\t") => {
            packages.iter().find(|package| *package == one)
        }
        _ => None,
    };
    let owned = owned
        .ok_or_else(|| problem("Static C++ runtime is not owned by the recorded builder RPM"))?;
    let licence = host.run(
        &[
            "rpm",
            "--query",
            "--file",
            &runtime,
            "--queryformat",
            "%{LICENSE}\n",
        ],
        SHORT,
    )?;
    let licence = licence.trim_matches(space);
    reviewed_license(&Document::record([("license", licence.into())]))?;
    let list =
        |lines: &[String]| Document::List(lines.iter().map(|line| line.as_str().into()).collect());
    Ok(Document::record([
        ("rustc", rust.as_str().into()),
        ("cxx", compiler.as_str().into()),
        ("builder_rpms", list(&packages)),
        ("builder_signing_keys", list(&keys)),
        ("static_cxx_owner", owned.as_str().into()),
        ("static_cxx_license", licence.into()),
    ]))
}

#[cfg(test)]
#[path = "toolchain_tests.rs"]
pub(super) mod tests;
