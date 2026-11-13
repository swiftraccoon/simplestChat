//! The dependency declarations of the npm and Python locks.
//!
//! Vulnerability audits read whole dependency graphs. The licence review
//! reads only what a change declares anew, so each lock is parsed into
//! complete declarations: an installed path or a pinned requirement with
//! everything that identifies it. Nothing is installed, built or evaluated,
//! and a lock in any other form than the maintained one is refused.

mod requirement;

use crate::{
    Fault, Result, files,
    findings::{canonical, object, set},
    policy::space,
    record::{self, safe_name},
    require, scope,
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path};

/// The npm locks that are audited and reviewed.
pub const NPM_LOCKS: [&str; 2] = ["web/package-lock.json", "web/e2e/package-lock.json"];
/// The hash-locked requirement files that are audited and reviewed.
pub const PYTHON_LOCKS: [&str; 5] = [
    "build/python-requirements.txt",
    "ops/ansible/requirements.txt",
    "security/requirements.txt",
    "vendor/mediasoup-sys-0.19.0/python-invoke-requirements.txt",
    "vendor/mediasoup-sys-0.19.0/python-tools-requirements.txt",
];
/// The lock that the scanner environment is installed from.
const SCANNERS: &str = "security/requirements.txt";
/// The names of locks of other tools: no such file may change unreviewed.
/// `pylock.toml` is one of the `pylock.*.toml` files, which are named by
/// their shape.
const OTHER_LOCKS: [&str; 8] = [
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "Pipfile.lock",
    "poetry.lock",
    "uv.lock",
    "pdm.lock",
];
/// The most bytes of a lock or of a policy.
pub(crate) const MAX_LOCK: u64 = 2 * 1024 * 1024;
/// The most lines of a requirement file and the most declarations of a lock.
const MAX_ENTRIES: usize = 10_000;
/// How many requirement files may include one another in a row.
const MAX_INCLUDES: usize = 16;
const NPM: Fault = Fault("dependency_npm_lock");
const POLICY: Fault = Fault("dependency_license_policy");

/// One declaration: everything a lock says about a dependency it installs.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Dependency {
    /// `npm` or `pypi`.
    pub ecosystem: &'static str,
    /// The lock that declares it.
    pub manifest: String,
    /// Its name.
    pub name: String,
    /// Its version.
    pub version: String,
    /// The whole declaration in one spelling.
    pub identity: String,
    /// The integrity value or the SHA-256 of each artifact.
    pub hashes: BTreeSet<String>,
    /// The licence the lock itself declares, when it declares one.
    pub license: String,
    /// Where the lock says the artifact is, when it says so.
    pub source: String,
}

/// Reads a path of a tree, or answers that the tree has no such file.
pub type Load<'a> = &'a dyn Fn(&str) -> Result<Option<Vec<u8>>>;

/// Every installed path of the npm lock `content` at `path`: development,
/// optional and nested packages too, and never a link.
pub fn npm_dependencies(path: &str, content: &[u8]) -> Result<BTreeSet<Dependency>> {
    let document = record::strict(content).map_err(|_| NPM)?;
    let raw = object(&document).map_err(|_| NPM)?;
    require(
        raw.get("lockfileVersion") == Some(&json!(3)),
        "dependency_npm_lock_version",
    )?;
    let packages = raw.get("packages").and_then(Value::as_object).ok_or(NPM)?;
    require(packages.len() <= MAX_ENTRIES, "dependency_npm_count")?;
    let mut dependencies = BTreeSet::new();
    for (location, value) in packages {
        if location.is_empty() {
            continue;
        }
        let package = value.as_object().ok_or(NPM)?;
        let installed = location.rsplit_once("node_modules/");
        let linked = package.get("link").is_some_and(set);
        let (Some((_, below)), false) = (installed, linked) else {
            return Err(Fault("dependency_npm_path"));
        };
        let text = |name: &str, otherwise: &str| match package.get(name) {
            Some(value) => value.as_str().map(str::to_owned).ok_or(NPM),
            None => Ok(otherwise.to_owned()),
        };
        let mut identity = String::new();
        canonical(&json!([location, value]), &mut identity).map_err(|_| NPM)?;
        let version = package.get("version").and_then(Value::as_str).ok_or(NPM)?;
        dependencies.insert(Dependency {
            ecosystem: "npm",
            manifest: path.to_owned(),
            name: text("name", below)?,
            version: version.to_owned(),
            identity,
            hashes: BTreeSet::from([text("integrity", "")?]),
            license: text("license", "")?,
            source: text("resolved", "")?,
        });
    }
    Ok(dependencies)
}

/// `text` in lines, ended as the reference's `splitlines` ends them: by a
/// line feed, a carriage return or both, a vertical tab, a form feed, a
/// file, group or record separator, a next line, or a line or paragraph
/// separator.
fn splitlines(text: &str) -> Vec<&str> {
    const ENDS: [char; 10] = [
        '\n', '\r', '\u{b}', '\u{c}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}', '\u{2028}',
        '\u{2029}',
    ];
    let mut lines = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let Some(end) = rest.find(ENDS) else {
            lines.push(rest);
            break;
        };
        lines.push(&rest[..end]);
        let ended = &rest[end..];
        let width = if ended.starts_with("\r\n") {
            2
        } else {
            ended.chars().next().map_or(0, char::len_utf8)
        };
        rest = &ended[width..];
    }
    lines
}

/// The requirements and options of a requirement file: a backslash before a
/// line feed joins two lines, a number sign starts a comment, and what is
/// left of a line without its surrounding space is kept unless it is empty.
pub fn requirement_lines(content: &[u8]) -> Result<Vec<String>> {
    require(
        content.len() as u64 <= MAX_LOCK,
        "dependency_requirements_size",
    )?;
    let text = std::str::from_utf8(content).map_err(|_| Fault("dependency_requirements_text"))?;
    let joined = text.replace("\\\n", " ");
    let lines = splitlines(&joined);
    require(lines.len() <= MAX_ENTRIES, "dependency_requirements_count")?;
    let kept = lines.into_iter().filter_map(|line| {
        let line = line.split('#').next().unwrap_or("").trim_matches(space);
        (!line.is_empty()).then(|| line.to_owned())
    });
    Ok(kept.collect())
}

/// The path `relative` names from beside the file `path`, with no empty,
/// current-directory or resolvable parent step left in it.
fn beside(path: &str, relative: &str) -> String {
    let directory = path.rfind('/').map_or("", |cut| &path[..cut]);
    let joined = if relative.starts_with('/') || directory.is_empty() {
        relative.to_owned()
    } else {
        format!("{directory}/{relative}")
    };
    let mut steps: Vec<&str> = Vec::new();
    for step in joined.split('/').filter(|step| !matches!(*step, "" | ".")) {
        let climbs = step == ".." && steps.last().is_some_and(|last| *last != "..");
        if climbs {
            steps.pop();
        } else {
            steps.push(step);
        }
    }
    // An absolute path stays one, and so is refused as no path of the tree.
    let lead = if joined.starts_with('/') { "/" } else { "" };
    format!("{lead}{}", steps.join("/"))
}

/// The pins of the requirement file `path`, which `depth` files include one
/// after another, and of the files it includes. A lock that is not there
/// declares nothing; an include that is not there is refused, as is a longer
/// row of includes than the bound, which is where a file that includes
/// itself ends.
fn python_dependencies(
    path: &str,
    load: Load<'_>,
    depth: usize,
    found: &mut BTreeSet<Dependency>,
) -> Result<()> {
    require(
        depth < MAX_INCLUDES && safe_name(path),
        "dependency_requirements_include",
    )?;
    let Some(content) = load(path)? else {
        return require(depth == 0, "dependency_requirements_missing_include");
    };
    for line in requirement_lines(&content)? {
        if matches!(line.as_str(), "--require-hashes" | "--only-binary=:all:") {
            continue;
        }
        if let Some(included) = line.strip_prefix("-r ") {
            let included = beside(path, included.trim_matches(space));
            python_dependencies(&included, load, depth + 1, found)?;
        } else {
            let pin = requirement::pin(&line)?;
            found.insert(Dependency {
                ecosystem: "pypi",
                manifest: path.to_owned(),
                name: pin.name,
                version: pin.version,
                identity: pin.identity,
                hashes: pin.hashes,
                license: String::new(),
                source: String::new(),
            });
        }
    }
    Ok(())
}

/// Every declaration of the maintained locks of one tree.
pub fn dependencies(load: Load<'_>) -> Result<BTreeSet<Dependency>> {
    let mut found = BTreeSet::new();
    for path in NPM_LOCKS {
        if let Some(content) = load(path)? {
            found.extend(npm_dependencies(path, &content)?);
        }
    }
    for path in PYTHON_LOCKS {
        python_dependencies(path, load, 0, &mut found)?;
    }
    require(found.len() <= MAX_ENTRIES, "dependency_count")?;
    Ok(found)
}

/// The licences Cargo's own policy allows: the one allowlist there is.
pub fn allowed_licenses(snapshot: &Path) -> Result<BTreeSet<String>> {
    let content = files::bounded(&snapshot.join("deny.toml"), MAX_LOCK, false)?;
    let text = std::str::from_utf8(&content).map_err(|_| POLICY)?;
    let document: Value = toml::from_str(text).map_err(|_| POLICY)?;
    let policy = document.get("licenses").and_then(Value::as_object);
    let listed = policy
        .and_then(|policy| policy.get("allow"))
        .and_then(Value::as_array);
    let listed = listed.ok_or(POLICY)?;
    let mut allowed = BTreeSet::new();
    for licence in listed {
        allowed.insert(licence.as_str().ok_or(POLICY)?.to_owned());
    }
    require(
        !allowed.is_empty() && allowed.len() == listed.len(),
        POLICY.0,
    )?;
    Ok(allowed)
}

/// Every Python lock that is there installs wheels alone: the option is a
/// line of the lock, or, for the scanners' lock, of the command that
/// installs it.
pub fn require_wheel_only(snapshot: &Path) -> Result<()> {
    for path in PYTHON_LOCKS {
        let target = snapshot.join(path);
        if !target.exists() {
            continue;
        }
        if path == SCANNERS {
            let installer =
                files::bounded(&snapshot.join("build/check-security.sh"), MAX_LOCK, false)?;
            let installer = String::from_utf8(installer)
                .map_err(|_| Fault("dependency_security_wheel_only_policy"))?;
            let joined = installer.replace("\\\n", " ");
            let words: Vec<&str> = joined
                .split(space)
                .filter(|word| !word.is_empty())
                .collect();
            let command = words.join(" ");
            let wanted =
                "--require-hashes --only-binary=:all: --requirement security/requirements.txt";
            require(
                command.contains(wanted),
                "dependency_security_wheel_only_policy",
            )?;
        } else {
            let lines = requirement_lines(&files::bounded(&target, MAX_LOCK, false)?)?;
            let only = lines.iter().any(|line| line == "--only-binary=:all:");
            require(only, "dependency_python_wheel_only_policy")?;
        }
    }
    Ok(())
}

/// No lock of another tool, and no requirement file outside the maintained
/// ones, differs from the base: a new graph of dependencies needs the audits
/// to read it first. `names` are the files of both trees.
pub fn require_audited_locks(
    names: &BTreeSet<String>,
    current: Load<'_>,
    previous: Load<'_>,
    include_vendor: bool,
) -> Result<()> {
    for path in names {
        let maintained =
            NPM_LOCKS.contains(&path.as_str()) || PYTHON_LOCKS.contains(&path.as_str());
        // Installed requirement files are audited even below vendor/; unused
        // upstream development locks follow the optional source scan.
        if maintained || !scope::selected(path, include_vendor) {
            continue;
        }
        let name = path.rsplit('/').next().unwrap_or(path);
        let lock = OTHER_LOCKS.contains(&name)
            || (name.contains("requirements") && name.ends_with(".txt"))
            || (name.starts_with("pylock.") && name.ends_with(".toml"));
        if !lock {
            continue;
        }
        if let Some(content) = current(path)? {
            require(
                previous(path)?.as_ref() == Some(&content),
                "dependency_unaudited_lock_changed",
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
