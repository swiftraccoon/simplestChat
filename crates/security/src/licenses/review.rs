//! The licence review of what a change declares anew.
//!
//! The vulnerability audits read every dependency on every run. Licences
//! are reviewed where they can change: the declarations of the locks are
//! compared with those of an ancestor, and each new one must be allowed by
//! what its registry says of exactly the locked artifacts. What was found
//! is written to a receipt whether the review passes or not.

use super::{
    Bounds, Dependency, Evidence, MAX_LOCK, Registry, Reviewed, allowed_licenses, dependencies,
    npm_license, python_licenses, require_audited_locks, require_wheel_only,
};
use crate::{
    Fault,
    context::{Context, Outcome, Run},
    digest,
    fast::Programs,
    files,
    record::{pretty, safe_name},
    require,
    spdx::{Allowed, permits},
};
use serde_json::{Map, Value, json};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    path::Path,
    time::Instant,
};

/// The name of the review in the summary.
const NAME: &str = "changed-dependency-licenses";
/// The receipt, in the output directory.
const RECEIPT: &str = "dependency-licenses.json";

/// One wheel whose licence is not on the list and was reviewed for the one
/// lock that installs it: a tool of the controller that is never shipped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Controller<'a> {
    /// The lock that may declare it.
    pub manifest: &'a str,
    /// The canonical name of the package.
    pub name: &'a str,
    /// Its version.
    pub version: &'a str,
    /// The licence its metadata declares.
    pub license: &'a str,
    /// The SHA-256 of the wheel.
    pub wheel: &'a str,
    /// The SHA-256 of its metadata.
    pub metadata: &'a str,
}

/// The standalone linter of the Ansible controller.
pub const CONTROLLER: Controller<'static> = Controller {
    manifest: "ops/ansible/requirements.txt",
    name: "ansible-lint",
    version: "26.9.0",
    license: "GPL-3.0-or-later",
    wheel: "4d01903fca9c42f7ce2b366ea5503d0701a27b3c6fdf684276a72888eea57f50",
    metadata: "9297d04b256c09bb4b76edf974f5b537a720cc18dac9a2f0efc20bb4510d214b",
};

impl Controller<'_> {
    /// Whether `evidence` is this wheel, declared by this lock.
    pub(super) fn reviewed(&self, dependency: &Dependency, evidence: &Evidence) -> bool {
        let Evidence::Wheel {
            license,
            wheel,
            metadata,
            ..
        } = evidence
        else {
            return false;
        };
        dependency.ecosystem == "pypi"
            && dependency.manifest == self.manifest
            && dependency.name == self.name
            && dependency.version == self.version
            && license == self.license
            && wheel == self.wheel
            && metadata == self.metadata
    }
}

/// What one review is of and what it holds its findings to.
#[derive(Clone, Copy, Debug)]
pub struct Review<'a> {
    /// The copy of the sources whose locks are read.
    pub snapshot: &'a Path,
    /// The commit the change is compared with; the head when there is none.
    pub base: Option<&'a str>,
    /// Whether locks below `vendor/` are held to the audit rule too.
    pub include_vendor: bool,
    /// The hash-locked requirement files that are read beside the npm locks:
    /// the gate's own [`PYTHON_LOCKS`](super::PYTHON_LOCKS).
    pub python_locks: &'a [&'a str],
    /// The bounds of the registry session.
    pub bounds: Bounds,
    /// The licences reviewed for wheels that declare none as an expression.
    pub reviewed: &'a [Reviewed<'a>],
    /// The reviewed wheel of the controller.
    pub controller: Controller<'a>,
}

/// The commit the change is compared with, and the declarations that the
/// snapshot makes and that commit does not. A base that is no ancestor of
/// the head (an owner's force push, or a commit nobody has) leaves nothing
/// to compare: the review is skipped and the summary says so, and the next
/// ordinary push is compared with this head.
fn changed(
    context: &mut Context,
    programs: &dyn Programs,
    review: &Review<'_>,
) -> Outcome<(String, BTreeSet<Dependency>)> {
    let selected = review
        .base
        .filter(|base| !base.is_empty())
        .unwrap_or("HEAD");
    require(
        selected == "HEAD" || crate::commit(selected),
        "dependency_diff_base",
    )?;
    let git = programs.prerequisite("git")?;
    let asked = |words: &[&str]| -> Vec<OsString> {
        let mut argv = vec![git.as_os_str().to_owned()];
        argv.extend(words.iter().map(Into::into));
        argv
    };
    let commit = format!("{selected}^{{commit}}");
    let how = Run {
        accepted: &[0, 128],
        ..Run::default()
    };
    let verified = asked(&["rev-parse", "--verify", &commit]);
    let (status, revision) = context.run("dependency-base", &verified, &how)?;
    let revision = String::from_utf8(revision).map_err(|_| Fault("dependency_diff_base"))?;
    let resolved = if status == 0 { revision.trim() } else { "" };
    let mut ancestor = 1;
    if !resolved.is_empty() {
        let how = Run {
            accepted: &[0, 1, 128],
            ..Run::default()
        };
        let related = asked(&["merge-base", "--is-ancestor", resolved, "HEAD"]);
        ancestor = context.run("dependency-base-ancestor", &related, &how)?.0;
    }
    if ancestor != 0 {
        context.annotate([("rangeScan", "skipped_base_not_an_ancestor".into())]);
        return Ok((selected.to_owned(), BTreeSet::new()));
    }
    let listed = |listing: Vec<u8>, code: &'static str| -> Outcome<BTreeSet<String>> {
        let listing = String::from_utf8(listing).map_err(|_| Fault(code))?;
        let names = listing.trim_end_matches('\0').split('\0');
        Ok(names.map(str::to_owned).collect())
    };
    let tree = asked(&["ls-tree", "-rz", "--name-only", resolved]);
    let before = context
        .run("dependency-base-files", &tree, &Run::default())?
        .1;
    let before = listed(before, "dependency_base_path")?;
    let files = asked(&[
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
    ]);
    let now = context
        .run("dependency-current-files", &files, &Run::default())?
        .1;
    let mut names = listed(now, "dependency_current_path")?;
    names.extend(before.iter().cloned());
    let shared = RefCell::new(context);
    let shown: RefCell<BTreeMap<String, Option<Vec<u8>>>> = RefCell::default();
    let previous = |path: &str| -> Outcome<Option<Vec<u8>>> {
        require(safe_name(path), "dependency_base_path")?;
        if let Some(content) = shown.borrow().get(path) {
            return Ok(content.clone());
        }
        let content = if before.contains(path) {
            let show = asked(&["show", &format!("{resolved}:{path}")]);
            let mut context = shared.borrow_mut();
            Some(
                context
                    .run("dependency-base-lock", &show, &Run::default())?
                    .1,
            )
        } else {
            None
        };
        shown.borrow_mut().insert(path.to_owned(), content.clone());
        Ok(content)
    };
    let current = |path: &str| -> Outcome<Option<Vec<u8>>> {
        require(safe_name(path), "dependency_current_path")?;
        let target = review.snapshot.join(path);
        if !target.exists() {
            return Ok(None);
        }
        Ok(Some(files::bounded(&target, MAX_LOCK, false)?))
    };
    let locks = review.python_locks;
    require_audited_locks(&names, &current, &previous, review.include_vendor, locks)?;
    let declared = dependencies(&current, locks)?;
    let known = dependencies(&previous, locks)?;
    let new = declared.difference(&known).cloned().collect();
    Ok((resolved.to_owned(), new))
}

/// Judge every new declaration and answer how many there were and how many
/// fetches the registries were asked. Each finding is added to `receipt`
/// before it is held to the list.
fn judged(
    context: &mut Context,
    programs: &dyn Programs,
    review: &Review<'_>,
    receipt: &mut Map<String, Value>,
) -> Outcome<(usize, usize)> {
    let allowed = allowed_licenses(review.snapshot)?;
    require_wheel_only(review.snapshot, review.python_locks)?;
    let (resolved, new) = changed(context, programs, review)?;
    receipt.insert("baseRevision".to_owned(), resolved.into());
    receipt.insert("allowedLicenses".to_owned(), json!(allowed));
    let list = Allowed::new(allowed.iter().map(String::as_str));
    let began = Instant::now();
    let since = || began.elapsed();
    let mut registry = Registry::new(programs, &since, review.bounds);
    let mut ordered: Vec<&Dependency> = new.iter().collect();
    ordered.sort_by_key(|item| (item.ecosystem, &item.manifest, &item.name, &item.identity));
    for dependency in ordered {
        let evidence = if dependency.ecosystem == "npm" {
            npm_license(context, &mut registry, dependency)?
        } else {
            python_licenses(context, &mut registry, dependency, review.reviewed)?
        };
        let found: Vec<Value> = evidence.iter().map(Evidence::record).collect();
        let record = json!({
            "ecosystem": dependency.ecosystem,
            "manifest": dependency.manifest,
            "name": dependency.name,
            "version": dependency.version,
            "declarationSha256": digest(dependency.identity.as_bytes()),
            "evidence": found,
        });
        if let Some(Value::Array(records)) = receipt.get_mut("dependencies") {
            records.push(record);
        }
        for item in &evidence {
            // Nothing is installed from a source distribution.
            let license = match item {
                Evidence::Sdist { .. } => continue,
                Evidence::Npm { license, .. } | Evidence::Wheel { license, .. } => license,
            };
            require(
                permits(license, &list)? || review.controller.reviewed(dependency, item),
                "dependency_license_not_allowed",
            )?;
        }
    }
    Ok((new.len(), registry.requests()))
}

/// Review the licences of what the snapshot declares and its base does not.
/// The receipt is written and the summary line is completed however the
/// review ends.
pub fn check(context: &mut Context, programs: &dyn Programs, review: &Review<'_>) -> Outcome<()> {
    let started = Instant::now();
    let line = context.begin(vec![("name", NAME.into()), ("exitStatus", 1.into())]);
    let mut receipt = Map::new();
    receipt.insert("schemaVersion".to_owned(), 1.into());
    receipt.insert("passed".to_owned(), false.into());
    receipt.insert("dependencies".to_owned(), json!([]));
    let outcome = judged(context, programs, review, &mut receipt);
    if let Ok((new, requests)) = outcome {
        receipt.insert("passed".to_owned(), true.into());
        context.complete(
            line,
            [
                ("exitStatus", 0.into()),
                ("changedDependencies", new.into()),
                ("registryRequests", requests.into()),
            ],
        );
    }
    let written = pretty(&Value::Object(receipt));
    files::write_private(&context.output().join(RECEIPT), written.as_bytes(), 0o600)?;
    let elapsed = (started.elapsed().as_secs_f64() * 1000.0).round() / 1000.0;
    context.complete(line, [("elapsedSeconds", elapsed.into())]);
    outcome.map(|_| ())
}
